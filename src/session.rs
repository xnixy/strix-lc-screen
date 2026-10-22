//! One HID session with the pump screen.

use crate::frame::{self, FrameError};
use crate::msg::{self, Response};
use anyhow::{anyhow, bail, Context, Result};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VENDOR: u16 = 0x0b05;
const PRODUCT: u16 = 0x1de7;
const USAGE_PAGE: u16 = 0xff00;
const USAGE: u16 = 0x0001;

pub struct Panel {
    device: HidDevice,
    seq: u32,
    /// Reused 1025-byte picture report. Filling this beats allocating one per slice.
    report: Vec<u8>,
    /// Result of the first `conn`. A later handshake makes the panel drop the
    /// live picture and go back to its built-in clip.
    identity: Option<serde_json::Value>,
    /// A USB read or write failed. The handle may be dead, so the caller should open a new one.
    io_failed: bool,
    last_write: Instant,
    /// The live picture channel was closed. The screen refuses to open it
    /// again until the USB link has been quiet for `RESET_QUIET`.
    needs_reset: bool,
}

/// A healthy 720×720 frame takes about 45 ms. When the screen has dropped the
/// live picture channel it stops taking frames, and one frame takes about 2 s.
/// Commands sent meanwhile get no reply, so give up on the frame early.
const FRAME_STALL: Duration = Duration::from_secs(1);

/// Once the live channel closes, the screen refuses to open it again until its
/// 5-second USB idle watchdog has reset the link. Measured: 3 s of silence is
/// not enough and 7 s always is.
const RESET_QUIET: Duration = Duration::from_secs(6);

/// How long a command waits for its reply.
const REPLY_WAIT: Duration = Duration::from_secs(2);
/// Data bytes in one 1025-byte report, for pictures and files alike.
const CHUNK: usize = 1000;

impl Panel {
    pub fn open() -> Result<Self> {
        let api = HidApi::new().context("HID library failed to start")?;
        let info = find_screen(&api)?;
        let device = info.open_device(&api).context(open_screen_hint())?;
        device
            .set_blocking_mode(false)
            .context("could not set the screen read mode")?;
        Ok(Self {
            device,
            seq: 0,
            report: vec![0u8; 1025],
            identity: None,
            io_failed: false,
            last_write: Instant::now(),
            needs_reset: false,
        })
    }

    pub fn io_failed(&self) -> bool {
        self.io_failed
    }

    /// Stay off the USB link until the screen's idle watchdog has reset it.
    /// Returns false if `cancel` asked to stop first.
    fn wait_for_reset(&self, cancel: &dyn Fn() -> bool) -> bool {
        loop {
            if cancel() {
                return false;
            }
            let quiet = self.last_write.elapsed();
            if quiet >= RESET_QUIET {
                return true;
            }
            std::thread::sleep((RESET_QUIET - quiet).min(Duration::from_millis(100)));
        }
    }

    pub fn connect(&mut self) -> Result<serde_json::Value> {
        let response = self.call("conn", None)?;
        expect_ok(&response, "conn")?;
        let identity = response.json().context("connect response was not JSON")?;
        self.identity = Some(identity.clone());
        Ok(identity)
    }

    /// Serial and firmware from the handshake. Does not send `conn` again.
    pub fn identity(&mut self) -> Result<serde_json::Value> {
        if let Some(identity) = &self.identity {
            return Ok(identity.clone());
        }
        self.connect()
    }

    pub fn state(&mut self) -> Result<serde_json::Value> {
        let response = self.call("devStateGet", None)?;
        expect_ok(&response, "devStateGet")?;
        response.json().context("state response was not JSON")
    }

    pub fn power(&mut self, event: &str) -> Result<()> {
        let body = serde_json::json!({ "event": event }).to_string();
        let response = self.call("power", Some(body.as_bytes()))?;
        if event == "suspend" {
            self.needs_reset = true;
        }
        expect_ok(&response, "power")
    }

    pub fn brightness(&mut self, value: u8) -> Result<()> {
        let body = serde_json::json!({ "value": value }).to_string();
        let response = self.call("brightness", Some(body.as_bytes()))?;
        expect_ok(&response, "brightness")
    }

    pub fn rotate(&mut self, degrees: u16) -> Result<()> {
        let body = serde_json::json!({ "degree": degrees }).to_string();
        let response = self.call("rotate", Some(body.as_bytes()))?;
        expect_ok(&response, "rotate")
    }

