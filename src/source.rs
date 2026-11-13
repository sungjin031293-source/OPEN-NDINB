//! Shared capture interface. Every source delivers 8-bit 4:2:2 UYVY frames (DeckLink bmdFormat8BitYUV layout),
//! which is also what NDI sends without conversion (UYVY for the original, UYVA for the matte).
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row of `data` (>= width * 2).
    pub stride: usize,
    /// UYVY 4:2:2, BT.709 video range.
    pub data: Vec<u8>,
    /// Optional per-pixel alpha (width*height, tightly packed) carried by the source file (ProRes 4444, DXV3 with alpha).
    pub alpha: Option<Vec<u8>>,
    pub fps_n: i32,
    pub fps_d: i32,
    /// Capture wall-clock time in 100 ns units since the UNIX epoch (sent as the NDI timecode; used for latency).
    pub tc_100ns: i64,
}

pub type FrameSink = Box<dyn Fn(Frame) + Send + Sync + 'static>;

/// A running capture. Dropping or calling stop() must stop delivering frames before returning.
pub trait Capture: Send {
    fn stop(&mut self);
    /// Short human readable state, e.g. "1920x1080 59.94p" or "신호 없음".
    fn status(&self) -> String;
}

pub fn now_100ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| (d.as_nanos() / 100) as i64).unwrap_or(0)
}
