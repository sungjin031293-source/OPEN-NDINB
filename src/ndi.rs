//! Minimal NDI 6 binding, loaded at runtime from the installed NDI Runtime
//! (NDI_RUNTIME_DIR_V6). Nothing from the NDI SDK is linked or redistributed.
#![allow(dead_code)]
use libloading::Library;
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr::null;
use std::sync::{Arc, OnceLock};

pub const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}
pub const FOURCC_UYVY: u32 = fourcc(b'U', b'Y', b'V', b'Y');
pub const FOURCC_UYVA: u32 = fourcc(b'U', b'Y', b'V', b'A');
pub const FOURCC_BGRA: u32 = fourcc(b'B', b'G', b'R', b'A');
pub const FOURCC_BGRX: u32 = fourcc(b'B', b'G', b'R', b'X');
const FRAME_FORMAT_PROGRESSIVE: i32 = 1;
pub const FRAME_TYPE_VIDEO: i32 = 1;
pub const RECV_COLOR_BGRX_BGRA: i32 = 0;
pub const RECV_BANDWIDTH_HIGHEST: i32 = 100;

#[repr(C)]
pub struct VideoFrameV2 {
    pub xres: i32,
    pub yres: i32,
    pub fourcc: u32,
    pub frame_rate_n: i32,
    pub frame_rate_d: i32,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: i32,
    pub timecode: i64,
    pub p_data: *mut u8,
    pub line_stride_in_bytes: i32,
    pub p_metadata: *const c_char,
    pub timestamp: i64,
}

impl Default for VideoFrameV2 {
    fn default() -> Self {
        Self {
            xres: 0,
            yres: 0,
            fourcc: 0,
            frame_rate_n: 0,
            frame_rate_d: 0,
            picture_aspect_ratio: 0.0,
            frame_format_type: FRAME_FORMAT_PROGRESSIVE,
            timecode: 0,
            p_data: std::ptr::null_mut(),
            line_stride_in_bytes: 0,
            p_metadata: null(),
            timestamp: 0,
        }
    }
}

#[repr(C)]
struct SendCreate {
    p_ndi_name: *const c_char,
    p_groups: *const c_char,
    clock_video: bool,
    clock_audio: bool,
}