    /// Store one screen option on the pump. Only the commands in `Setting`
    /// can be sent this way. Firmware upgrade, recovery, factory test, and the
    /// USB id and serial writers are deliberately not reachable from here.
    pub fn apply(&mut self, setting: &Setting) -> Result<()> {
        let (cmd, body) = setting.request();
        let response = self.call(cmd, Some(body.to_string().as_bytes()))?;
        if *setting == Setting::ScreenOn(false) {
            self.needs_reset = true;
        }
        expect_ok(&response, cmd)
    }

    /// Wake the panel, turn on the live picture channel, and push one JPEG.
    /// The panel drops that picture about a second after frames stop, and goes
    /// back to its built-in clip. Callers that want a still to remain have to
    /// keep sending it.
    pub fn show_jpeg(&mut self, jpeg: &[u8], cancel: &dyn Fn() -> bool) -> Result<()> {
        if jpeg.is_empty() {
            bail!("picture is empty");
        }
        if cancel() {
            return Ok(());
        }
        if self.needs_reset && !self.wait_for_reset(cancel) {
            return Ok(());
        }
        let _ = self.power("resume");
        let enable = br#"{"enable":true}"#;
        let response = self.call("realtimeDisplay", Some(enable))?;
        if response.code != 200 {
            // Also refused while the screen is switched off.
            self.needs_reset = true;
            bail!("the screen refused the picture channel");
        }
        self.needs_reset = false;
        self.transport_jpeg(jpeg, cancel)
    }

    /// Send a firmware package and have the pump install it. This repeats what
    /// the ASUS app did in a logged V1.0.5 to V1.0.6 update on this model:
    ///
    /// 1. Frames stop, and about a second passes.
    /// 2. `transport` names the file. The pump answers 200 and from then on
    ///    treats every report as file data, not commands.
    /// 3. All blocks go back to back: 0x5C reports with the block count and
    ///    index, flag 0, and up to 1000 bytes each. Only the last is answered,
    ///    with AckNumber 0.
    /// 4. `transported` closes the file. The pump unpacks it and restarts
    ///    about ten seconds later, so this handle stops working.
    ///
    /// Nothing is retried. A stopped transfer is dropped by the pump, which
    /// installs nothing until `transported` arrives.
    pub fn install_firmware(&mut self, package: &[u8]) -> Result<()> {
        const QUIET: Duration = Duration::from_millis(1500);
        if package.is_empty() {
            bail!("the firmware package is empty");
        }
        let blocks = package.len().div_ceil(CHUNK);
        let Ok(blocks) = u16::try_from(blocks) else {
            bail!("the firmware package is too large");
        };
        while self.last_write.elapsed() < QUIET {
            std::thread::sleep(QUIET - self.last_write.elapsed());
        }
        // Key order and file name as the ASUS app sends them.
        let body = format!(
            r#"{{"type":"firmware","fileSize":{},"fileName":"update.zip"}}"#,
            package.len()
        );
        let response =
            self.call_within("transport", Some(body.as_bytes()), Duration::from_secs(10))?;
        expect_ok(&response, "transport")?;
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| (elapsed.as_secs() & 0xff) as u8)
            .unwrap_or(1);
        for (index, chunk) in package.chunks(CHUNK).enumerate() {
            fill_block(&mut self.report, id, blocks, index as u16, 0, chunk);
            let wrote = match self.device.write(&self.report) {
                Ok(wrote) => wrote,
                Err(error) => {
                    self.io_failed = true;
                    return Err(error).context(format!(
                        "the firmware transfer stopped at block {} of {blocks}. Nothing was installed",
                        index + 1
                    ));
                }
            };
            self.last_write = Instant::now();
            if wrote < self.report.len() {
                self.io_failed = true;
                bail!(
                    "the firmware transfer stopped at block {} of {blocks}. Nothing was installed",
                    index + 1
                );
            }
        }
        // The pump numbers this one reply 0.
        let response = self.read_reply("transport", "0", Duration::from_secs(10))?;
        expect_ok(&response, "transport")?;
        let response = self.call_within(
            "transported",
            // The pump stores the md5 without checking it. The ASUS app sends "todo".
            Some(br#"{"md5":"todo","fileName":"update.zip"}"#),
            Duration::from_secs(6),
        )?;
        expect_ok(&response, "transported")?;
        // The pump is about to restart. The next request opens a new handle.
        self.io_failed = true;
        Ok(())
    }

    /// Push the next video frame without repeating the wake and channel setup.
    pub fn push_jpeg(&mut self, jpeg: &[u8], cancel: &dyn Fn() -> bool) -> Result<()> {
        self.transport_jpeg(jpeg, cancel)
    }

