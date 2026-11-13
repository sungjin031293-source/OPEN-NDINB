use crate::source::{Capture, FrameSink};

#[cfg(lav_embedded)]
mod imp {
    use super::*;
    use crate::source::{now_100ns, Frame};
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    include!(concat!(env!("OUT_DIR"), "/lav_embed.rs"));

    extern "C" {
        fn lav_open(path: *const c_char, err: *mut c_char, errcap: c_int, w: *mut c_int, h: *mut c_int, fps_n: *mut c_int, fps_d: *mut c_int, has_alpha: *mut c_int, codec: *mut c_char, codec_cap: c_int) -> *mut c_void;
        fn lav_read(r: *mut c_void, uyvy: *mut u8, alpha: *mut u8, got_alpha: *mut c_int, pts_100ns: *mut i64, err: *mut c_char, errcap: c_int) -> c_int;
        fn lav_rewind(r: *mut c_void) -> c_int;
        fn lav_close(r: *mut c_void);
    }

    static LOADED: OnceLock<Result<(), String>> = OnceLock::new();

    fn extract_dir() -> PathBuf {
        let base = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA")).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        base.join("OPEN-NDINB").join(LAV_TAG)
    }

    fn load_override(dir: &std::path::Path) -> Result<(), String> {
        for prefix in ["avutil", "swresample", "swscale", "avcodec", "avformat"] {
            let name = LAV_FILES
                .iter()
                .map(|(n, ..)| *n)
                .find(|n| n.starts_with(prefix))
                .ok_or_else(|| format!("{prefix} DLL name unknown"))?;
            let path = dir.join(name);
            let lib = unsafe { libloading::os::windows::Library::load_with_flags(&path, libloading::os::windows::LOAD_WITH_ALTERED_SEARCH_PATH) }
                .map_err(|e| format!("OPEN_NDINB_FFMPEG_DLL_DIR의 {name}을 불러올 수 없습니다: {e}"))?;
            let _ = lib.into_raw();
        }
        Ok(())
    }

    fn ensure_loaded() -> Result<(), String> {
        LOADED
            .get_or_init(|| {
                if let Some(custom) = std::env::var_os("OPEN_NDINB_FFMPEG_DLL_DIR") {
                    return load_override(std::path::Path::new(&custom));
                }
                let dir = extract_dir();
                std::fs::create_dir_all(&dir).map_err(|e| format!("영상 디코더 준비 실패 ({}): {e}", dir.display()))?;
                for (name, packed, size, is_dll) in LAV_FILES {
                    let path = dir.join(name);
                    let ok = std::fs::metadata(&path).map(|m| m.len() == *size).unwrap_or(false);
                    if !ok {
                        let raw = miniz_oxide::inflate::decompress_to_vec(packed).map_err(|e| format!("영상 디코더 압축 해제 실패 ({name}): {e:?}"))?;
                        let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
                        std::fs::write(&tmp, &raw).map_err(|e| format!("영상 디코더 저장 실패 ({name}): {e}"))?;
                        if std::fs::rename(&tmp, &path).is_err() {
                            let _ = std::fs::remove_file(&tmp);
                            if !std::fs::metadata(&path).map(|m| m.len() == *size).unwrap_or(false) {
                                return Err(format!("영상 디코더 파일을 쓸 수 없습니다: {}", path.display()));
                            }
                        }
                    }
                    if *is_dll {
                        let lib = unsafe { libloading::os::windows::Library::load_with_flags(&path, libloading::os::windows::LOAD_WITH_ALTERED_SEARCH_PATH) }
                            .map_err(|e| format!("영상 디코더를 불러올 수 없습니다 ({name}): {e}"))?;
                        let _ = lib.into_raw();
                    }
                }
                Ok(())
            })
            .clone()
    }

    struct Reader(*mut c_void);
    unsafe impl Send for Reader {}
    impl Drop for Reader {
        fn drop(&mut self) {
            unsafe { lav_close(self.0) }
        }
    }

    struct Info {
        w: u32,
        h: u32,
        fps_n: i32,
        fps_d: i32,
        alpha: bool,
        codec: String,
    }

