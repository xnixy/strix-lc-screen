//! Saved options and the sign-in startup entry.

use crate::layout::{self, Background, Layout};
use crate::stats::Metric;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

#[cfg(windows)]
const RUN_VALUE: &str = "strix-lc-screen";

/// Fields missing from an older settings file take their default value.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub close_to_tray: bool,
    pub start_with_windows: bool,
    /// Sign-in launches the app with `--tray`, so the window stays hidden.
    pub start_in_tray: bool,
    pub resume_last: bool,
    pub last_path: Option<String>,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
    pub speed: f32,
    /// Put the pump screen to sleep while this computer is locked.
    pub sleep_when_locked: bool,
    /// What the pump shows when the app starts.
    pub source: Source,
    /// Readings picked in 0.2.x. Only read, to lay out the widgets the first
    /// time. Older builds stored firmware overlay names in `overlay_items`.
    #[serde(skip_serializing)]
    pub stats_items: Vec<Metric>,
    /// Draw the widgets over the photo or video.
    pub stats_on_picture: bool,
    /// "top" or "bottom" in 0.2.x. Only read, like `stats_items`.
    #[serde(skip_serializing)]
    pub stats_position: String,
    /// The stats dashboard. Laid out from `stats_items` when missing.
    pub dashboard_layout: Option<Layout>,
    /// Widgets over the photo or video. Laid out from `stats_items` when missing.
    pub picture_layout: Option<Layout>,
    /// Temperatures in °F instead of °C.
    pub fahrenheit: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The chosen photo or video.
    #[default]
    Picture,
    /// The stats dashboard.
    Dashboard,
    /// The pump's own animation. Nothing is sent.
    Animation,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            start_with_windows: false,
            start_in_tray: true,
            resume_last: true,
            last_path: None,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            speed: 1.0,
            sleep_when_locked: false,
            source: Source::Picture,
            stats_items: vec![
                Metric::Time,
                Metric::Date,
                Metric::CpuUsage,
                Metric::CpuClock,
                Metric::GpuUsage,
                Metric::GpuTemp,
                Metric::MemoryUsage,
                Metric::NetDown,
            ],
            stats_on_picture: false,
            stats_position: String::new(),
            dashboard_layout: None,
            picture_layout: None,
            fahrenheit: false,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        let mut settings: Self = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        settings.fill_layouts();
        settings
    }

    /// Lay out the readings picked before widgets could be moved, the way they were drawn.
    pub fn fill_layouts(&mut self) {
        if self.dashboard_layout.is_none() {
            self.dashboard_layout = Some(layout::arrange_dashboard(
                &self.stats_items,
                Background::default(),
            ));
        }
        if self.picture_layout.is_none() {
            let at_top = self.stats_position == "top";
            self.picture_layout = Some(layout::arrange_band(&self.stats_items, at_top));
        }
    }

    pub fn dashboard(&mut self) -> &mut Layout {
        self.dashboard_layout.get_or_insert_with(Layout::default)
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(&path, bytes).with_context(|| format!("could not write {}", path.display()))
    }
}

pub fn settings_path() -> PathBuf {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        if !appdata.is_empty() {
            return PathBuf::from(appdata)
                .join("strix-lc-screen")
                .join("settings.json");
        }
    }
    config_dir().join("strix-lc-screen").join("settings.json")
}

fn config_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".config");
        }
    }
    std::env::temp_dir()
}

pub fn set_run_at_logon(enable: bool, hidden: bool) -> Result<()> {
    #[cfg(windows)]
    {
        set_run_at_logon_windows(enable, hidden)
    }
    #[cfg(target_os = "linux")]
    {
        set_run_at_logon_linux(enable, hidden)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (enable, hidden);
        anyhow::bail!("starting at sign-in is only available on Windows and Linux");
    }
}

#[cfg(windows)]
fn set_run_at_logon_windows(enable: bool, hidden: bool) -> Result<()> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = hkcu
        .create_subkey_with_flags(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            KEY_SET_VALUE,
        )
        .context("could not open the Windows startup list")?;
    if enable {
        let exe = std::env::current_exe().context("could not find this program's path")?;
        run.set_value(RUN_VALUE, &startup_command(&exe, hidden))
            .context("could not add this program to Windows startup")?;
    } else if let Err(error) = run.delete_value(RUN_VALUE) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error).context("could not remove this program from Windows startup");
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn set_run_at_logon_linux(enable: bool, hidden: bool) -> Result<()> {
    let path = config_dir()
        .join("autostart")
        .join("strix-lc-screen.desktop");
    if !enable {
        match std::fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("could not remove {}", path.display()));
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let exe = std::env::current_exe().context("could not find this program's path")?;
    std::fs::write(&path, autostart_desktop(&exe, hidden))
        .with_context(|| format!("could not write {}", path.display()))
}

fn startup_command(exe: &Path, hidden: bool) -> String {
    let path = exe.display().to_string();
    let quoted = format!("\"{path}\"");
    if hidden {
        format!("{quoted} --tray")
    } else {
        quoted
    }
}

