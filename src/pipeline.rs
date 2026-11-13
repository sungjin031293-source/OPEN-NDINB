use crate::maxine::Matter;
use crate::ndi::{self, Sender, FOURCC_UYVA, FOURCC_UYVY};
use crate::source::{now_100ns, Capture, Frame, FrameSink};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

pub const PREVIEW_W: usize = 640;

#[derive(Clone, PartialEq, Debug)]
pub enum SourceKind {
    DeckLink(usize),
    Webcam(usize),
    Pattern,
    File(String),
}

struct Slot {
    frame: Mutex<Option<Arc<Frame>>>,
    cv: Condvar,
}

impl Slot {
    fn new() -> Arc<Self> {
        Arc::new(Slot { frame: Mutex::new(None), cv: Condvar::new() })
    }
    fn put(&self, f: Arc<Frame>) -> bool {
        let dropped = self.frame.lock().unwrap().replace(f).is_some();
        self.cv.notify_one();
        dropped
    }
    fn take(&self, stop: &AtomicBool) -> Option<Arc<Frame>> {
        let mut g = self.frame.lock().unwrap();
        loop {
            if stop.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(f) = g.take() {
                return Some(f);
            }
            g = self.cv.wait_timeout(g, std::time::Duration::from_millis(100)).unwrap().0;
        }
    }
    fn wake(&self) {
        self.cv.notify_all();
    }
}

#[derive(Default)]
pub struct Preview {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
    pub serial: u64,
}

#[derive(Default)]
pub struct Stats {
    pub frames_in: AtomicU64,
    pub frames_orig: AtomicU64,
    pub frames_matte: AtomicU64,
    pub dropped_matte: AtomicU64,
    pub matte_us_last: AtomicU64,
    pub orig_send_lat_us: AtomicU64,
    pub matte_send_lat_us: AtomicU64,
    pub conn_orig: AtomicU64,
    pub conn_matte: AtomicU64,
}

pub struct Shared {
    pub stats: Stats,
    pub preview_orig: Mutex<Preview>,
    pub preview_matte: Mutex<Preview>,
    pub error: Mutex<Option<String>>,
    pub fmt: Mutex<String>,
}

pub struct Pipeline {
    pub shared: Arc<Shared>,
    capture: Option<Box<dyn Capture>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    orig_slot: Arc<Slot>,
    matte_slot: Arc<Slot>,
    pub names: (String, String),
}

fn uyvy_to_rgb(u: u8, y: u8, v: u8) -> [u8; 3] {
    let c = (y as f32 - 16.0) * 1.1644;
    let d = u as f32 - 128.0;
    let e = v as f32 - 128.0;
    let cl = |x: f32| x.clamp(0.0, 255.0) as u8;
    [cl(c + 1.7927 * e), cl(c - 0.2132 * d - 0.5329 * e), cl(c + 2.1124 * d)]
}

fn make_preview(f: &Frame, alpha: Option<&[u8]>, serial: u64) -> Preview {
    let pw = PREVIEW_W.min(f.width as usize);
    let ph = (f.height as usize * pw / f.width as usize).max(1);
    let mut rgba = vec![0u8; pw * ph * 4];
    for py in 0..ph {
        let sy = py * f.height as usize / ph;
        let row = &f.data[sy * f.stride..];
        for px in 0..pw {
            let sx = px * f.width as usize / pw;
            let pair = (sx / 2) * 4;
            let y = if sx % 2 == 0 { row[pair + 1] } else { row[pair + 3] };
            let mut rgb = uyvy_to_rgb(row[pair], y, row[pair + 2]);
            if let Some(a) = alpha {
                let al = a[sy * f.width as usize + sx] as u32;
                let checker = if ((px / 12) + (py / 12)) % 2 == 0 { 236u32 } else { 204u32 };
                for c in rgb.iter_mut() {
                    *c = ((*c as u32 * al + checker * (255 - al)) / 255) as u8;
                }
            }
            let o = (py * pw + px) * 4;
            rgba[o..o + 3].copy_from_slice(&rgb);
            rgba[o + 3] = 255;
        }
    }
    Preview { size: [pw, ph], rgba, serial }
}

pub fn host_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "PC".into())
}

