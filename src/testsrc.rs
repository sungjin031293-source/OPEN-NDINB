use crate::source::{now_100ns, Capture, Frame, FrameSink};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct ThreadCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    child: Arc<Mutex<Option<Child>>>,
    status: Arc<Mutex<String>>,
}

impl Capture for ThreadCapture {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
    fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }
}

impl Drop for ThreadCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn rgb_to_uyvy_pair(r: [u8; 2], g: [u8; 2], b: [u8; 2]) -> [u8; 4] {
    let y = |i: usize| (16.0 + 0.1826 * r[i] as f32 + 0.6142 * g[i] as f32 + 0.0620 * b[i] as f32) as u8;
    let (ra, ga, ba) = ((r[0] as f32 + r[1] as f32) / 2.0, (g[0] as f32 + g[1] as f32) / 2.0, (b[0] as f32 + b[1] as f32) / 2.0);
    let u = (128.0 - 0.1006 * ra - 0.3386 * ga + 0.4392 * ba) as u8;
    let v = (128.0 + 0.4392 * ra - 0.3989 * ga - 0.0403 * ba) as u8;
    [u, y(0), v, y(1)]
}

pub fn start_pattern(width: u32, height: u32, fps: u32, sink: FrameSink) -> Box<dyn Capture> {
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(format!("테스트 패턴 {width}x{height} {fps}p")));
    let s2 = stop.clone();
    let thread = std::thread::spawn(move || {
        let period = Duration::from_secs_f64(1.0 / fps as f64);
        let start = Instant::now();
        let mut n: u64 = 0;
        while !s2.load(Ordering::SeqCst) {
            let t = n as f32 / fps as f32;
            let stride = width as usize * 2;
            let mut data = vec![0u8; stride * height as usize];
            let cx = (width as f32) * (0.5 + 0.3 * (t * 0.8).sin());
            let cy = height as f32 * 0.55;
            let rad = height as f32 * 0.25;
            for y in 0..height as usize {
                let row = &mut data[y * stride..(y + 1) * stride];
                for x2 in 0..(width as usize / 2) {
                    let mut r = [0u8; 2];
                    let mut g = [0u8; 2];
                    let mut b = [0u8; 2];
                    for k in 0..2 {
                        let x = (x2 * 2 + k) as f32;
                        let d = ((x - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                        if d < rad {
                            r[k] = 230;
                            g[k] = 180;
                            b[k] = 150;
                        } else {
                            r[k] = ((x / width as f32) * 255.0) as u8;
                            g[k] = ((y as f32 / height as f32) * 255.0) as u8;
                            b[k] = 120;
                        }
                    }
                    row[x2 * 4..x2 * 4 + 4].copy_from_slice(&rgb_to_uyvy_pair(r, g, b));
                }
            }
            n += 1;
            let due = start + period * n as u32;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            sink(Frame { width, height, stride, data, alpha: None, fps_n: fps as i32, fps_d: 1, tc_100ns: now_100ns() });
        }
    });
    Box::new(ThreadCapture { stop, thread: Some(thread), child: Arc::new(Mutex::new(None)), status })
}

fn probe(path: &str) -> Result<(u32, u32, i32, i32), String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,r_frame_rate", "-of", "csv=p=0", path])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("ffprobe 실행 실패 (ffmpeg 설치 필요): {e}"))?;
    let s = String::from_utf8_lossy(&out.stdout);
    let parts: Vec<&str> = s.trim().split(',').collect();
    if parts.len() < 3 {
        return Err(format!("영상 정보를 읽을 수 없습니다: {path}"));
    }
    let w = parts[0].parse().map_err(|_| "width")?;
    let h = parts[1].parse().map_err(|_| "height")?;
    let mut fr = parts[2].split('/');
    let n = fr.next().and_then(|v| v.parse().ok()).unwrap_or(30);
    let d = fr.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    Ok((w, h, n, d))
}

pub fn start_file(path: &str, sink: FrameSink) -> Result<Box<dyn Capture>, String> {
    let (w, h, fps_n, fps_d) = probe(path)?;
    let (w, h) = (w & !1, h);
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-re", "-stream_loop", "-1", "-i", path, "-an", "-vf"])
        .arg(format!("crop={w}:{h}:0:0"))
        .args(["-pix_fmt", "uyvy422", "-color_range", "tv", "-f", "rawvideo", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("ffmpeg 실행 실패: {e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(format!("파일 {w}x{h} {:.2}p", fps_n as f64 / fps_d as f64)));
    let s2 = stop.clone();
    let st2 = status.clone();
    let thread = std::thread::spawn(move || {
        let stride = w as usize * 2;
        let size = stride * h as usize;
        while !s2.load(Ordering::SeqCst) {
            let mut data = vec![0u8; size];
            if stdout.read_exact(&mut data).is_err() {
                *st2.lock().unwrap() = "파일 재생 끝/오류".into();
                break;
            }
            sink(Frame { width: w, height: h, stride, data, alpha: None, fps_n, fps_d, tc_100ns: now_100ns() });
        }
    });
    Ok(Box::new(ThreadCapture { stop, thread: Some(thread), child: Arc::new(Mutex::new(Some(child))), status }))
}