#[cfg(any(target_os = "linux", test))]
fn autostart_desktop(exe: &Path, hidden: bool) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Pump screen\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        startup_command(exe, hidden)
    )
}

static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);
static SHOW_HOOK: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Wakes the window after a second launch asks the running copy to show itself.
pub fn on_show_request(hook: impl Fn() + Send + Sync + 'static) {
    let _ = SHOW_HOOK.set(Box::new(hook));
}

pub fn take_show_request() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::SeqCst)
}

#[cfg(target_os = "linux")]
fn notify_show() {
    SHOW_REQUESTED.store(true, Ordering::SeqCst);
    if let Some(hook) = SHOW_HOOK.get() {
        hook();
    }
}

/// Keeps a single window. A second launch shows the one already running.
pub fn claim_single_instance() -> bool {
    #[cfg(windows)]
    {
        claim_single_instance_windows()
    }
    #[cfg(target_os = "linux")]
    {
        claim_named("strix-lc-screen")
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        true
    }
}

#[cfg(windows)]
fn claim_single_instance_windows() -> bool {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn CreateMutexW(attrs: *mut c_void, initial_owner: i32, name: *const u16) -> *mut c_void;
        fn GetLastError() -> u32;
        fn FindWindowW(class: *const u16, window: *const u16) -> *mut c_void;
    }
    const ERROR_ALREADY_EXISTS: u32 = 183;
    let name: Vec<u16> = "Local\\strix-lc-screen"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let handle = CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
        if handle.is_null() {
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let title: Vec<u16> = "Pump screen"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
            if !hwnd.is_null() {
                crate::gui::reveal_pump_window();
            }
            return false;
        }
    }
    true
}

#[cfg(target_os = "linux")]
fn claim_named(name: &str) -> bool {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
    let Ok(addr) = SocketAddr::from_abstract_name(name) else {
        return true;
    };
    if UnixStream::connect_addr(&addr).is_ok() {
        return false;
    }
    let listener = match UnixListener::bind_addr(&addr) {
        Ok(listener) => listener,
        Err(_) => return UnixStream::connect_addr(&addr).is_err(),
    };
    std::thread::spawn(move || {
        for _connection in listener.incoming().flatten() {
            notify_show();
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_settings_file_keeps_its_values() {
        let old = r#"{"close_to_tray":false,"start_with_windows":true,"start_in_tray":true,
            "resume_last":true,"last_path":"C:\\clip.mp4","zoom":2.0,"pan_x":0.5,"pan_y":0.0,"speed":1.5}"#;
        let settings: Settings = serde_json::from_str(old).unwrap();
        assert!(!settings.close_to_tray);
        assert_eq!(settings.speed, 1.5);
        assert!(!settings.sleep_when_locked);
        assert_eq!(settings.source, Source::Picture);
    }

    #[test]
    fn fields_from_the_firmware_overlay_build_are_ignored() {
        let previous = r#"{"speed":1.25,"built_in_clip":true,"overlay_items":["CPU Usage"],
            "sleep_when_locked":true}"#;
        let settings: Settings = serde_json::from_str(previous).unwrap();
        assert_eq!(settings.speed, 1.25);
        assert!(settings.sleep_when_locked);
        assert!(!settings.stats_items.is_empty());
    }

    #[test]
    fn readings_picked_in_0_2_become_widgets_where_they_were() {
        let previous = r#"{"stats_items":["cpu_usage","gpu_temp","time"],
            "stats_on_picture":true,"stats_position":"top"}"#;
        let mut settings: Settings = serde_json::from_str(previous).unwrap();
        settings.fill_layouts();
        let band = &settings.picture_layout.as_ref().unwrap().widgets;
        assert_eq!(band.len(), 3);
        assert!(
            band.iter().all(|widget| widget.y < 360.0),
            "band stays on top"
        );
        let tiles = &settings.dashboard_layout.as_ref().unwrap().widgets;
        assert_eq!(tiles.len(), 3);
        // Saving drops the old fields, so the layouts win from then on.
        let saved = serde_json::to_string(&settings).unwrap();
        assert!(!saved.contains("stats_items"));
        assert!(saved.contains("dashboard_layout"));
    }

    #[test]
    fn hidden_startup_adds_the_tray_flag() {
        let command = startup_command(Path::new(r"C:\Pump\strix-lc-screen.exe"), true);
        assert_eq!(command, r#""C:\Pump\strix-lc-screen.exe" --tray"#);
    }

    #[test]
    fn autostart_entry_quotes_the_program_and_adds_tray() {
        let text = autostart_desktop(Path::new("/home/nick/pump screen/strix-lc-screen"), true);
        assert!(text.contains("Exec=\"/home/nick/pump screen/strix-lc-screen\" --tray\n"));
        assert!(text.contains("X-GNOME-Autostart-enabled=true\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn second_launch_asks_the_first_to_show() {
        let name = format!("strix-lc-screen-test-{}", std::process::id());
        assert!(claim_named(&name));
        assert!(!claim_named(&name));
        let start = std::time::Instant::now();
        while !take_show_request() {
            if start.elapsed() > std::time::Duration::from_secs(2) {
                panic!("the running copy was not asked to show its window");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