impl Pipeline {
    pub fn start(kind: &SourceKind, mode: u32, base_name: &str) -> Result<Pipeline, String> {
        ndi::get()?;
        let name_alpha = format!("{base_name} - Alpha");
        let name_orig = format!("{base_name} - Original");
        let shared = Arc::new(Shared {
            stats: Stats::default(),
            preview_orig: Mutex::new(Preview::default()),
            preview_matte: Mutex::new(Preview::default()),
            error: Mutex::new(None),
            fmt: Mutex::new(String::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let orig_slot = Slot::new();
        let matte_slot = Slot::new();
        let mut threads = Vec::new();

        let send_orig = Sender::new(&name_orig)?;
        let send_matte = Sender::new(&name_alpha)?;

        {
            let (stop, slot, sh) = (stop.clone(), orig_slot.clone(), shared.clone());
            threads.push(std::thread::Builder::new().name("ndi-original".into()).spawn(move || {
                let mut last_preview = Instant::now();
                let mut serial = 0;
                let mut uyva_orig: Vec<u8> = Vec::new();
                while let Some(f) = slot.take(&stop) {
                    match &f.alpha {
                        Some(a) => {
                            let w = f.width as usize;
                            let h = f.height as usize;
                            uyva_orig.clear();
                            uyva_orig.reserve(w * 3 * h);
                            for y in 0..h {
                                uyva_orig.extend_from_slice(&f.data[y * f.stride..y * f.stride + w * 2]);
                            }
                            uyva_orig.extend_from_slice(&a[..w * h]);
                            send_orig.send(FOURCC_UYVA, f.width, f.height, w * 2, &uyva_orig, f.fps_n, f.fps_d, f.tc_100ns);
                        }
                        None => send_orig.send(FOURCC_UYVY, f.width, f.height, f.stride, &f.data, f.fps_n, f.fps_d, f.tc_100ns),
                    }
                    sh.stats.orig_send_lat_us.store(((now_100ns() - f.tc_100ns) / 10).max(0) as u64, Ordering::Relaxed);
                    let n = sh.stats.frames_orig.fetch_add(1, Ordering::Relaxed);
                    if n % 30 == 0 {
                        sh.stats.conn_orig.store(send_orig.connections().max(0) as u64, Ordering::Relaxed);
                    }
                    if last_preview.elapsed().as_millis() >= 33 {
                        last_preview = Instant::now();
                        serial += 1;
                        *sh.preview_orig.lock().unwrap() = make_preview(&f, f.alpha.as_deref(), serial);
                    }
                }
            }).unwrap());
        }

        {
            let (stop, slot, sh) = (stop.clone(), matte_slot.clone(), shared.clone());
            threads.push(std::thread::Builder::new().name("matte".into()).spawn(move || {
                let mut matter: Option<Matter> = None;
                let mut alpha = Vec::new();
                let mut uyva = Vec::new();
                let mut last_preview = Instant::now();
                let mut serial = 0;
                while let Some(f) = slot.take(&stop) {
                    let too_small = f.height < 288;
                    if !too_small && matter.as_ref().map(|m| m.size() != (f.width, f.height)).unwrap_or(true) {
                        drop(matter.take());
                        match Matter::new(f.width, f.height, mode) {
                            Ok(m) => matter = Some(m),
                            Err(e) => {
                                *sh.error.lock().unwrap() = Some(e);
                                return;
                            }
                        }
                    }
                    let t0 = Instant::now();
                    let res = if too_small {
                        let n = f.width as usize * f.height as usize;
                        alpha.clear();
                        match &f.alpha {
                            Some(a) => alpha.extend_from_slice(&a[..n]),
                            None => alpha.resize(n, 255),
                        }
                        Ok(())
                    } else {
                        matter.as_mut().unwrap().matte(&f.data, f.stride, &mut alpha)
                    };
                    if let Err(e) = res {
                        *sh.error.lock().unwrap() = Some(e);
                        return;
                    }
                    sh.stats.matte_us_last.store(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
                    let w = f.width as usize;
                    let h = f.height as usize;
                    uyva.clear();
                    uyva.reserve(w * 2 * h + w * h);
                    for y in 0..h {
                        uyva.extend_from_slice(&f.data[y * f.stride..y * f.stride + w * 2]);
                    }
                    uyva.extend_from_slice(&alpha);
                    send_matte.send(FOURCC_UYVA, f.width, f.height, w * 2, &uyva, f.fps_n, f.fps_d, f.tc_100ns);
                    sh.stats.matte_send_lat_us.store(((now_100ns() - f.tc_100ns) / 10).max(0) as u64, Ordering::Relaxed);
                    let n = sh.stats.frames_matte.fetch_add(1, Ordering::Relaxed);
                    if n % 30 == 0 {
                        sh.stats.conn_matte.store(send_matte.connections().max(0) as u64, Ordering::Relaxed);
                    }
                    if last_preview.elapsed().as_millis() >= 33 {
                        last_preview = Instant::now();
                        serial += 1;
                        *sh.preview_matte.lock().unwrap() = make_preview(&f, Some(&alpha), serial);
                    }
                }
            }).unwrap());
        }

        let sink: FrameSink = {
            let (os, ms, sh) = (orig_slot.clone(), matte_slot.clone(), shared.clone());
            Box::new(move |f: Frame| {
                let n = sh.stats.frames_in.fetch_add(1, Ordering::Relaxed);
                if n % 30 == 0 {
                    *sh.fmt.lock().unwrap() = format!("{}x{} {:.2}fps", f.width, f.height, f.fps_n as f64 / f.fps_d.max(1) as f64);
                }
                let f = Arc::new(f);
                os.put(f.clone());
                if ms.put(f) {
                    sh.stats.dropped_matte.fetch_add(1, Ordering::Relaxed);
                }
            })
        };

        let capture = match kind {
            SourceKind::Pattern => Ok(crate::testsrc::start_pattern(1920, 1080, 60, sink)),
            SourceKind::File(p) if p.trim().is_empty() => Err("영상 파일을 먼저 선택하세요.".to_string()),
            SourceKind::File(p) => crate::lav::start_any(p, sink),
            SourceKind::DeckLink(i) => crate::decklink::start(*i, sink),
            SourceKind::Webcam(i) => crate::webcam::start(*i, sink),
        };
        let mut p = Pipeline { shared, capture: None, stop, threads, orig_slot, matte_slot, names: (name_alpha, name_orig) };
        match capture {
            Ok(c) => {
                p.capture = Some(c);
                Ok(p)
            }
            Err(e) => {
                p.shutdown();
                Err(e)
            }
        }
    }

    pub fn status(&self) -> String {
        self.capture.as_ref().map(|c| c.status()).unwrap_or_default()
    }

    pub fn shutdown(&mut self) {
        if let Some(mut c) = self.capture.take() {
            c.stop();
        }
        self.stop.store(true, Ordering::SeqCst);
        self.orig_slot.wake();
        self.matte_slot.wake();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}