    fn cstr(buf: &[u8]) -> String {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    fn open(path: &str) -> Result<(Reader, Info), String> {
        ensure_loaded()?;
        let cpath = CString::new(path).map_err(|_| "파일 경로가 올바르지 않습니다".to_string())?;
        let mut err = [0u8; 512];
        let mut codec = [0u8; 64];
        let (mut w, mut h, mut fn_, mut fd, mut a) = (0, 0, 0, 0, 0);
        let r = unsafe { lav_open(cpath.as_ptr(), err.as_mut_ptr() as *mut c_char, err.len() as c_int, &mut w, &mut h, &mut fn_, &mut fd, &mut a, codec.as_mut_ptr() as *mut c_char, codec.len() as c_int) };
        if r.is_null() {
            return Err(cstr(&err));
        }
        Ok((Reader(r), Info { w: w as u32, h: h as u32, fps_n: fn_.max(1), fps_d: fd.max(1), alpha: a != 0, codec: cstr(&codec) }))
    }

    struct LavCapture {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
        status: Arc<Mutex<String>>,
    }

    impl Capture for LavCapture {
        fn stop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
        fn status(&self) -> String {
            self.status.lock().unwrap().clone()
        }
    }

    impl Drop for LavCapture {
        fn drop(&mut self) {
            self.stop();
        }
    }

    pub fn start_file(path: &str, sink: FrameSink) -> Result<Box<dyn Capture>, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string());
        let status = Arc::new(Mutex::new(format!("{name} 여는 중...")));
        let (stop_t, status_t, path_t) = (stop.clone(), status.clone(), path.to_string());
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let thread = std::thread::Builder::new()
            .name("lav-file".into())
            .spawn(move || {
                let (mut reader, info) = match open(&path_t) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                let (w, h) = (info.w as usize, info.h as usize);
                *status_t.lock().unwrap() = format!(
                    "{name} · {} {}x{} {:.2}p{}",
                    info.codec,
                    w,
                    h,
                    info.fps_n as f64 / info.fps_d as f64,
                    if info.alpha { " · 알파 포함" } else { "" }
                );
                let _ = tx.send(Ok(()));
                let frame_100ns = (10_000_000i64 * info.fps_d as i64 / info.fps_n as i64).max(1);
                let mut clock = Instant::now();
                let mut base: Option<i64> = None;
                let mut offset: i64 = 0;
                let mut last: i64 = 0;
                let mut synth: i64 = 0;
                let mut err = [0u8; 512];
                while !stop_t.load(Ordering::SeqCst) {
                    let mut data = Vec::<u8>::with_capacity(w * 2 * h);
                    let mut alpha = Vec::<u8>::with_capacity(if info.alpha { w * h } else { 0 });
                    unsafe {
                        data.set_len(w * 2 * h);
                        alpha.set_len(alpha.capacity());
                    }
                    let mut got_alpha: c_int = 0;
                    let mut pts: i64 = -1;
                    let rc = unsafe {
                        lav_read(
                            reader.0,
                            data.as_mut_ptr(),
                            if info.alpha { alpha.as_mut_ptr() } else { std::ptr::null_mut() },
                            &mut got_alpha,
                            &mut pts,
                            err.as_mut_ptr() as *mut c_char,
                            err.len() as c_int,
                        )
                    };
                    if rc < 0 {
                        *status_t.lock().unwrap() = cstr(&err);
                        break;
                    }
                    if rc == 0 {
                        offset = last + frame_100ns;
                        if unsafe { lav_rewind(reader.0) } < 0 {
                            match open(&path_t) {
                                Ok((r, _)) => reader = r,
                                Err(e) => {
                                    *status_t.lock().unwrap() = e;
                                    break;
                                }
                            }
                        }
                        continue;
                    }
                    let ts = if pts >= 0 { pts } else { synth };
                    synth = ts + frame_100ns;
                    let t = ts + offset;
                    let b = *base.get_or_insert_with(|| {
                        clock = Instant::now();
                        t
                    });
                    last = t;
                    let due = clock + Duration::from_nanos(((t - b).max(0) as u64) * 100);
                    let now = Instant::now();
                    if due > now {
                        std::thread::sleep(due - now);
                    } else if now - due > Duration::from_millis(500) {
                        clock = now;
                        base = Some(t);
                    }
                    sink(Frame {
                        width: w as u32,
                        height: h as u32,
                        stride: w * 2,
                        data,
                        alpha: if got_alpha != 0 { Some(alpha) } else { None },
                        fps_n: info.fps_n,
                        fps_d: info.fps_d,
                        tc_100ns: now_100ns(),
                    });
                }
            })
            .map_err(|e| format!("영상 재생 스레드를 만들 수 없습니다: {e}"))?;
        match rx.recv() {
            Ok(Ok(())) => Ok(Box::new(LavCapture { stop, thread: Some(thread), status })),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err("영상 파일을 열 수 없습니다: 디코더 스레드가 종료되었습니다".into())
            }
        }
    }

    pub fn available() -> bool {
        true
    }

    pub fn notice() -> &'static str {
        LAV_NOTICE
    }
}

#[cfg(not(lav_embedded))]
mod imp {
    use super::*;
    pub fn start_file(_path: &str, _sink: FrameSink) -> Result<Box<dyn Capture>, String> {
        Err("내장 영상 디코더가 포함되지 않은 빌드입니다".into())
    }
    pub fn available() -> bool {
        false
    }
    pub fn notice() -> &'static str {
        ""
    }
}

pub use imp::{available, notice, start_file};

pub fn start_any(path: &str, sink: FrameSink) -> Result<Box<dyn Capture>, String> {
    let sink = std::sync::Arc::new(sink);
    let mk = |s: &std::sync::Arc<FrameSink>| -> FrameSink {
        let s = s.clone();
        Box::new(move |f| s(f))
    };
    if !std::path::Path::new(path).is_file() {
        return Err(format!("영상 파일을 찾을 수 없습니다: {path}"));
    }
    if !available() {
        return crate::webcam::start_file(path, mk(&sink));
    }
    start_file(path, mk(&sink)).or_else(|lav_err| crate::webcam::start_file(path, mk(&sink)).map_err(|_| format!("이 영상을 열 수 없습니다. {lav_err}")))
}