#[repr(C)]
struct FindCreate {
    show_local_sources: bool,
    p_groups: *const c_char,
    p_extra_ips: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Source {
    pub p_ndi_name: *const c_char,
    pub p_url_address: *const c_char,
}

#[repr(C)]
struct RecvCreateV3 {
    source_to_connect_to: Source,
    color_format: i32,
    bandwidth: i32,
    allow_video_fields: bool,
    p_ndi_recv_name: *const c_char,
}

type Inst = *mut c_void;

pub struct Ndi {
    _lib: Library,
    pub dll_path: String,
    send_create: unsafe extern "C" fn(*const SendCreate) -> Inst,
    send_destroy: unsafe extern "C" fn(Inst),
    send_send_video_v2: unsafe extern "C" fn(Inst, *const VideoFrameV2),
    send_get_no_connections: unsafe extern "C" fn(Inst, u32) -> i32,
    find_create_v2: unsafe extern "C" fn(*const FindCreate) -> Inst,
    find_destroy: unsafe extern "C" fn(Inst),
    find_wait_for_sources: unsafe extern "C" fn(Inst, u32) -> bool,
    find_get_current_sources: unsafe extern "C" fn(Inst, *mut u32) -> *const Source,
    recv_create_v3: unsafe extern "C" fn(*const RecvCreateV3) -> Inst,
    recv_destroy: unsafe extern "C" fn(Inst),
    recv_capture_v2: unsafe extern "C" fn(Inst, *mut VideoFrameV2, *mut c_void, *mut c_void, u32) -> i32,
    recv_free_video_v2: unsafe extern "C" fn(Inst, *const VideoFrameV2),
}

unsafe impl Send for Ndi {}
unsafe impl Sync for Ndi {}

fn candidates() -> Vec<String> {
    let mut v = Vec::new();
    #[cfg(windows)]
    {
        for var in ["NDI_RUNTIME_DIR_V6", "NDI_RUNTIME_DIR_V5"] {
            if let Ok(dir) = std::env::var(var) {
                v.push(format!("{}\\Processing.NDI.Lib.x64.dll", dir.trim_end_matches('\\')));
            }
        }
        v.push(r"C:\Program Files\NDI\NDI 6 Runtime\v6\Processing.NDI.Lib.x64.dll".into());
        v.push("Processing.NDI.Lib.x64.dll".into());
    }
    #[cfg(not(windows))]
    {
        v.push("libndi.so.6".into());
        v.push("libndi.so".into());
    }
    v
}

static NDI: OnceLock<Result<Arc<Ndi>, String>> = OnceLock::new();

pub fn get() -> Result<Arc<Ndi>, String> {
    NDI.get_or_init(load).clone()
}

fn load() -> Result<Arc<Ndi>, String> {
    let mut last = String::new();
    for path in candidates() {
        match unsafe { Library::new(&path) } {
            Ok(lib) => unsafe {
                macro_rules! sym {
                    ($n:literal) => {
                        *lib.get(concat!($n, "\0").as_bytes()).map_err(|e| format!("NDI {}: {e}", $n))?
                    };
                }
                let init: unsafe extern "C" fn() -> bool = sym!("NDIlib_initialize");
                if !init() {
                    return Err("NDI 초기화 실패 (이 CPU에서 NDI를 지원하지 않음)".into());
                }
                return Ok(Arc::new(Ndi {
                    send_create: sym!("NDIlib_send_create"),
                    send_destroy: sym!("NDIlib_send_destroy"),
                    send_send_video_v2: sym!("NDIlib_send_send_video_v2"),
                    send_get_no_connections: sym!("NDIlib_send_get_no_connections"),
                    find_create_v2: sym!("NDIlib_find_create_v2"),
                    find_destroy: sym!("NDIlib_find_destroy"),
                    find_wait_for_sources: sym!("NDIlib_find_wait_for_sources"),
                    find_get_current_sources: sym!("NDIlib_find_get_current_sources"),
                    recv_create_v3: sym!("NDIlib_recv_create_v3"),
                    recv_destroy: sym!("NDIlib_recv_destroy"),
                    recv_capture_v2: sym!("NDIlib_recv_capture_v2"),
                    recv_free_video_v2: sym!("NDIlib_recv_free_video_v2"),
                    dll_path: path.clone(),
                    _lib: lib,
                }));
            },
            Err(e) => last = format!("{path}: {e}"),
        }
    }
    Err(format!(
        "NDI Runtime을 찾을 수 없습니다. https://ndi.link/NDIRedistV6 에서 설치하세요. ({last})"
    ))
}

pub struct Sender {
    ndi: Arc<Ndi>,
    inst: Inst,
    _name: CString,
}
unsafe impl Send for Sender {}

impl Sender {
    pub fn new(name: &str) -> Result<Self, String> {
        let ndi = get()?;
        let cname = CString::new(name).unwrap();
        let create = SendCreate { p_ndi_name: cname.as_ptr(), p_groups: null(), clock_video: false, clock_audio: false };
        let inst = unsafe { (ndi.send_create)(&create) };
        if inst.is_null() {
            return Err(format!("NDI 송출 생성 실패: {name}"));
        }
        Ok(Self { ndi, inst, _name: cname })
    }

