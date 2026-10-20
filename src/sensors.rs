//! Raw readings from the operating system. None of these need a driver or
//! administrator rights. Counters are cumulative; `stats::Sampler` turns them
//! into rates.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuTimes {
    pub idle: u64,
    pub total: u64,
}

/// Bytes in use and in total.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Space {
    pub used: u64,
    pub total: u64,
}

/// Bytes moved since boot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Traffic {
    pub read: u64,
    pub written: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    /// 0 is Sunday.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
}

pub fn cpu_usage(before: CpuTimes, after: CpuTimes) -> Option<f64> {
    let total = after.total.checked_sub(before.total)?;
    let idle = after.idle.checked_sub(before.idle)?;
    if total == 0 {
        return None;
    }
    Some((1.0 - idle as f64 / total as f64).clamp(0.0, 1.0) * 100.0)
}

fn average(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values
        .filter(|value| *value > 0.0)
        .fold((0.0, 0u32), |(sum, count), value| (sum + value, count + 1));
    (count > 0).then(|| sum / f64::from(count))
}

#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "linux")]
pub use self::linux::*;

#[cfg(not(any(windows, target_os = "linux")))]
pub use self::other::*;

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    pub fn cpu_times() -> Option<CpuTimes> {
        unsafe extern "system" {
            fn GetSystemTimes(
                idle: *mut FileTime,
                kernel: *mut FileTime,
                user: *mut FileTime,
            ) -> i32;
        }
        let (mut idle, mut kernel, mut user) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return None;
        }
        let value = |time: &FileTime| (u64::from(time.high) << 32) | u64::from(time.low);
        // Kernel time already includes idle time.
        Some(CpuTimes {
            idle: value(&idle),
            total: value(&kernel) + value(&user),
        })
    }

    /// Average current MHz across cores, as Windows reports it.
    pub fn cpu_speed() -> Option<f64> {
        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        struct ProcessorPowerInformation {
            number: u32,
            max_mhz: u32,
            current_mhz: u32,
            mhz_limit: u32,
            max_idle_state: u32,
            current_idle_state: u32,
        }
        #[link(name = "powrprof")]
        unsafe extern "system" {
            fn CallNtPowerInformation(
                level: i32,
                input: *const c_void,
                input_len: u32,
                output: *mut c_void,
                output_len: u32,
            ) -> i32;
        }
        const PROCESSOR_INFORMATION: i32 = 11;
        let count = std::thread::available_parallelism().ok()?.get();
        let mut info = vec![ProcessorPowerInformation::default(); count];
        let bytes = std::mem::size_of_val(info.as_slice()) as u32;
        let status = unsafe {
            CallNtPowerInformation(
                PROCESSOR_INFORMATION,
                std::ptr::null(),
                0,
                info.as_mut_ptr().cast(),
                bytes,
            )
        };
        if status != 0 {
            return None;
        }
        average(info.iter().map(|cpu| f64::from(cpu.current_mhz)))
    }

    /// Needs a kernel driver on Windows. HWiNFO can supply it instead.
    pub fn cpu_temperature() -> Option<f64> {
        None
    }

    pub fn memory() -> Option<Space> {
        #[repr(C)]
        struct MemoryStatus {
            length: u32,
            memory_load: u32,
            total_phys: u64,
            avail_phys: u64,
            total_page_file: u64,
            avail_page_file: u64,
            total_virtual: u64,
            avail_virtual: u64,
            avail_extended_virtual: u64,
        }
        unsafe extern "system" {
            fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
        }
        let mut status = MemoryStatus {
            length: std::mem::size_of::<MemoryStatus>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
            return None;
        }
        Some(Space {
            used: status.total_phys.saturating_sub(status.avail_phys),
            total: status.total_phys,
        })
    }

    /// The drive Windows is installed on.
    pub fn system_disk() -> Option<Space> {
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(
                path: *const u16,
                free_to_caller: *mut u64,
                total: *mut u64,
                free: *mut u64,
            ) -> i32;
        }
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let path: Vec<u16> = format!("{drive}\\").encode_utf16().chain(Some(0)).collect();
        let (mut caller, mut total, mut free) = (0u64, 0u64, 0u64);
        if unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut caller, &mut total, &mut free) } == 0 {
            return None;
        }
        Some(Space {
            used: total.saturating_sub(free),
            total,
        })
    }

    /// Received and sent bytes on physical network adapters. Virtual adapters
    /// such as Hyper-V switches are skipped, so traffic is not counted twice.
    pub fn network() -> Option<Traffic> {
        #[link(name = "iphlpapi")]
        unsafe extern "system" {
            fn GetIfTable2(table: *mut *mut u8) -> u32;
            fn FreeMibTable(table: *mut c_void);
        }
        // MIB_IF_TABLE2 is NumEntries, then MIB_IF_ROW2 entries from byte 8.
        // Offsets inside MIB_IF_ROW2 (1352 bytes) from netioapi.h.
        const ROW: usize = 1352;
        const FLAGS: usize = 1152;
        const OPER_STATUS: usize = 1156;
        const IN_OCTETS: usize = 1208;
        const OUT_OCTETS: usize = 1280;
        const HARDWARE_INTERFACE: u8 = 1;
        const OPER_UP: u32 = 1;
        let mut table: *mut u8 = std::ptr::null_mut();
        if unsafe { GetIfTable2(&mut table) } != 0 || table.is_null() {
            return None;
        }
        let mut traffic = Traffic::default();
        unsafe {
            let count = std::ptr::read_unaligned(table.cast::<u32>()) as usize;
            for index in 0..count {
                let row = table.add(8 + index * ROW);
                let hardware = *row.add(FLAGS) & HARDWARE_INTERFACE != 0;
                let up = std::ptr::read_unaligned(row.add(OPER_STATUS).cast::<u32>()) == OPER_UP;
                if hardware && up {
                    traffic.read += std::ptr::read_unaligned(row.add(IN_OCTETS).cast::<u64>());
                    traffic.written += std::ptr::read_unaligned(row.add(OUT_OCTETS).cast::<u64>());
                }
            }
            FreeMibTable(table.cast());
        }
        Some(traffic)
    }

    /// Disk throughput comes from the PhysicalDisk performance counters, which
    /// already report bytes per second.
    pub struct DiskCounters {
        query: isize,
        read: isize,
        write: isize,
    }

    #[link(name = "pdh")]
    unsafe extern "system" {
        fn PdhOpenQueryW(source: *const u16, user: usize, query: *mut isize) -> u32;
        fn PdhAddEnglishCounterW(
            query: isize,
            path: *const u16,
            user: usize,
            counter: *mut isize,
        ) -> u32;
        fn PdhCollectQueryData(query: isize) -> u32;
        fn PdhGetFormattedCounterValue(
            counter: isize,
            format: u32,
            kind: *mut u32,
            value: *mut FormattedValue,
        ) -> u32;
        fn PdhCloseQuery(query: isize) -> u32;
    }

    #[repr(C)]
    #[derive(Default)]
    struct FormattedValue {
        status: u32,
        value: f64,
    }

    impl DiskCounters {
        pub fn open() -> Option<Self> {
            let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
            let mut query = 0isize;
            if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != 0 {
                return None;
            }
            let (mut read, mut write) = (0isize, 0isize);
            let read_path = wide(r"\PhysicalDisk(_Total)\Disk Read Bytes/sec");
            let write_path = wide(r"\PhysicalDisk(_Total)\Disk Write Bytes/sec");
            let ok = unsafe {
                PdhAddEnglishCounterW(query, read_path.as_ptr(), 0, &mut read) == 0
                    && PdhAddEnglishCounterW(query, write_path.as_ptr(), 0, &mut write) == 0
            };
            if !ok {
                unsafe { PdhCloseQuery(query) };
                return None;
            }
            // Rate counters need a first sample to compare against.
            unsafe { PdhCollectQueryData(query) };
            Some(Self { query, read, write })
        }

        /// Bytes per second read and written since the previous call.
        pub fn rates(&mut self) -> Option<(f64, f64)> {
            const DOUBLE: u32 = 0x0000_0200;
            if unsafe { PdhCollectQueryData(self.query) } != 0 {
                return None;
            }
            let value = |counter: isize| -> Option<f64> {
                let mut kind = 0u32;
                let mut value = FormattedValue::default();
                let status =
                    unsafe { PdhGetFormattedCounterValue(counter, DOUBLE, &mut kind, &mut value) };
                (status == 0).then_some(value.value.max(0.0))
            };
            Some((value(self.read)?, value(self.write)?))
        }
    }

    impl Drop for DiskCounters {
        fn drop(&mut self) {
            unsafe { PdhCloseQuery(self.query) };
        }
    }

    pub fn local_time() -> LocalTime {
        #[repr(C)]
        #[derive(Default)]
        struct SystemTime {
            year: u16,
            month: u16,
            weekday: u16,
            day: u16,
            hour: u16,
            minute: u16,
            second: u16,
            milliseconds: u16,
        }
        unsafe extern "system" {
            fn GetLocalTime(time: *mut SystemTime);
        }
        let mut now = SystemTime::default();
        unsafe { GetLocalTime(&mut now) };
        LocalTime {
            year: now.year,
            month: now.month as u8,
            day: now.day as u8,
            weekday: now.weekday as u8,
            hour: now.hour as u8,
            minute: now.minute as u8,
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    pub fn cpu_times() -> Option<CpuTimes> {
        parse_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)
    }

    pub fn cpu_speed() -> Option<f64> {
        parse_cpuinfo_mhz(&std::fs::read_to_string("/proc/cpuinfo").ok()?)
    }

    /// Package or die temperature from the CPU's hwmon driver.
    pub fn cpu_temperature() -> Option<f64> {
        const DRIVERS: [&str; 3] = ["k10temp", "zenpower", "coretemp"];
        for entry in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
            let dir = entry.path();
            let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
            if !DRIVERS.contains(&name.trim()) {
                continue;
            }
            // temp1 is Tctl on AMD and the package sensor on Intel.
            let raw = std::fs::read_to_string(dir.join("temp1_input")).ok()?;
            let millidegrees: f64 = raw.trim().parse().ok()?;
            return Some(millidegrees / 1000.0);
        }
        None
    }

    pub fn memory() -> Option<Space> {
        parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)
    }

    pub fn system_disk() -> Option<Space> {
        // struct statvfs on 64-bit glibc is 112 bytes: f_bsize, f_frsize,
        // f_blocks, f_bfree, f_bavail, then more fields.
        unsafe extern "C" {
            fn statvfs(path: *const std::ffi::c_char, buf: *mut [u64; 14]) -> i32;
        }
        let mut buf = [0u64; 14];
        if unsafe { statvfs(c"/".as_ptr(), &mut buf) } != 0 {
            return None;
        }
        let (fragment, blocks, available) = (buf[1], buf[2], buf[4]);
        let total = blocks.saturating_mul(fragment);
        Some(Space {
            used: total.saturating_sub(available.saturating_mul(fragment)),
            total,
        })
    }

    pub fn network() -> Option<Traffic> {
        parse_net_dev(&std::fs::read_to_string("/proc/net/dev").ok()?)
    }

    /// Reads /proc/diskstats. Rates come from the change between calls.
    pub struct DiskCounters {
        last: Option<(Traffic, std::time::Instant)>,
    }

    impl DiskCounters {
        pub fn open() -> Option<Self> {
            Some(Self { last: None })
        }

        pub fn rates(&mut self) -> Option<(f64, f64)> {
            let text = std::fs::read_to_string("/proc/diskstats").ok()?;
            let now = parse_diskstats(&text, |name| {
                std::path::Path::new("/sys/block").join(name).exists()
            });
            let at = std::time::Instant::now();
            let previous = self.last.replace((now, at));
            let (before, then) = previous?;
            let seconds = at.duration_since(then).as_secs_f64();
            if seconds <= 0.0 {
                return None;
            }
            Some((
                now.read.saturating_sub(before.read) as f64 / seconds,
                now.written.saturating_sub(before.written) as f64 / seconds,
            ))
        }
    }

    pub fn local_time() -> LocalTime {
        // struct tm starts with nine ints: sec, min, hour, mday, mon, year,
        // wday, yday, isdst. Room is left for the fields after them.
        #[repr(C, align(8))]
        struct Tm([i32; 16]);
        unsafe extern "C" {
            fn time(out: *mut i64) -> i64;
            fn localtime_r(time: *const i64, tm: *mut Tm) -> *mut Tm;
        }
        let mut tm = Tm([0; 16]);
        unsafe {
            let now = time(std::ptr::null_mut());
            localtime_r(&now, &mut tm);
        }
        let tm = tm.0;
        LocalTime {
            year: (tm[5] + 1900) as u16,
            month: (tm[4] + 1) as u8,
            day: tm[3] as u8,
            weekday: tm[6] as u8,
            hour: tm[2] as u8,
            minute: tm[1] as u8,
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod other {
    use super::*;

    pub fn cpu_times() -> Option<CpuTimes> {
        None
    }
    pub fn cpu_speed() -> Option<f64> {
        None
    }
    pub fn cpu_temperature() -> Option<f64> {
        None
    }
    pub fn memory() -> Option<Space> {
        None
    }
    pub fn system_disk() -> Option<Space> {
        None
    }
    pub fn network() -> Option<Traffic> {
        None
    }
    pub struct DiskCounters;
    impl DiskCounters {
        pub fn open() -> Option<Self> {
            None
        }
        pub fn rates(&mut self) -> Option<(f64, f64)> {
            None
        }
    }
    pub fn local_time() -> LocalTime {
        LocalTime::default()
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse().ok())
        .collect();
    if values.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal. Guest time is already in user.
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    let total = values.iter().take(8).sum();
    Some(CpuTimes { idle, total })
}

#[cfg(any(target_os = "linux", test))]
fn parse_cpuinfo_mhz(text: &str) -> Option<f64> {
    average(text.lines().filter_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim() != "cpu MHz" {
            return None;
        }
        value.trim().parse::<f64>().ok()
    }))
}