    fn transport_jpeg(&mut self, jpeg: &[u8], cancel: &dyn Fn() -> bool) -> Result<()> {
        // One 1025-byte report per slice: report id 0x00, tag 0x5C, big-endian
        // value length, a 21-byte header, then up to 1000 JPEG bytes.
        // Header: id, block count, zero-based index, flag 1, then 15 zero bytes.
        let blocks = jpeg.len().div_ceil(CHUNK).max(1) as u16;
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| (elapsed.as_secs() & 0xff) as u8)
            .unwrap_or(1);
        let mut offset = 0usize;
        let mut index = 0u16;
        let started = Instant::now();
        while offset < jpeg.len() {
            if cancel() {
                return Ok(());
            }
            if started.elapsed() > FRAME_STALL {
                self.needs_reset = true;
                bail!("the pump stopped taking the picture");
            }
            let end = (offset + CHUNK).min(jpeg.len());
            let chunk = &jpeg[offset..end];
            fill_block(&mut self.report, id, blocks, index, 1, chunk);
            let wrote = match self.device.write(&self.report) {
                Ok(wrote) => wrote,
                Err(error) => {
                    self.io_failed = true;
                    return Err(error).context("could not write the picture to the pump");
                }
            };
            self.last_write = Instant::now();
            if wrote == 0 {
                bail!("the pump did not accept the picture");
            }
            offset = end;
            index = index.saturating_add(1);
        }
        Ok(())
    }

    fn call(&mut self, cmd: &str, body: Option<&[u8]>) -> Result<Response> {
        self.call_within(cmd, body, REPLY_WAIT)
    }

    fn call_within(
        &mut self,
        cmd: &str,
        body: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Response> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        let date = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the unix epoch")?
            .as_millis();
        let payload = msg::request(cmd, seq, date, body);
        self.exchange(cmd, seq, &payload, timeout)
    }

    fn exchange(
        &mut self,
        cmd: &str,
        seq: u32,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Response> {
        let wire = frame::encode(payload);
        let mut report = Vec::with_capacity(wire.len() + 1);
        report.push(0x00);
        report.extend_from_slice(&wire);
        let wrote = match self.device.write(&report) {
            Ok(wrote) => wrote,
            Err(error) => {
                self.io_failed = true;
                return Err(error).with_context(|| format!("HID write failed for {cmd}"));
            }
        };
        self.last_write = Instant::now();
        if wrote < report.len() {
            bail!("{cmd} wrote {wrote} of {} bytes", report.len());
        }
        let expected_ack = seq.wrapping_add(1).to_string();
        self.read_reply(cmd, &expected_ack, timeout)
    }

    /// Wait for the reply whose AckNumber is `expected_ack`.
    fn read_reply(&mut self, cmd: &str, expected_ack: &str, timeout: Duration) -> Result<Response> {
        let deadline = Instant::now() + timeout;
        let mut pending = Vec::new();
        while Instant::now() < deadline {
            let mut buf = vec![0u8; 64 * 1024];
            let read = match self.device.read_timeout(&mut buf, 200) {
                Ok(read) => read,
                Err(error) => {
                    self.io_failed = true;
                    return Err(error).with_context(|| format!("HID read failed for {cmd}"));
                }
            };
            if read == 0 {
                continue;
            }
            pending.extend_from_slice(&buf[..read]);
            if pending.first() == Some(&0x00) && pending.get(1) == Some(&0x5a) {
                pending.remove(0);
            }
            match frame::decode(&pending) {
                Ok((frame, used)) => {
                    pending.drain(..used);
                    let response = msg::parse_response(&frame.payload)
                        .with_context(|| format!("{cmd} response was not a text message"))?;
                    if response.header("AckNumber") != Some(expected_ack) {
                        continue;
                    }
                    return Ok(response);
                }
                Err(FrameError::Truncated | FrameError::BadEscape) => continue,
                Err(error) => return Err(error).context(format!("{cmd} returned a bad frame")),
            }
        }
        bail!(
            "{cmd} got no matching reply within {} seconds",
            timeout.as_secs()
        )
    }
}

