//! Readings for the stats panel, gathered about once a second.
//!
//! Most come from the operating system. GPU readings come from NVIDIA's NVML.
//! CPU temperature on Windows, CPU power, and pump, coolant, and fan readings
//! come from HWiNFO when it is running with shared memory on.

use crate::gpu;
use crate::hwinfo::{self, Kind};
use crate::sensors::{self, CpuTimes, DiskCounters, LocalTime, Traffic};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    CpuUsage,
    CpuClock,
    CpuTemp,
    CpuPower,
    GpuUsage,
    GpuClock,
    GpuTemp,
    GpuPower,
    GpuFan,
    GpuMemory,
    MemoryUsage,
    MemoryUsed,
    DiskUsage,
    DiskRead,
    DiskWrite,
    NetDown,
    NetUp,
    PumpSpeed,
    CoolantTemp,
    FanSpeed,
    Time,
    Date,
}

/// In the order the window lists them.
pub const ALL: [Metric; 22] = [
    Metric::CpuUsage,
    Metric::CpuClock,
    Metric::CpuTemp,
    Metric::CpuPower,
    Metric::GpuUsage,
    Metric::GpuClock,
    Metric::GpuTemp,
    Metric::GpuPower,
    Metric::GpuFan,
    Metric::GpuMemory,
    Metric::MemoryUsage,
    Metric::MemoryUsed,
    Metric::DiskUsage,
    Metric::DiskRead,
    Metric::DiskWrite,
    Metric::NetDown,
    Metric::NetUp,
    Metric::PumpSpeed,
    Metric::CoolantTemp,
    Metric::FanSpeed,
    Metric::Time,
    Metric::Date,
];

/// Where a reading comes from, so the window can say what is missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    System,
    Nvidia,
    /// HWiNFO on Windows. Linux reads CPU temperature from hwmon.
    HwInfo,
}

impl Metric {
    pub fn group(self) -> &'static str {
        use Metric::*;
        match self {
            CpuUsage | CpuClock | CpuTemp | CpuPower => "CPU",
            GpuUsage | GpuClock | GpuTemp | GpuPower | GpuFan | GpuMemory => "GPU",
            MemoryUsage | MemoryUsed => "Memory",
            DiskUsage | DiskRead | DiskWrite => "Disk",
            NetDown | NetUp => "Network",
            PumpSpeed | CoolantTemp | FanSpeed => "Cooling",
            Time | Date => "Clock",
        }
    }

    /// Name in the window, under its group.
    pub fn name(self) -> &'static str {
        use Metric::*;
        match self {
            CpuUsage | GpuUsage | MemoryUsage => "Usage",
            CpuClock | GpuClock => "Clock",
            CpuTemp | GpuTemp => "Temperature",
            CpuPower | GpuPower => "Power",
            GpuFan => "Fan",
            GpuMemory => "Memory",
            MemoryUsed => "In use",
            DiskUsage => "Space used",
            DiskRead => "Read speed",
            DiskWrite => "Write speed",
            NetDown => "Download",
            NetUp => "Upload",
            PumpSpeed => "Pump speed",
            CoolantTemp => "Coolant",
            FanSpeed => "Fan speed",
            Time => "Time",
            Date => "Date",
        }
    }

    /// Short caption drawn on the pump.
    pub fn label(self) -> &'static str {
        use Metric::*;
        match self {
            CpuUsage => "CPU",
            CpuClock => "CPU CLOCK",
            CpuTemp => "CPU TEMP",
            CpuPower => "CPU POWER",
            GpuUsage => "GPU",
            GpuClock => "GPU CLOCK",
            GpuTemp => "GPU TEMP",
            GpuPower => "GPU POWER",
            GpuFan => "GPU FAN",
            GpuMemory => "VRAM",
            MemoryUsage => "RAM",
            MemoryUsed => "RAM USED",
            DiskUsage => "DISK",
            DiskRead => "DISK READ",
            DiskWrite => "DISK WRITE",
            NetDown => "DOWNLOAD",
            NetUp => "UPLOAD",
            PumpSpeed => "PUMP",
            CoolantTemp => "COOLANT",
            FanSpeed => "FAN",
            Time => "TIME",
            Date => "DATE",
        }
    }

    pub fn source(self) -> Source {
        use Metric::*;
        match self {
            GpuUsage | GpuClock | GpuTemp | GpuPower | GpuFan | GpuMemory => Source::Nvidia,
            CpuTemp | CpuPower | PumpSpeed | CoolantTemp | FanSpeed => Source::HwInfo,
            _ => Source::System,
        }
    }
}

/// A reading, with the top of its scale when it has one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Value {
    pub now: f64,
    pub max: Option<f64>,
}

impl Value {
    fn of(now: f64) -> Self {
        Self { now, max: None }
    }

