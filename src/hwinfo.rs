//! Sensor readings from HWiNFO, when it is running with "Shared Memory
//! Support" turned on. HWiNFO has the driver that reads CPU temperature, pump
//! and fan speeds, and voltages, so this program does not need one.
//!
//! The block is HWiNFO_SENSORS_SHARED_MEM2 from HWiNFO's published SDK. It is
//! only read, never written.

/// Kind of reading, from SENSOR_READING_TYPE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Temperature,
    Voltage,
    Fan,
    Current,
    Power,
    Clock,
    Usage,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub kind: Kind,
    pub label: String,
    pub value: f64,
}

const SIGNATURE: u32 = 0x5369_5748; // "HWiS"
const LABEL: usize = 128;
const UNIT: usize = 16;
// Inside a reading element: type, sensor index, reading id, original label,
// user label, unit, then value, min, max, and average as doubles.
const READING_TYPE: usize = 0;
const READING_LABEL_USER: usize = 12 + LABEL;
const READING_VALUE: usize = 12 + LABEL * 2 + UNIT;

/// Every reading HWiNFO currently shares, or `None` if it is not running with
/// shared memory on.
pub fn read() -> Option<Vec<Reading>> {
    parse(&snapshot()?)
}

/// Parse a copy of the shared block. Element sizes come from the header, so
/// newer HWiNFO versions with longer elements still parse.
pub fn parse(block: &[u8]) -> Option<Vec<Reading>> {
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(block.get(at..at + 4)?.try_into().ok()?))
    };
    if u32_at(0)? != SIGNATURE {
        return None;
    }
    // Header: signature, version, revision, poll time (8 bytes), then the
    // sensor section offset, size, and count, and the reading section offset,
    // size, and count.
    let offset = u32_at(32)? as usize;
    let size = u32_at(36)? as usize;
    let count = u32_at(40)? as usize;
    if size < READING_VALUE + 8 || count > 100_000 {
        return None;
    }
    let mut readings = Vec::with_capacity(count);
    for index in 0..count {
        let start = offset.checked_add(index.checked_mul(size)?)?;
        let element = block.get(start..start + size)?;
        let kind = match u32::from_le_bytes(element[READING_TYPE..4].try_into().ok()?) {
            1 => Kind::Temperature,
            2 => Kind::Voltage,
            3 => Kind::Fan,
            4 => Kind::Current,
            5 => Kind::Power,
            6 => Kind::Clock,
            7 => Kind::Usage,
            8 => Kind::Other,
            _ => continue,
        };
        let label = c_string(&element[READING_LABEL_USER..READING_LABEL_USER + LABEL]);
        let value = f64::from_le_bytes(element[READING_VALUE..READING_VALUE + 8].try_into().ok()?);
        if value.is_finite() {
            readings.push(Reading { kind, label, value });
        }
    }
    Some(readings)
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// The first reading of `kind` whose label contains one of `names`, tried in
/// order. Matching ignores case.
pub fn find(readings: &[Reading], kind: Kind, names: &[&str]) -> Option<f64> {
    names.iter().find_map(|name| {
        let name = name.to_ascii_lowercase();
        readings
            .iter()
            .find(|reading| {
                reading.kind == kind && reading.label.to_ascii_lowercase().contains(&name)
            })
            .map(|reading| reading.value)
    })
}

#[cfg(windows)]
fn snapshot() -> Option<Vec<u8>> {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
        fn MapViewOfFile(
            mapping: *mut c_void,
            access: u32,
            high: u32,
            low: u32,
            bytes: usize,
        ) -> *mut c_void;
        fn UnmapViewOfFile(view: *const c_void) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn VirtualQuery(address: *const c_void, info: *mut MemoryInfo, length: usize) -> usize;
    }
    #[repr(C)]
    struct MemoryInfo {
        base: *mut c_void,
        allocation_base: *mut c_void,
        allocation_protect: u32,
        partition: u16,
        region_size: usize,
        state: u32,
        protect: u32,
        kind: u32,
    }
    const FILE_MAP_READ: u32 = 0x0004;
    let name: Vec<u16> = "Global\\HWiNFO_SENS_SM2"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        let mapping = OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr());
        if mapping.is_null() {
            return None;
        }
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        if view.is_null() {
            CloseHandle(mapping);
            return None;
        }
        let mut info: MemoryInfo = std::mem::zeroed();
        let copied = if VirtualQuery(view, &mut info, std::mem::size_of::<MemoryInfo>()) != 0 {
            Some(std::slice::from_raw_parts(view.cast::<u8>(), info.region_size).to_vec())
        } else {
            None
        };
        UnmapViewOfFile(view);
        CloseHandle(mapping);
        copied
    }
}

#[cfg(not(windows))]
fn snapshot() -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(readings: &[(u32, &str, f64)]) -> Vec<u8> {
        const SIZE: usize = READING_VALUE + 32 + 144; // newer HWiNFO adds UTF-8 copies
        let offset = 64usize;
        let mut data = vec![0u8; offset + SIZE * readings.len()];
        data[0..4].copy_from_slice(&SIGNATURE.to_le_bytes());
        data[32..36].copy_from_slice(&(offset as u32).to_le_bytes());
        data[36..40].copy_from_slice(&(SIZE as u32).to_le_bytes());
        data[40..44].copy_from_slice(&(readings.len() as u32).to_le_bytes());
        for (index, (kind, label, value)) in readings.iter().enumerate() {
            let at = offset + index * SIZE;
            data[at..at + 4].copy_from_slice(&kind.to_le_bytes());
            let label_at = at + READING_LABEL_USER;
            data[label_at..label_at + label.len()].copy_from_slice(label.as_bytes());
            let value_at = at + READING_VALUE;
            data[value_at..value_at + 8].copy_from_slice(&value.to_le_bytes());
        }
        data
    }

    #[test]
    fn readings_parse_with_longer_elements() {
        let data = block(&[(1, "CPU (Tctl/Tdie)", 61.5), (3, "AIO Pump", 2100.0)]);
        let readings = parse(&data).unwrap();
        assert_eq!(readings.len(), 2);
        assert_eq!(readings[0].kind, Kind::Temperature);
        assert_eq!(readings[0].label, "CPU (Tctl/Tdie)");
        assert_eq!(readings[1].value, 2100.0);
    }

    #[test]
    fn find_tries_names_in_order_and_checks_the_kind() {
        let data = block(&[
            (1, "CPU Package", 70.0),
            (1, "CPU (Tctl/Tdie)", 65.0),
            (3, "CPU", 900.0),
            (3, "W_PUMP+", 2400.0),
        ]);
        let readings = parse(&data).unwrap();
        let tctl = find(&readings, Kind::Temperature, &["tctl", "cpu package"]);
        assert_eq!(tctl, Some(65.0));
        assert_eq!(find(&readings, Kind::Fan, &["pump"]), Some(2400.0));
        assert_eq!(find(&readings, Kind::Temperature, &["pump"]), None);
    }

    #[test]
    fn other_blocks_are_ignored() {
        assert_eq!(parse(&[0u8; 64]), None);
        assert_eq!(parse(&[]), None);
    }
}