/// Options the firmware keeps between power cycles. Names and bodies come from
/// the command table in the screen firmware (V1.0.6) and the ASUS app.
///
/// Left out on purpose:
/// - `timeout` is the firmware's USB watchdog in seconds, not a screen timer.
///   A small value makes it reset the USB link.
/// - `presetThemeId` switches the whole UI theme. Every firmware asset has a
///   `_0` and `_1` copy, and theme 1 left a pump screen blank.
/// - `sysinfoDisplay` was accepted but drew nothing on a pump screen.
/// - `logoId` and `suspendId` have values that could not be put back.
#[derive(Clone, Debug, PartialEq)]
pub enum Setting {
    /// The ASUS app's screen switch. Off turns the panel off, and it stays off
    /// through power events until it is turned on again.
    ScreenOn(bool),
    /// The ASUS app's "Standby mode". On shows the sleep clip while the computer
    /// sleeps. Off turns the panel off instead.
    DisplayInSleep(bool),
}

impl Setting {
    fn request(&self) -> (&'static str, serde_json::Value) {
        use serde_json::json;
        match self {
            Setting::ScreenOn(on) => ("waterBlockScreen", json!({ "enable": on })),
            Setting::DisplayInSleep(on) => ("displayInSleep", json!({ "enable": on })),
        }
    }
}

fn open_screen_hint() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "could not open the pump screen. Install linux/60-strix-lc-screen.rules, reload udev, replug the cooler, and try again"
    }
    #[cfg(not(target_os = "linux"))]
    {
        "could not open the pump screen. Quit \"ROG STRIX LC & SLC IV Series\" and try again"
    }
}

fn find_screen(api: &HidApi) -> Result<DeviceInfo> {
    let mut found = Vec::new();
    for device in api.device_list() {
        let vendor_ok = device.vendor_id() == VENDOR;
        let product_ok = device.product_id() == PRODUCT;
        let usage_ok = device.usage_page() == USAGE_PAGE && device.usage() == USAGE;
        if vendor_ok && product_ok && usage_ok {
            found.push(device.clone());
        }
    }
    found.sort_by_key(|device| device.interface_number());
    found
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no ROG Strix LC IV screen found (USB 0B05:1DE7 interface 0)"))
}

/// One 1025-byte report: report id 0x00, tag 0x5C, big-endian value length,
/// a 21-byte header (id, block count, zero-based index, flag, then zeros), and
/// up to 1000 data bytes. Live frames use flag 1 and files flag 0.
fn fill_block(report: &mut Vec<u8>, id: u8, blocks: u16, index: u16, flag: u8, data: &[u8]) {
    report.resize(1025, 0);
    report.fill(0);
    report[1] = 0x5c;
    report[2..4].copy_from_slice(&((21 + data.len()) as u16).to_be_bytes());
    report[4] = id;
    report[5..7].copy_from_slice(&blocks.to_be_bytes());
    report[7..9].copy_from_slice(&index.to_be_bytes());
    report[9] = flag;
    report[25..25 + data.len()].copy_from_slice(data);
}

fn expect_ok(response: &Response, cmd: &str) -> Result<()> {
    if response.code == 200 {
        return Ok(());
    }
    let body = String::from_utf8_lossy(&response.body);
    bail!("{cmd} was rejected with status {}: {body}", response.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_blocks_match_the_logged_asus_update() {
        // The last of 612 blocks of the 611,852-byte V1.0.6 package, as the
        // ASUS app logged it: 5c 0369 21 0264 0263 00 ..., then the data.
        let data = [0xffu8, 0x61, 0x86, 0x56];
        let mut report = Vec::new();
        let tail = vec![0u8; 611_852 - 611 * CHUNK];
        fill_block(&mut report, 0x21, 612, 611, 0, &tail);
        assert_eq!(report.len(), 1025);
        assert_eq!(
            &report[..10],
            &[0x00, 0x5c, 0x03, 0x69, 0x21, 0x02, 0x64, 0x02, 0x63, 0x00]
        );
        assert!(report[10..25].iter().all(|byte| *byte == 0));
        fill_block(&mut report, 0x21, 612, 611, 0, &data);
        assert_eq!(&report[25..29], &data);
        assert!(report[29..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn settings_use_the_firmware_command_names_and_keys() {
        let cases = [
            (
                Setting::ScreenOn(false),
                "waterBlockScreen",
                r#"{"enable":false}"#,
            ),
            (
                Setting::DisplayInSleep(true),
                "displayInSleep",
                r#"{"enable":true}"#,
            ),
        ];
        for (setting, cmd, body) in cases {
            let (sent_cmd, sent_body) = setting.request();
            assert_eq!(sent_cmd, cmd);
            assert_eq!(sent_body.to_string(), body);
        }
    }
}
