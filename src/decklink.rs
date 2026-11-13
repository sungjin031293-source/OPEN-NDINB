//! Blackmagic DeckLink capture backend (COM API v12.0 via C++ shim on Windows).
use crate::source::{Capture, Frame, FrameSink};

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use crate::source::now_100ns;
    use std::ffi::{c_char, c_void, CStr};
    use std::ptr::NonNull;

    #[repr(C)]
    struct DlCapture {
        _private: [u8; 0],
    }

    type DlFrameCallback = unsafe extern "C" fn(
        user: *mut c_void,
        data: *const u8,
        width: u32,
        height: u32,
        stride: u32,
        fps_n: i32,
        fps_d: i32,
        has_signal: i32,
    );

    extern "C" {
        fn dl_list_devices(cb: unsafe extern "C" fn(user: *mut c_void, name: *const c_char), user: *mut c_void) -> i32;
        fn dl_start(
            index: u32,
            cb: DlFrameCallback,
            user: *mut c_void,
            err_buf: *mut c_char,
            err_cap: u32,
        ) -> *mut DlCapture;
        fn dl_stop(cap: *mut DlCapture);
        fn dl_destroy(cap: *mut DlCapture);
        fn dl_status(cap: *mut DlCapture, buf: *mut c_char, cap_len: u32) -> u32;
    }

    struct CallbackContext {
        sink: FrameSink,
    }

    unsafe extern "C" fn push_device_name(user: *mut c_void, name: *const c_char) {
        if user.is_null() || name.is_null() {
            return;
        }
        let list = &mut *(user as *mut Vec<String>);
        if let Ok(s) = CStr::from_ptr(name).to_str() {
            list.push(s.to_owned());
        } else {
            list.push(CStr::from_ptr(name).to_string_lossy().into_owned());
        }
    }

    unsafe extern "C" fn on_frame_arrived(
        user: *mut c_void,
        data: *const u8,
        width: u32,
        height: u32,
        stride: u32,
        fps_n: i32,
        fps_d: i32,
        has_signal: i32,
    ) {
        let tc_100ns = now_100ns();
        if has_signal == 0 || user.is_null() || data.is_null() || width == 0 || height == 0 {
            return;
        }
        let stride_usize = stride as usize;
        let min_row = (width as usize).saturating_mul(2);
        if stride_usize < min_row {
            return;
        }
        let byte_len = match stride_usize.checked_mul(height as usize) {
            Some(n) if n > 0 => n,
            _ => return,
        };

        let mut buf = Vec::<u8>::with_capacity(byte_len);
        std::ptr::copy_nonoverlapping(data, buf.as_mut_ptr(), byte_len);
        buf.set_len(byte_len);

        let ctx = &*(user as *const CallbackContext);
        (ctx.sink)(Frame {
            alpha: None,
            width,
            height,
            stride: stride_usize,
            data: buf,
            fps_n,
            fps_d,
            tc_100ns,
        });
    }

    pub struct DeckLinkCapture {
        raw: Option<NonNull<DlCapture>>,
        ctx: Option<Box<CallbackContext>>,
    }

    // SAFETY: DlCapture synchronizes COM callbacks internally with mutex/atomic state;
    // dl_stop joins/detaches the callback before ctx is dropped.
    unsafe impl Send for DeckLinkCapture {}

    impl DeckLinkCapture {
        fn stop_inner(&mut self) {
            if let Some(raw) = self.raw.take() {
                unsafe {
                    dl_stop(raw.as_ptr());
                    dl_destroy(raw.as_ptr());
                }
            }
            self.ctx.take();
        }
    }

    impl Capture for DeckLinkCapture {
        fn stop(&mut self) {
            self.stop_inner();
        }

        fn status(&self) -> String {
            let Some(raw) = self.raw else {
                return "중지됨".to_string();
            };
            let mut buf = [0u8; 256];
            let len = unsafe {
                dl_status(
                    raw.as_ptr(),
                    buf.as_mut_ptr() as *mut c_char,
                    buf.len() as u32,
                )
            } as usize;
            String::from_utf8_lossy(&buf[..len.min(buf.len())]).into_owned()
        }
    }

    impl Drop for DeckLinkCapture {
        fn drop(&mut self) {
            self.stop_inner();
        }
    }

    pub fn list_devices() -> Vec<String> {
        let mut names = Vec::new();
        unsafe {
            dl_list_devices(push_device_name, &mut names as *mut Vec<String> as *mut c_void);
        }
        names
    }

    pub fn start(index: usize, sink: FrameSink) -> Result<Box<dyn Capture>, String> {
        let mut ctx = Box::new(CallbackContext { sink });
        let ctx_ptr = (&mut *ctx) as *mut CallbackContext as *mut c_void;
        let mut err_buf = [0u8; 512];

        let raw = unsafe {
            dl_start(
                index as u32,
                on_frame_arrived,
                ctx_ptr,
                err_buf.as_mut_ptr() as *mut c_char,
                err_buf.len() as u32,
            )
        };

        let Some(non_null) = NonNull::new(raw) else {
            let msg = unsafe { CStr::from_ptr(err_buf.as_ptr() as *const c_char) }
                .to_string_lossy()
                .into_owned();
            if msg.is_empty() {
                return Err("Blackmagic Desktop Video 드라이버가 없습니다".to_string());
            }
            return Err(msg);
        };

        Ok(Box::new(DeckLinkCapture {
            raw: Some(non_null),
            ctx: Some(ctx),
        }))
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::*;

    pub fn list_devices() -> Vec<String> {
        Vec::new()
    }

    pub fn start(_index: usize, _sink: FrameSink) -> Result<Box<dyn Capture>, String> {
        Err("Blackmagic Desktop Video 드라이버가 없습니다".to_string())
    }
}

pub fn list_devices() -> Vec<String> {
    imp::list_devices()
}

pub fn start(index: usize, sink: FrameSink) -> Result<Box<dyn Capture>, String> {
    imp::start(index, sink)
}