    fn out_of(now: f64, max: f64) -> Self {
        Self {
            now,
            max: Some(max),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Readings {
    values: HashMap<Metric, Value>,
    pub time: LocalTime,
}

const WEEKDAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

impl Readings {
    pub fn get(&self, metric: Metric) -> Option<Value> {
        match metric {
            Metric::Time | Metric::Date => Some(Value::of(0.0)),
            _ => self.values.get(&metric).copied(),
        }
    }

    pub fn has(&self, metric: Metric) -> bool {
        self.get(metric).is_some()
    }

    /// How full the bar under a reading is, from 0 to 1.
    pub fn fraction(&self, metric: Metric) -> Option<f64> {
        let value = self.get(metric)?;
        let max = match metric {
            Metric::CpuTemp | Metric::GpuTemp => Some(100.0),
            Metric::CoolantTemp => Some(60.0),
            _ => value.max,
        }?;
        (max > 0.0).then(|| (value.now / max).clamp(0.0, 1.0))
    }

    /// The reading as drawn on the pump. Missing readings show a dash.
    pub fn text(&self, metric: Metric) -> String {
        use Metric::*;
        match metric {
            Time => return format!("{:02}:{:02}", self.time.hour, self.time.minute),
            Date => {
                let weekday = WEEKDAYS.get(self.time.weekday as usize).unwrap_or(&"");
                let month = MONTHS
                    .get((self.time.month as usize).wrapping_sub(1))
                    .unwrap_or(&"");
                return format!("{weekday} {} {month}", self.time.day);
            }
            _ => {}
        }
        let Some(value) = self.get(metric) else {
            return "—".into();
        };
        let now = value.now;
        match metric {
            CpuUsage | GpuUsage | MemoryUsage | DiskUsage | GpuFan => format!("{now:.0}%"),
            CpuClock | GpuClock => clock(now),
            CpuTemp | GpuTemp | CoolantTemp => format!("{now:.0}°C"),
            CpuPower | GpuPower => format!("{now:.0} W"),
            MemoryUsed | GpuMemory => match value.max {
                Some(total) => format!("{:.1}/{:.0} GB", gigabytes(now), gigabytes(total)),
                None => format!("{:.1} GB", gigabytes(now)),
            },
            DiskRead | DiskWrite | NetDown | NetUp => rate(now),
            PumpSpeed | FanSpeed => format!("{now:.0} RPM"),
            Time | Date => unreachable!(),
        }
    }

    #[cfg(test)]
    pub fn set(&mut self, metric: Metric, now: f64, max: Option<f64>) {
        self.values.insert(metric, Value { now, max });
    }
}

fn clock(mhz: f64) -> String {
    if mhz >= 1000.0 {
        format!("{:.2} GHz", mhz / 1000.0)
    } else {
        format!("{mhz:.0} MHz")
    }
}

fn gigabytes(bytes: f64) -> f64 {
    bytes / 1_073_741_824.0
}

fn rate(bytes_per_second: f64) -> String {
    let kib = bytes_per_second / 1024.0;
    if kib >= 1024.0 * 100.0 {
        format!("{:.0} MB/s", kib / 1024.0)
    } else if kib >= 1024.0 {
        format!("{:.1} MB/s", kib / 1024.0)
    } else {
        format!("{kib:.0} KB/s")
    }
}

/// Keeps the previous counters so rates cover the time between samples.
pub struct Sampler {
    cpu: Option<CpuTimes>,
    network: Option<(Traffic, Instant)>,
    disk: Option<DiskCounters>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self {
            cpu: None,
            network: None,
            disk: DiskCounters::open(),
        }
    }
}

impl Sampler {
    pub fn sample(&mut self) -> Readings {
        let mut values = HashMap::new();
        let mut put = |metric: Metric, value: Option<Value>| {
            if let Some(value) = value {
                values.insert(metric, value);
            }
        };

        let cpu = sensors::cpu_times();
        if let (Some(before), Some(after)) = (self.cpu, cpu) {
            put(
                Metric::CpuUsage,
                sensors::cpu_usage(before, after).map(|usage| Value::out_of(usage, 100.0)),
            );
        }
        if cpu.is_some() {
            self.cpu = cpu;
        }
        put(Metric::CpuClock, sensors::cpu_speed().map(Value::of));
        put(Metric::CpuTemp, sensors::cpu_temperature().map(Value::of));

        if let Some(memory) = sensors::memory().filter(|memory| memory.total > 0) {
            let (used, total) = (memory.used as f64, memory.total as f64);
            put(
                Metric::MemoryUsage,
                Some(Value::out_of(used / total * 100.0, 100.0)),
            );
            put(Metric::MemoryUsed, Some(Value::out_of(used, total)));
        }
        if let Some(disk) = sensors::system_disk().filter(|disk| disk.total > 0) {
            let share = disk.used as f64 / disk.total as f64 * 100.0;
            put(Metric::DiskUsage, Some(Value::out_of(share, 100.0)));
        }
        if let Some((read, write)) = self.disk.as_mut().and_then(DiskCounters::rates) {
            put(Metric::DiskRead, Some(Value::of(read)));
            put(Metric::DiskWrite, Some(Value::of(write)));
        }
        if let Some(traffic) = sensors::network() {
            let at = Instant::now();
            if let Some((before, then)) = self.network.replace((traffic, at)) {
                let seconds = at.duration_since(then).as_secs_f64();
                if seconds > 0.0 {
                    let down = traffic.read.saturating_sub(before.read) as f64 / seconds;
                    let up = traffic.written.saturating_sub(before.written) as f64 / seconds;
                    put(Metric::NetDown, Some(Value::of(down)));
                    put(Metric::NetUp, Some(Value::of(up)));
                }
            }
        }

        if let Some(gpu) = gpu::read() {
            put(
                Metric::GpuUsage,
                gpu.load.map(|load| Value::out_of(load, 100.0)),
            );
            put(Metric::GpuClock, gpu.speed.map(Value::of));
            put(Metric::GpuTemp, gpu.temperature.map(Value::of));
            put(Metric::GpuPower, gpu.power.map(Value::of));
            put(Metric::GpuFan, gpu.fan.map(|fan| Value::out_of(fan, 100.0)));
            if let (Some(used), Some(total)) = (gpu.memory_used, gpu.memory_total) {
                put(Metric::GpuMemory, Some(Value::out_of(used, total)));
            }
        }

        if let Some(shared) = hwinfo::read() {
            let find = |kind, names: &[&str]| hwinfo::find(&shared, kind, names).map(Value::of);
            if let Some(temp) = find(
                Kind::Temperature,
                &["tctl", "cpu package", "cpu die", "cpu"],
            ) {
                put(Metric::CpuTemp, Some(temp));
            }
            put(
                Metric::CpuPower,
                find(Kind::Power, &["cpu package power", "cpu ppt", "cpu power"]),
            );
            put(Metric::PumpSpeed, find(Kind::Fan, &["pump"]));
            put(
                Metric::CoolantTemp,
                find(Kind::Temperature, &["coolant", "liquid", "water"]),
            );
            put(
                Metric::FanSpeed,
                find(Kind::Fan, &["cpu", "chassis", "system"]),
            );
        }

        Readings {
            values,
            time: sensors::local_time(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_format_for_the_panel() {
        let mut readings = Readings::default();
        readings.set(Metric::CpuUsage, 12.6, Some(100.0));
        readings.set(Metric::CpuClock, 4321.0, None);
        readings.set(Metric::GpuTemp, 61.4, None);
        readings.set(
            Metric::MemoryUsed,
            19.5 * 1_073_741_824.0,
            Some(64.0 * 1_073_741_824.0),
        );
        readings.set(Metric::NetDown, 2.5 * 1024.0 * 1024.0, None);
        readings.set(Metric::NetUp, 300.0 * 1024.0, None);
        readings.set(Metric::PumpSpeed, 2100.4, None);
        assert_eq!(readings.text(Metric::CpuUsage), "13%");
        assert_eq!(readings.text(Metric::CpuClock), "4.32 GHz");
        assert_eq!(readings.text(Metric::GpuTemp), "61°C");
        assert_eq!(readings.text(Metric::MemoryUsed), "19.5/64 GB");
        assert_eq!(readings.text(Metric::NetDown), "2.5 MB/s");
        assert_eq!(readings.text(Metric::NetUp), "300 KB/s");
        assert_eq!(readings.text(Metric::PumpSpeed), "2100 RPM");
        assert_eq!(readings.text(Metric::CpuPower), "—");
    }

    #[test]
    fn bars_use_the_scale_of_each_reading() {
        let mut readings = Readings::default();
        readings.set(Metric::CpuUsage, 25.0, Some(100.0));
        readings.set(Metric::GpuTemp, 50.0, None);
        readings.set(Metric::NetDown, 1000.0, None);
        assert_eq!(readings.fraction(Metric::CpuUsage), Some(0.25));
        assert_eq!(readings.fraction(Metric::GpuTemp), Some(0.5));
        assert_eq!(readings.fraction(Metric::NetDown), None);
    }

    #[test]
    fn date_and_time_come_from_the_local_clock() {
        let readings = Readings {
            time: LocalTime {
                year: 2026,
                month: 10,
                day: 19,
                weekday: 1,
                hour: 9,
                minute: 5,
            },
            ..Readings::default()
        };
        assert_eq!(readings.text(Metric::Time), "09:05");
        assert_eq!(readings.text(Metric::Date), "MON 19 OCT");
    }

    #[test]
    fn metric_names_round_trip_through_settings() {
        let json = serde_json::to_string(&Metric::GpuMemory).unwrap();
        assert_eq!(json, "\"gpu_memory\"");
        let back: Metric = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Metric::GpuMemory);
    }
}