    /// Synchronous send; `data` only needs to live for the duration of the call.
    pub fn send(&self, fourcc: u32, w: u32, h: u32, stride: usize, data: &[u8], fps_n: i32, fps_d: i32, timecode: i64) {
        let f = VideoFrameV2 {
            xres: w as i32,
            yres: h as i32,
            fourcc,
            frame_rate_n: fps_n,
            frame_rate_d: fps_d,
            picture_aspect_ratio: w as f32 / h as f32,
            timecode,
            p_data: data.as_ptr() as *mut u8,
            line_stride_in_bytes: stride as i32,
            ..Default::default()
        };
        unsafe { (self.ndi.send_send_video_v2)(self.inst, &f) }
    }

    pub fn connections(&self) -> i32 {
        unsafe { (self.ndi.send_get_no_connections)(self.inst, 0) }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        unsafe { (self.ndi.send_destroy)(self.inst) }
    }
}

pub struct Finder {
    ndi: Arc<Ndi>,
    inst: Inst,
}

impl Finder {
    pub fn new() -> Result<Self, String> {
        let ndi = get()?;
        let c = FindCreate { show_local_sources: true, p_groups: null(), p_extra_ips: null() };
        let inst = unsafe { (ndi.find_create_v2)(&c) };
        if inst.is_null() {
            return Err("NDI find 생성 실패".into());
        }
        Ok(Self { ndi, inst })
    }
    pub fn wait(&self, ms: u32) -> bool {
        unsafe { (self.ndi.find_wait_for_sources)(self.inst, ms) }
    }
    pub fn sources(&self) -> Vec<(String, String)> {
        let mut n = 0u32;
        let p = unsafe { (self.ndi.find_get_current_sources)(self.inst, &mut n) };
        let mut out = Vec::new();
        for i in 0..n as usize {
            let s = unsafe { *p.add(i) };
            let cs = |p: *const c_char| if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() };
            out.push((cs(s.p_ndi_name), cs(s.p_url_address)));
        }
        out
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        unsafe { (self.ndi.find_destroy)(self.inst) }
    }
}

pub struct Receiver {
    ndi: Arc<Ndi>,
    inst: Inst,
}
unsafe impl Send for Receiver {}

pub struct Received<'a> {
    rx: &'a Receiver,
    pub frame: VideoFrameV2,
}

impl Received<'_> {
    pub fn bytes(&self) -> &[u8] {
        let len = self.frame.line_stride_in_bytes as usize * self.frame.yres as usize;
        unsafe { std::slice::from_raw_parts(self.frame.p_data, len) }
    }
}

impl Drop for Received<'_> {
    fn drop(&mut self) {
        unsafe { (self.rx.ndi.recv_free_video_v2)(self.rx.inst, &self.frame) }
    }
}

impl Receiver {
    pub fn new(name: &str, url: &str) -> Result<Self, String> {
        let ndi = get()?;
        let cn = CString::new(name).unwrap();
        let cu = CString::new(url).unwrap();
        let rname = CString::new("ndi-probe").unwrap();
        let c = RecvCreateV3 {
            source_to_connect_to: Source { p_ndi_name: cn.as_ptr(), p_url_address: if url.is_empty() { null() } else { cu.as_ptr() } },
            color_format: RECV_COLOR_BGRX_BGRA,
            bandwidth: RECV_BANDWIDTH_HIGHEST,
            allow_video_fields: false,
            p_ndi_recv_name: rname.as_ptr(),
        };
        let inst = unsafe { (ndi.recv_create_v3)(&c) };
        if inst.is_null() {
            return Err(format!("NDI 수신 생성 실패: {name}"));
        }
        Ok(Self { ndi, inst })
    }

    pub fn capture(&self, timeout_ms: u32) -> Option<Received<'_>> {
        let mut f = VideoFrameV2::default();
        let t = unsafe { (self.ndi.recv_capture_v2)(self.inst, &mut f, std::ptr::null_mut(), std::ptr::null_mut(), timeout_ms) };
        if t == FRAME_TYPE_VIDEO {
            Some(Received { rx: self, frame: f })
        } else {
            None
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        unsafe { (self.ndi.recv_destroy)(self.inst) }
    }
}