#[cfg(any(target_os = "linux", test))]
fn parse_meminfo(text: &str) -> Option<Space> {
    let field = |name: &str| -> Option<u64> {
        let line = text.lines().find(|line| line.starts_with(name))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    };
    let total = field("MemTotal:")?;
    let available = field("MemAvailable:")?;
    Some(Space {
        used: total.saturating_sub(available),
        total,
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_net_dev(text: &str) -> Option<Traffic> {
    let mut traffic = Traffic::default();
    let mut found = false;
    for line in text.lines().skip(2) {
        let Some((name, counters)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == "lo" {
            continue;
        }
        let values: Vec<u64> = counters
            .split_whitespace()
            .filter_map(|value| value.parse().ok())
            .collect();
        if values.len() >= 9 {
            traffic.read += values[0];
            traffic.written += values[8];
            found = true;
        }
    }
    found.then_some(traffic)
}

/// Sums whole disks only. Partitions would count the same bytes twice.
#[cfg(any(target_os = "linux", test))]
fn parse_diskstats(text: &str, is_whole_disk: impl Fn(&str) -> bool) -> Traffic {
    const SECTOR: u64 = 512;
    let mut traffic = Traffic::default();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        let name = fields[2];
        let virtual_disk = ["loop", "ram", "zram", "dm-", "sr", "md"]
            .iter()
            .any(|prefix| name.starts_with(prefix));
        if virtual_disk || !is_whole_disk(name) {
            continue;
        }
        let read: u64 = fields[5].parse().unwrap_or(0);
        let written: u64 = fields[9].parse().unwrap_or(0);
        traffic.read += read * SECTOR;
        traffic.written += written * SECTOR;
    }
    traffic
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_comes_from_the_change_between_samples() {
        let before = CpuTimes {
            idle: 100,
            total: 200,
        };
        let after = CpuTimes {
            idle: 175,
            total: 300,
        };
        assert_eq!(cpu_usage(before, after), Some(25.0));
        assert_eq!(cpu_usage(after, before), None);
    }

    #[test]
    fn proc_stat_counts_iowait_as_idle() {
        let text = "cpu  10 0 10 70 10 0 0 0 0 0\ncpu0 1 0 1 7 1 0 0 0 0 0\n";
        assert_eq!(
            parse_proc_stat(text),
            Some(CpuTimes {
                idle: 80,
                total: 100
            })
        );
    }

    #[test]
    fn cpuinfo_speed_is_the_average_of_cores() {
        let text = "processor\t: 0\ncpu MHz\t\t: 3000.000\nprocessor\t: 1\ncpu MHz\t\t: 4000.000\n";
        assert_eq!(parse_cpuinfo_mhz(text), Some(3500.0));
    }

    #[test]
    fn meminfo_used_is_total_minus_available() {
        let text = "MemTotal:       16000 kB\nMemFree:         1000 kB\nMemAvailable:    6000 kB\n";
        assert_eq!(
            parse_meminfo(text),
            Some(Space {
                used: 10000 * 1024,
                total: 16000 * 1024
            })
        );
    }

    #[test]
    fn net_dev_skips_loopback() {
        let text = "Inter-|   Receive\n face |bytes\n    lo: 500 1 0 0 0 0 0 0 500 1 0 0 0 0 0 0\n  eth0: 1000 5 0 0 0 0 0 0 2000 6 0 0 0 0 0 0\n";
        assert_eq!(
            parse_net_dev(text),
            Some(Traffic {
                read: 1000,
                written: 2000
            })
        );
    }

    #[test]
    fn diskstats_counts_whole_disks_once() {
        let text = "   8       0 sda 10 0 100 0 20 0 200 0 0 0 0\n   8       1 sda1 10 0 100 0 20 0 200 0 0 0 0\n   7       0 loop0 1 0 50 0 1 0 50 0 0 0 0\n";
        let traffic = parse_diskstats(text, |name| name == "sda");
        assert_eq!(
            traffic,
            Traffic {
                read: 100 * 512,
                written: 200 * 512
            }
        );
    }
}
