//! NVIDIA GPU readings through NVML, which ships with the NVIDIA driver.
//!
//! The library is loaded at run time, so the program still starts on computers
//! without an NVIDIA card. Only the first GPU is read.

use std::ffi::c_void;
use std::sync::OnceLock;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuStats {
    /// Percent, 0 to 100.
    pub load: Option<f64>,
    /// Degrees Celsius.
    pub temperature: Option<f64>,
    /// Graphics clock in MHz.
    pub speed: Option<f64>,
    /// Watts.
    pub power: Option<f64>,
    /// Percent of the fan's top speed.
    pub fan: Option<f64>,
    /// Video memory in use and in total, in bytes.
    pub memory_used: Option<f64>,
    pub memory_total: Option<f64>,
}

type Device = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: u32,
    memory: u32,
}

type InitFn = unsafe extern "C" fn() -> i32;
type ByIndexFn = unsafe extern "C" fn(u32, *mut Device) -> i32;
/// Temperature and clock both take a selector and write a u32.
type SelectFn = unsafe extern "C" fn(Device, u32, *mut u32) -> i32;
type UtilizationFn = unsafe extern "C" fn(Device, *mut Utilization) -> i32;
/// Power (milliwatts) and fan speed (percent) both write one u32.
type ValueFn = unsafe extern "C" fn(Device, *mut u32) -> i32;
type MemoryFn = unsafe extern "C" fn(Device, *mut Memory) -> i32;

#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

struct Nvml {
    device: Device,
    temperature: SelectFn,
    utilization: UtilizationFn,
    clock: SelectFn,
    power: ValueFn,
    fan: ValueFn,
    memory: MemoryFn,
}

// NVML documents its calls as thread safe, and the device handle stays valid
// for the life of the process.
unsafe impl Send for Nvml {}
unsafe impl Sync for Nvml {}

const SUCCESS: i32 = 0;
const TEMPERATURE_GPU: u32 = 0;
const CLOCK_GRAPHICS: u32 = 0;

fn nvml() -> Option<&'static Nvml> {
    static NVML: OnceLock<Option<Nvml>> = OnceLock::new();
    NVML.get_or_init(load).as_ref()
}

pub fn read() -> Option<GpuStats> {
    let nvml = nvml()?;
    let mut stats = GpuStats::default();
    unsafe {
        let mut value = 0u32;
        if (nvml.temperature)(nvml.device, TEMPERATURE_GPU, &mut value) == SUCCESS {
            stats.temperature = Some(f64::from(value));
        }
        let mut usage = Utilization::default();
        if (nvml.utilization)(nvml.device, &mut usage) == SUCCESS {
            stats.load = Some(f64::from(usage.gpu));
        }
        let mut mhz = 0u32;
        if (nvml.clock)(nvml.device, CLOCK_GRAPHICS, &mut mhz) == SUCCESS {
            stats.speed = Some(f64::from(mhz));
        }
        let mut milliwatts = 0u32;
        if (nvml.power)(nvml.device, &mut milliwatts) == SUCCESS {
            stats.power = Some(f64::from(milliwatts) / 1000.0);
        }
        let mut percent = 0u32;
        if (nvml.fan)(nvml.device, &mut percent) == SUCCESS {
            stats.fan = Some(f64::from(percent));
        }
        let mut memory = Memory::default();
        if (nvml.memory)(nvml.device, &mut memory) == SUCCESS && memory.total > 0 {
            stats.memory_used = Some(memory.used as f64);
            stats.memory_total = Some(memory.total as f64);
        }
    }
    Some(stats)
}

fn load() -> Option<Nvml> {
    let library = open_library()?;
    // Each symbol is cast to the signature in NVML's nvml.h.
    unsafe {
        let init = std::mem::transmute::<*mut c_void, InitFn>(symbol(library, "nvmlInit_v2")?);
        let by_index = std::mem::transmute::<*mut c_void, ByIndexFn>(symbol(
            library,
            "nvmlDeviceGetHandleByIndex_v2",
        )?);
        if init() != SUCCESS {
            return None;
        }
        let mut device: Device = std::ptr::null_mut();
        if by_index(0, &mut device) != SUCCESS || device.is_null() {
            return None;
        }
        Some(Nvml {
            device,
            temperature: std::mem::transmute::<*mut c_void, SelectFn>(symbol(
                library,
                "nvmlDeviceGetTemperature",
            )?),
            utilization: std::mem::transmute::<*mut c_void, UtilizationFn>(symbol(
                library,
                "nvmlDeviceGetUtilizationRates",
            )?),
            clock: std::mem::transmute::<*mut c_void, SelectFn>(symbol(
                library,
                "nvmlDeviceGetClockInfo",
            )?),
            power: std::mem::transmute::<*mut c_void, ValueFn>(symbol(
                library,
                "nvmlDeviceGetPowerUsage",
            )?),
            fan: std::mem::transmute::<*mut c_void, ValueFn>(symbol(
                library,
                "nvmlDeviceGetFanSpeed",
            )?),
            memory: std::mem::transmute::<*mut c_void, MemoryFn>(symbol(
                library,
                "nvmlDeviceGetMemoryInfo",
            )?),
        })
    }
}

#[cfg(windows)]
fn open_library() -> Option<*mut c_void> {
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    }
    // Only look in System32, where the driver installs nvml.dll.
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
    let name: Vec<u16> = "nvml.dll".encode_utf16().chain(Some(0)).collect();
    let library = unsafe {
        LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    (!library.is_null()).then_some(library)
}

#[cfg(windows)]
unsafe fn symbol(library: *mut c_void, name: &str) -> Option<*mut c_void> {
    unsafe extern "system" {
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }
    let name = std::ffi::CString::new(name).ok()?;
    let address = unsafe { GetProcAddress(library, name.as_ptr().cast()) };
    (!address.is_null()).then_some(address)
}

#[cfg(target_os = "linux")]
fn open_library() -> Option<*mut c_void> {
    unsafe extern "C" {
        fn dlopen(name: *const std::ffi::c_char, flags: i32) -> *mut c_void;
    }
    const RTLD_NOW: i32 = 2;
    let library = unsafe { dlopen(c"libnvidia-ml.so.1".as_ptr(), RTLD_NOW) };
    (!library.is_null()).then_some(library)
}

#[cfg(target_os = "linux")]
unsafe fn symbol(library: *mut c_void, name: &str) -> Option<*mut c_void> {
    unsafe extern "C" {
        fn dlsym(library: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void;
    }
    let name = std::ffi::CString::new(name).ok()?;
    let address = unsafe { dlsym(library, name.as_ptr()) };
    (!address.is_null()).then_some(address)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn open_library() -> Option<*mut c_void> {
    None
}

#[cfg(not(any(windows, target_os = "linux")))]
unsafe fn symbol(_library: *mut c_void, _name: &str) -> Option<*mut c_void> {
    None
}
