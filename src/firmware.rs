//! Checks a firmware package before it goes to the pump.
//!
//! ASUS packages are zip files holding `update.img` and a `build.prop` that
//! names the device. The ASUS app only compares the product name. This also
//! insists on the official layout, both files stored uncompressed, matching
//! checksums, and a version that is not older than the pump's, so a damaged
//! download or a package for another model never reaches the pump.

use anyhow::{bail, Context, Result};
use std::path::Path;

/// `ro.product.system.name` for the LC IV 360 (USB 0B05:1DE7), from the ASUS
/// app's model table. The SLC IV and the white models use other codes.
pub const PROJECT_CODE: &str = "C09P06S0508";
/// `ro.product.system.device` in V1.0.6.
const DEVICE: &str = "WC07";
/// V1.0.6 is about 600 KB. Anything far larger is not a pump package.
const MAX_SIZE: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Package {
    /// Such as "V1.0.6".
    pub version: String,
    /// The zip file, sent to the pump as is.
    pub bytes: Vec<u8>,
}

pub fn read(path: &Path) -> Result<Package> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    check(bytes)
}

pub fn check(bytes: Vec<u8>) -> Result<Package> {
    if bytes.len() > MAX_SIZE {
        bail!("this file is too large to be pump firmware");
    }
    let entries = entries(&bytes).context("this is not an ASUS firmware package")?;
    let mut names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    names.sort_unstable();
    if names != ["build.prop", "update.img"] {
        bail!(
            "this is not an ASUS pump firmware package. It should hold build.prop and update.img"
        );
    }
    for entry in &entries {
        let data = stored_data(&bytes, entry)?;
        if crc32(data) != entry.crc {
            bail!(
                "{} in the package is damaged. Download it again",
                entry.name
            );
        }
    }
    let prop = entries
        .iter()
        .find(|entry| entry.name == "build.prop")
        .expect("checked above");
    let prop = String::from_utf8_lossy(stored_data(&bytes, prop)?).into_owned();
    let value = |key: &str| {
        prop.lines()
            .filter_map(|line| line.split_once('='))
            .find(|(name, _)| name.trim() == key)
            .map(|(_, value)| value.trim().to_string())
    };
    let product = value("ro.product.system.name").unwrap_or_default();
    if product != PROJECT_CODE {
        bail!(
            "this firmware is for another model ({}). This pump needs {PROJECT_CODE}",
            if product.is_empty() {
                "unnamed"
            } else {
                &product
            }
        );
    }
    if value("ro.product.system.device").as_deref() != Some(DEVICE) {
        bail!("this firmware is for another device board");
    }
    let version = value("ro.build.display.id").unwrap_or_default();
    if parse_version(&version).is_none() {
        bail!("the package does not say which version it is");
    }
    Ok(Package { version, bytes })
}

/// "V1.0.6" as numbers, for comparing.
pub fn parse_version(text: &str) -> Option<Vec<u32>> {
    let text = text.trim().trim_start_matches(['V', 'v']);
    let parts: Option<Vec<u32>> = text.split('.').map(|part| part.parse().ok()).collect();
    parts.filter(|parts| !parts.is_empty())
}

/// The pump refuses to go back to an older version, so do not try.
pub fn is_downgrade(package: &str, installed: &str) -> bool {
    match (parse_version(package), parse_version(installed)) {
        (Some(package), Some(installed)) => package < installed,
        _ => false,
    }
}

