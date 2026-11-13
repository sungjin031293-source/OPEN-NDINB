use libloading::Library;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: u32,
    memory: u32,
}

pub const HISTORY: usize = 120;

#[derive(Default)]
pub struct GpuState {
    pub available: bool,
    pub util: VecDeque<f32>,
    pub last: f32,
}

pub fn start_monitor() -> Arc<Mutex<GpuState>> {
    let state = Arc::new(Mutex::new(GpuState::default()));
    let st = state.clone();
    std::thread::Builder::new()
        .name("gpu-monitor".into())
        .spawn(move || {
            let path = if cfg!(windows) { "nvml.dll" } else { "libnvidia-ml.so.1" };
            let Ok(lib) = (unsafe { Library::new(path) }) else { return };
            unsafe {
                let Ok(init) = lib.get::<unsafe extern "C" fn() -> i32>(b"nvmlInit_v2\0") else { return };
                let Ok(handle_by_index) = lib.get::<unsafe extern "C" fn(u32, *mut *mut c_void) -> i32>(b"nvmlDeviceGetHandleByIndex_v2\0") else { return };
                let Ok(get_util) = lib.get::<unsafe extern "C" fn(*mut c_void, *mut Utilization) -> i32>(b"nvmlDeviceGetUtilizationRates\0") else { return };
                if init() != 0 {
                    return;
                }
                let mut dev: *mut c_void = std::ptr::null_mut();
                if handle_by_index(0, &mut dev) != 0 {
                    return;
                }
                st.lock().unwrap().available = true;
                loop {
                    let mut u = Utilization::default();
                    if get_util(dev, &mut u) == 0 {
                        let mut s = st.lock().unwrap();
                        s.last = u.gpu as f32;
                        s.util.push_back(u.gpu as f32);
                        while s.util.len() > HISTORY {
                            s.util.pop_front();
                        }
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        })
        .ok();
    state
}