struct Entry {
    name: String,
    method: u16,
    crc: u32,
    compressed: usize,
    size: usize,
    local: usize,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The zip's central directory.
fn entries(bytes: &[u8]) -> Result<Vec<Entry>> {
    const END: u32 = 0x0605_4b50;
    const CENTRAL: u32 = 0x0201_4b50;
    // The end record is 22 bytes plus a comment of up to 64 KB.
    let earliest = bytes.len().saturating_sub(22 + 65_535);
    let end = (earliest..=bytes.len().saturating_sub(22))
        .rev()
        .find(|at| u32_at(bytes, *at) == Some(END))
        .context("no zip directory")?;
    let count = u16_at(bytes, end + 10).context("short zip directory")? as usize;
    let mut at = u32_at(bytes, end + 16).context("short zip directory")? as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(bytes, at) != Some(CENTRAL) {
            bail!("broken zip directory");
        }
        let field = |offset| u32_at(bytes, at + offset).context("broken zip directory");
        let short = |offset| u16_at(bytes, at + offset).context("broken zip directory");
        let name_len = short(28)? as usize;
        let extra_len = short(30)? as usize;
        let comment_len = short(32)? as usize;
        let name = bytes
            .get(at + 46..at + 46 + name_len)
            .context("broken zip directory")?;
        entries.push(Entry {
            name: String::from_utf8_lossy(name).into_owned(),
            method: short(10)?,
            crc: field(16)?,
            compressed: field(20)? as usize,
            size: field(24)? as usize,
            local: field(42)? as usize,
        });
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(entries)
}

/// The bytes of an uncompressed entry.
fn stored_data<'a>(bytes: &'a [u8], entry: &Entry) -> Result<&'a [u8]> {
    const LOCAL: u32 = 0x0403_4b50;
    if entry.method != 0 || entry.compressed != entry.size {
        bail!(
            "{} in the package is compressed, unlike the official packages",
            entry.name
        );
    }
    if u32_at(bytes, entry.local) != Some(LOCAL) {
        bail!("broken zip entry for {}", entry.name);
    }
    let name_len = u16_at(bytes, entry.local + 26).context("broken zip entry")? as usize;
    let extra_len = u16_at(bytes, entry.local + 28).context("broken zip entry")? as usize;
    let start = entry.local + 30 + name_len + extra_len;
    bytes
        .get(start..start + entry.size)
        .with_context(|| format!("{} is cut short. Download the package again", entry.name))
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored zip, the way the official packages are built.
    fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in files {
            let local = out.len() as u32;
            let crc = crc32(data);
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]);
            central.extend_from_slice(&local.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    const PROP: &[u8] = b"ro.system.build.version.sdk=V1.2.7\nro.product.system.brand=ASUS\nro.product.system.device=WC07\nro.product.system.manufacturer=Byyeetek\nro.product.system.name=C09P06S0508\nro.build.display.id=V1.0.6\n";

    #[test]
    fn the_official_layout_is_accepted() {
        let package = check(zip(&[("update.img", b"image"), ("build.prop", PROP)])).unwrap();
        assert_eq!(package.version, "V1.0.6");
    }

    #[test]
    fn another_model_is_refused() {
        let prop = String::from_utf8_lossy(PROP).replace("C09P06S0508", "C09P04S0508");
        let error = check(zip(&[
            ("update.img", b"image"),
            ("build.prop", prop.as_bytes()),
        ]))
        .unwrap_err()
        .to_string();
        assert!(error.contains("another model"), "{error}");
    }

    #[test]
    fn a_damaged_image_is_refused() {
        let mut bytes = zip(&[("update.img", b"image"), ("build.prop", PROP)]);
        // Flip a byte inside update.img's data.
        let at = 30 + "update.img".len() + 2;
        bytes[at] ^= 0xff;
        let error = check(bytes).unwrap_err().to_string();
        assert!(error.contains("damaged"), "{error}");
    }

    #[test]
    fn extra_or_missing_files_are_refused() {
        assert!(check(zip(&[("build.prop", PROP)])).is_err());
        assert!(check(zip(&[
            ("update.img", b"image"),
            ("build.prop", PROP),
            ("run.sh", b"rm -rf /"),
        ]))
        .is_err());
        assert!(check(b"not a zip".to_vec()).is_err());
    }

    #[test]
    fn older_versions_count_as_downgrades() {
        assert!(is_downgrade("V1.0.5", "V1.0.6"));
        assert!(!is_downgrade("V1.0.6", "V1.0.6"));
        assert!(!is_downgrade("V1.0.10", "V1.0.6"));
        assert_eq!(parse_version("V1.0.6"), Some(vec![1, 0, 6]));
        assert_eq!(parse_version("unknown"), None);
    }

    #[test]
    fn the_downloaded_package_passes_when_present() {
        let Some(home) = std::env::var_os("USERPROFILE") else {
            return;
        };
        let path = Path::new(&home)
            .join("Downloads")
            .join("FW_ROG_Strix_LC_IV_360_ARGB_LCD_V1.0.6.zip");
        if path.exists() {
            assert_eq!(read(&path).unwrap().version, "V1.0.6");
        }
    }
}
