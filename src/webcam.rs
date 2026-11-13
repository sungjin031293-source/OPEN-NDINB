//! Windows Media Foundation webcam capture backend.
use crate::source::{Capture, FrameSink};

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use crate::source::{now_100ns, Frame};
    use std::ffi::c_void;
    use std::ptr;
    use std::slice;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use windows::core::{Interface, HSTRING, PWSTR};
    use windows::Win32::Media::MediaFoundation::{
        IMF2DBuffer, IMFActivate, IMFAttributes, IMFMediaBuffer, IMFMediaSource,
        IMFSample, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
        MFCreateSourceReaderFromMediaSource, MFCreateSourceReaderFromURL, MFEnumDeviceSources, MFMediaType_Video, MFShutdown,
        MFStartup, MFVideoFormat_NV12, MFVideoFormat_RGB32, MFVideoFormat_YUY2,
        MFSTARTUP_FULL, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
        MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        MF_LOW_LATENCY, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
        MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_READWRITE_DISABLE_CONVERTERS,
        MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
        MF_SOURCE_READERF_ERROR, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
        MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
    };
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
    };

    const STREAM_INDEX: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

    struct ComGuard {
        need_uninit: bool,
    }

    impl ComGuard {
        fn new() -> Self {
            unsafe {
                let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
                // S_OK (0) or S_FALSE (1) must be balanced with CoUninitialize().
                // RPC_E_CHANGED_MODE (0x80010106) means COM is already initialized on this thread with STA.
                let need_uninit = hr.is_ok();
                Self { need_uninit }
            }
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.need_uninit {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }

    struct MfGuard;

    impl MfGuard {
        fn new() -> Result<Self, String> {
            unsafe {
                MFStartup(MF_VERSION, MFSTARTUP_FULL)
                    .map_err(|e| format!("Media Foundation 초기화 실패: {e}"))?;
            }
            Ok(Self)
        }
    }

    impl Drop for MfGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
            }
        }
    }

    struct ActivateList {
        ptr: *mut Option<IMFActivate>,
        count: u32,
    }

    impl ActivateList {
        fn enumerate() -> Result<Self, String> {
            unsafe {
                let mut attrs: Option<IMFAttributes> = None;
                MFCreateAttributes(&mut attrs, 1)
                    .map_err(|e| format!("MF 속성 생성 실패: {e}"))?;
                let attrs = attrs.ok_or_else(|| "MF 속성 객체가 비어 있습니다.".to_string())?;
                attrs
                    .SetGUID(
                        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
                    )
                    .map_err(|e| format!("MF 캡처 장치 속성 설정 실패: {e}"))?;

                let mut ptr: *mut Option<IMFActivate> = ptr::null_mut();
                let mut count: u32 = 0;
                MFEnumDeviceSources(&attrs, &mut ptr, &mut count)
                    .map_err(|e| format!("웹캠 장치 열거 실패: {e}"))?;
                Ok(Self { ptr, count })
            }
        }

        fn as_slice(&self) -> &[Option<IMFActivate>] {
            if self.ptr.is_null() || self.count == 0 {
                &[]
            } else {
                unsafe { slice::from_raw_parts(self.ptr, self.count as usize) }
            }
        }

        fn friendly_name(act: &IMFActivate, idx: usize) -> String {
            unsafe {
                let mut pwstr = PWSTR::null();
                let mut len: u32 = 0;
                if act
                    .GetAllocatedString(
                        &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
                        &mut pwstr,
                        &mut len,
                    )
                    .is_ok()
                    && !pwstr.is_null()
                {
                    let slice = slice::from_raw_parts(pwstr.0, len as usize);
                    let name = String::from_utf16_lossy(slice);
                    CoTaskMemFree(Some(pwstr.0 as *const c_void));
                    if !name.trim().is_empty() {
                        return name;
                    }
                }
            }
            format!("웹캠 #{idx}")
        }
    }

    impl Drop for ActivateList {
        fn drop(&mut self) {
            if !self.ptr.is_null() {
                unsafe {
                    for i in 0..self.count as usize {
                        let _ = (*self.ptr.add(i)).take();
                    }
                    CoTaskMemFree(Some(self.ptr as *const c_void));
                }
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum OutputFormat {
        Yuy2,
        Nv12,
        Rgb32,
    }

    struct ModeCandidate {
        width: u32,
        height: u32,
        fps_n: u32,
        fps_d: u32,
    }

    impl ModeCandidate {
        fn fps_f64(&self) -> f64 {
            if self.fps_d == 0 {
                0.0
            } else {
                self.fps_n as f64 / self.fps_d as f64
            }
        }

        fn pixels(&self) -> u64 {
            (self.width as u64) * (self.height as u64)
        }

        fn within_1080p(&self) -> bool {
            self.width <= 1920 && self.height <= 1080
        }
    }

    fn unpack_u64(val: u64) -> (u32, u32) {
        ((val >> 32) as u32, (val & 0xFFFF_FFFF) as u32)
    }

    fn pack_u64(hi: u32, lo: u32) -> u64 {
        ((hi as u64) << 32) | (lo as u64)
    }

    unsafe fn select_best_native_mode(reader: &IMFSourceReader) -> Option<ModeCandidate> {
        let mut best: Option<ModeCandidate> = None;
        let mut idx: u32 = 0;
        while let Ok(mt) = reader.GetNativeMediaType(STREAM_INDEX, idx) {
            idx += 1;
            if let Ok(major) = mt.GetGUID(&MF_MT_MAJOR_TYPE) {
                if major != MFMediaType_Video {
                    continue;
                }
            }
            let Ok(size_packed) = mt.GetUINT64(&MF_MT_FRAME_SIZE) else {
                continue;
            };
            let (w, h) = unpack_u64(size_packed);
            if w < 2 || h == 0 {
                continue;
            }
            let (fps_n, fps_d) = mt
                .GetUINT64(&MF_MT_FRAME_RATE)
                .map(unpack_u64)
                .unwrap_or((30, 1));
            let fps_n = if fps_n == 0 { 30 } else { fps_n };
            let fps_d = if fps_d == 0 { 1 } else { fps_d };
            let cand = ModeCandidate {
                width: w,
                height: h,
                fps_n,
                fps_d,
            };

            let replace = match &best {
                None => true,
                Some(cur) => {
                    match (cur.within_1080p(), cand.within_1080p()) {
                        (false, true) => true,
                        (true, false) => false,
                        (true, true) => {
                            // Prefer highest resolution up to 1920x1080, then highest fps
                            let cp = cur.pixels();
                            let np = cand.pixels();
                            if np != cp {
                                np > cp
                            } else {
                                cand.fps_f64() > cur.fps_f64()
                            }
                        }
                        (false, false) => {
                            // If all modes exceed 1080p, pick smallest above 1080p, then highest fps
                            let cp = cur.pixels();
                            let np = cand.pixels();
                            if np != cp {
                                np < cp
                            } else {
                                cand.fps_f64() > cur.fps_f64()
                            }
                        }
                    }
                }
            };
            if replace {
                best = Some(cand);
            }
        }
        best
    }

    unsafe fn configure_output_type(
        reader: &IMFSourceReader,
        chosen: Option<&ModeCandidate>,
    ) -> Result<(OutputFormat, u32, u32, i32, i32, i32), String> {
        let formats = [
            (MFVideoFormat_YUY2, OutputFormat::Yuy2),
            (MFVideoFormat_NV12, OutputFormat::Nv12),
            (MFVideoFormat_RGB32, OutputFormat::Rgb32),
        ];

        let mut selected_fmt: Option<OutputFormat> = None;

        // First attempt: set subtype + chosen size + chosen frame rate
        if let Some(mode) = chosen {
            for (guid, fmt) in formats {
                if let Ok(mt) = MFCreateMediaType() {
                    let _ = mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
                    let _ = mt.SetGUID(&MF_MT_SUBTYPE, &guid);
                    let _ = mt.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(mode.width, mode.height));
                    let _ = mt.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(mode.fps_n, mode.fps_d));
                    if reader.SetCurrentMediaType(STREAM_INDEX, None, &mt).is_ok() {
                        selected_fmt = Some(fmt);
                        break;
                    }
                }
            }
            // Second attempt: set subtype + chosen size only
            if selected_fmt.is_none() {
                for (guid, fmt) in formats {
                    if let Ok(mt) = MFCreateMediaType() {
                        let _ = mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
                        let _ = mt.SetGUID(&MF_MT_SUBTYPE, &guid);
                        let _ = mt.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(mode.width, mode.height));
                        if reader.SetCurrentMediaType(STREAM_INDEX, None, &mt).is_ok() {
                            selected_fmt = Some(fmt);
                            break;
                        }
                    }
                }
            }
        }

        // Fallback attempt: set subtype only
        if selected_fmt.is_none() {
            for (guid, fmt) in formats {
                if let Ok(mt) = MFCreateMediaType() {
                    let _ = mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
                    let _ = mt.SetGUID(&MF_MT_SUBTYPE, &guid);
                    if reader.SetCurrentMediaType(STREAM_INDEX, None, &mt).is_ok() {
                        selected_fmt = Some(fmt);
                        break;
                    }
                }
            }
        }

        let fmt = selected_fmt.ok_or_else(|| {
            "웹캠 출력 포맷(YUY2/NV12/RGB32)을 설정할 수 없습니다.".to_string()
        })?;

        let cur_mt = reader
            .GetCurrentMediaType(STREAM_INDEX)
            .map_err(|e| format!("웹캠 현재 미디어 타입 조회 실패: {e}"))?;

        let (raw_w, raw_h) = cur_mt
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .map(unpack_u64)
            .or_else(|_| {
                chosen
                    .map(|m| (m.width, m.height))
                    .ok_or_else(|| "해상도 정보 없음".to_string())
            })
            .map_err(|_| "웹캠 해상도를 확인할 수 없습니다.".to_string())?;

        let (fps_n_u, fps_d_u) = cur_mt
            .GetUINT64(&MF_MT_FRAME_RATE)
            .map(unpack_u64)
            .unwrap_or_else(|_| chosen.map(|m| (m.fps_n, m.fps_d)).unwrap_or((30, 1)));

        let fps_n = if fps_n_u == 0 { 30 } else { fps_n_u as i32 };
        let fps_d = if fps_d_u == 0 { 1 } else { fps_d_u as i32 };

        let default_stride: i32 = cur_mt
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .map(|u| u as i32)
            .unwrap_or_else(|_| match fmt {
                OutputFormat::Yuy2 => (raw_w as i32) * 2,
                OutputFormat::Nv12 => raw_w as i32,
                OutputFormat::Rgb32 => (raw_w as i32) * 4,
            });

        Ok((fmt, raw_w, raw_h, fps_n, fps_d, default_stride))
    }

    #[inline]
    fn rgb_to_uyvy_pair(r0: u8, g0: u8, b0: u8, r1: u8, g1: u8, b1: u8) -> [u8; 4] {
        // BT.709 video range (16..235 Y, 16..240 UV) matching src/testsrc.rs
        let y0 = (16.0 + 0.1826 * r0 as f32 + 0.6142 * g0 as f32 + 0.0620 * b0 as f32)
            .clamp(16.0, 235.0) as u8;
        let y1 = (16.0 + 0.1826 * r1 as f32 + 0.6142 * g1 as f32 + 0.0620 * b1 as f32)
            .clamp(16.0, 235.0) as u8;
        let ra = (r0 as f32 + r1 as f32) * 0.5;
        let ga = (g0 as f32 + g1 as f32) * 0.5;
        let ba = (b0 as f32 + b1 as f32) * 0.5;
        let u = (128.0 - 0.1006 * ra - 0.3386 * ga + 0.4392 * ba).clamp(16.0, 240.0) as u8;
        let v = (128.0 + 0.4392 * ra - 0.3989 * ga - 0.0403 * ba).clamp(16.0, 240.0) as u8;
        [u, y0, v, y1]
    }

    unsafe fn convert_yuy2_to_uyvy(
        scanline0: *const u8,
        pitch: isize,
        out_w: usize,
        out_h: usize,
        dst: &mut [u8],
    ) {
        let row_bytes = out_w * 2;
        for y in 0..out_h {
            let src_row = slice::from_raw_parts(scanline0.offset(y as isize * pitch), row_bytes);
            let dst_row = &mut dst[y * row_bytes..(y + 1) * row_bytes];
            for (s, d) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
                // YUY2: Y0 U0 Y1 V0 -> UYVY: U0 Y0 V0 Y1
                d[0] = s[1];
                d[1] = s[0];
                d[2] = s[3];
                d[3] = s[2];
            }
        }
    }

    unsafe fn convert_nv12_to_uyvy(
        scanline0: *const u8,
        pitch: isize,
        raw_h: usize,
        out_w: usize,
        out_h: usize,
        dst: &mut [u8],
    ) {
        let abs_pitch = pitch.unsigned_abs();
        let y_base = if pitch >= 0 {
            scanline0
        } else {
            scanline0.offset(-((raw_h.saturating_sub(1) as isize) * abs_pitch as isize))
        };
        let uv_base = y_base.add(abs_pitch * raw_h);
        let dst_stride = out_w * 2;

        for y in 0..out_h {
            let y_row = slice::from_raw_parts(y_base.add(y * abs_pitch), out_w);
            let uv_row = slice::from_raw_parts(uv_base.add((y / 2) * abs_pitch), out_w);
            let dst_row = &mut dst[y * dst_stride..(y + 1) * dst_stride];
            for x2 in 0..(out_w / 2) {
                let i = x2 * 2;
                let y0 = y_row[i];
                let y1 = y_row[i + 1];
                let u = uv_row[i];
                let v = uv_row[i + 1];
                let d = &mut dst_row[x2 * 4..x2 * 4 + 4];
                d[0] = u;
                d[1] = y0;
                d[2] = v;
                d[3] = y1;
            }
        }
    }

    unsafe fn convert_rgb32_to_uyvy(
        scanline0: *const u8,
        pitch: isize,
        out_w: usize,
        out_h: usize,
        dst: &mut [u8],
    ) {
        let src_row_bytes = out_w * 4;
        let dst_stride = out_w * 2;
        for y in 0..out_h {
            let src_row =
                slice::from_raw_parts(scanline0.offset(y as isize * pitch), src_row_bytes);
            let dst_row = &mut dst[y * dst_stride..(y + 1) * dst_stride];
            for (s, d) in src_row.chunks_exact(8).zip(dst_row.chunks_exact_mut(4)) {
                // MFVideoFormat_RGB32 in memory is B, G, R, X
                let b0 = s[0];
                let g0 = s[1];
                let r0 = s[2];
                let b1 = s[4];
                let g1 = s[5];
                let r1 = s[6];
                d.copy_from_slice(&rgb_to_uyvy_pair(r0, g0, b0, r1, g1, b1));
            }
        }
    }

    unsafe fn extract_frame_uyvy(
        buf: &IMFMediaBuffer,
        fmt: OutputFormat,
        raw_w: u32,
        raw_h: u32,
        default_stride: i32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let out_w = (raw_w & !1) as usize;
        let out_h = raw_h as usize;
        if out_w < 2 || out_h == 0 {
            return None;
        }
        let dst_stride = out_w * 2;
        let mut dst = vec![0u8; dst_stride * out_h];

        // Prefer IMF2DBuffer::Lock2D when supported (automatically gives top scanline + signed pitch)
        if let Ok(buf2d) = buf.cast::<IMF2DBuffer>() {
            let mut scanline0: *mut u8 = ptr::null_mut();
            let mut pitch: i32 = 0;
            if buf2d.Lock2D(&mut scanline0, &mut pitch).is_ok() {
                let min_row_bytes = match fmt {
                    OutputFormat::Yuy2 => out_w * 2,
                    OutputFormat::Nv12 => out_w,
                    OutputFormat::Rgb32 => out_w * 4,
                };
                let valid = !scanline0.is_null() && (pitch.unsigned_abs() as usize) >= min_row_bytes;
                if valid {
                    match fmt {
                        OutputFormat::Yuy2 => convert_yuy2_to_uyvy(
                            scanline0,
                            pitch as isize,
                            out_w,
                            out_h,
                            &mut dst,
                        ),
                        OutputFormat::Nv12 => convert_nv12_to_uyvy(
                            scanline0,
                            pitch as isize,
                            raw_h as usize,
                            out_w,
                            out_h,
                            &mut dst,
                        ),
                        OutputFormat::Rgb32 => convert_rgb32_to_uyvy(
                            scanline0,
                            pitch as isize,
                            out_w,
                            out_h,
                            &mut dst,
                        ),
                    }
                }
                let _ = buf2d.Unlock2D();
                if valid {
                    return Some((out_w as u32, out_h as u32, dst));
                }
            }
        }

        // Fallback to 1D IMFMediaBuffer::Lock using default_stride from MF_MT_DEFAULT_STRIDE
        let mut raw_ptr: *mut u8 = ptr::null_mut();
        let mut cur_len: u32 = 0;
        if buf.Lock(&mut raw_ptr, None, Some(&mut cur_len)).is_ok() {
            let pitch = if default_stride != 0 {
                default_stride as isize
            } else {
                match fmt {
                    OutputFormat::Yuy2 => (raw_w as isize) * 2,
                    OutputFormat::Nv12 => raw_w as isize,
                    OutputFormat::Rgb32 => (raw_w as isize) * 4,
                }
            };
            let abs_pitch = pitch.unsigned_abs();
            let needed = match fmt {
                OutputFormat::Yuy2 | OutputFormat::Rgb32 => abs_pitch.saturating_mul(raw_h as usize),
                OutputFormat::Nv12 => abs_pitch.saturating_mul((raw_h as usize) * 3 / 2),
            };
            let valid = !raw_ptr.is_null() && (cur_len as usize) >= needed && abs_pitch > 0;
            if valid {
                let scanline0 = if pitch >= 0 {
                    raw_ptr as *const u8
                } else {
                    // Bottom-up image: first top row is the last row in memory
                    (raw_ptr as *const u8).add(abs_pitch * (raw_h as usize).saturating_sub(1))
                };
                match fmt {
                    OutputFormat::Yuy2 => {
                        convert_yuy2_to_uyvy(scanline0, pitch, out_w, out_h, &mut dst)
                    }
                    OutputFormat::Nv12 => convert_nv12_to_uyvy(
                        raw_ptr as *const u8,
                        abs_pitch as isize,
                        raw_h as usize,
                        out_w,
                        out_h,
                        &mut dst,
                    ),
                    OutputFormat::Rgb32 => {
                        convert_rgb32_to_uyvy(scanline0, pitch, out_w, out_h, &mut dst)
                    }
                }
            }
            let _ = buf.Unlock();
            if valid {
                return Some((out_w as u32, out_h as u32, dst));
            }
        }

        None
    }

    struct WebcamCapture {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
        status: Arc<Mutex<String>>,
    }

    impl Capture for WebcamCapture {
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

    impl Drop for WebcamCapture {
        fn drop(&mut self) {
            self.stop();
        }
    }

    pub fn list_devices() -> Vec<String> {
        let _com = ComGuard::new();
        let Ok(_mf) = MfGuard::new() else {
            return Vec::new();
        };
        let Ok(list) = ActivateList::enumerate() else {
            return Vec::new();
        };
        list.as_slice()
            .iter()
            .enumerate()
            .filter_map(|(i, opt)| opt.as_ref().map(|a| ActivateList::friendly_name(a, i)))
            .collect()
    }

    pub fn start(index: usize, sink: FrameSink) -> Result<Box<dyn Capture>, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new("웹캠 초기화 중...".to_string()));
        let stop_thread = stop.clone();
        let status_thread = status.clone();

        let (init_tx, init_rx) = mpsc::channel::<Result<(), String>>();

        let thread = thread::spawn(move || {
            let _com = ComGuard::new();
            let _mf = match MfGuard::new() {
                Ok(g) => g,
                Err(e) => {
                    let _ = init_tx.send(Err(format!("웹캠을 열 수 없습니다: {e}")));
                    return;
                }
            };

            let list = match ActivateList::enumerate() {
                Ok(l) => l,
                Err(e) => {
                    let _ = init_tx.send(Err(format!("웹캠을 열 수 없습니다: {e}")));
                    return;
                }
            };

            let slice = list.as_slice();
            if index >= slice.len() {
                let _ = init_tx.send(Err(format!(
                    "웹캠을 열 수 없습니다: 장치 인덱스 {index} 없음 (감지된 장치 {}개)",
                    slice.len()
                )));
                return;
            }

            let Some(activate) = slice[index].as_ref() else {
                let _ = init_tx.send(Err("웹캠을 열 수 없습니다: 유효하지 않은 장치 객체".to_string()));
                return;
            };

            unsafe {
                let media_source: IMFMediaSource = match activate.ActivateObject() {
                    Ok(ms) => ms,
                    Err(e) => {
                        let _ = init_tx.send(Err(format!(
                            "웹캠을 열 수 없습니다 (다른 프로그램에서 사용 중일 수 있습니다): {e}"
                        )));
                        return;
                    }
                };

                let mut reader_attrs: Option<IMFAttributes> = None;
                if let Err(e) = MFCreateAttributes(&mut reader_attrs, 3) {
                    let _ = media_source.Shutdown();
                    let _ = init_tx.send(Err(format!("웹캠을 열 수 없습니다 (속성 생성 실패): {e}")));
                    return;
                }
                let Some(reader_attrs) = reader_attrs else {
                    let _ = media_source.Shutdown();
                    let _ = init_tx.send(Err("웹캠을 열 수 없습니다: 속성 객체 생성 실패".to_string()));
                    return;
                };

                let _ = reader_attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1);
                let _ = reader_attrs.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 0);
                let _ = reader_attrs.SetUINT32(&MF_LOW_LATENCY, 1);

                let reader = match MFCreateSourceReaderFromMediaSource(&media_source, &reader_attrs)
                {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = media_source.Shutdown();
                        let _ = init_tx.send(Err(format!(
                            "웹캠을 열 수 없습니다 (SourceReader 생성 실패, 장치가 사용 중일 수 있습니다): {e}"
                        )));
                        return;
                    }
                };

                let best_native = select_best_native_mode(&reader);
                let (mut fmt, mut raw_w, mut raw_h, mut fps_n, mut fps_d, mut default_stride) =
                    match configure_output_type(&reader, best_native.as_ref()) {
                        Ok(cfg) => cfg,
                        Err(e) => {
                            drop(reader);
                            let _ = media_source.Shutdown();
                            let _ = init_tx.send(Err(format!("웹캠을 열 수 없습니다: {e}")));
                            return;
                        }
                    };

                let out_w = raw_w & !1;
                if out_w < 2 || raw_h == 0 {
                    drop(reader);
                    let _ = media_source.Shutdown();
                    let _ = init_tx.send(Err(format!(
                        "웹캠을 열 수 없습니다: 잘못된 해상도 ({raw_w}x{raw_h})"
                    )));
                    return;
                }

                let fps_val = fps_n as f64 / fps_d.max(1) as f64;
                *status_thread.lock().unwrap() = format!("웹캠 {out_w}x{raw_h} {fps_val:.2}p");
                let _ = init_tx.send(Ok(()));

                while !stop_thread.load(Ordering::SeqCst) {
                    let mut stream_flags: u32 = 0;
                    let mut sample: Option<IMFSample> = None;
                    let res = reader.ReadSample(
                        STREAM_INDEX,
                        0,
                        None,
                        Some(&mut stream_flags),
                        None,
                        Some(&mut sample),
                    );
                    let tc_100ns = now_100ns();

                    if let Err(e) = res {
                        *status_thread.lock().unwrap() = format!("웹캠 읽기 오류: {e}");
                        break;
                    }

                    let flags = stream_flags as i32;
                    if (flags & MF_SOURCE_READERF_ERROR.0) != 0 {
                        *status_thread.lock().unwrap() =
                            "웹캠 스트림 오류가 발생했습니다.".to_string();
                        break;
                    }
                    if (flags & MF_SOURCE_READERF_ENDOFSTREAM.0) != 0 {
                        *status_thread.lock().unwrap() = "웹캠 스트림 종료".to_string();
                        break;
                    }
                    if (flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0) != 0 {
                        if let Ok(cur_mt) = reader.GetCurrentMediaType(STREAM_INDEX) {
                            if let Ok(sub) = cur_mt.GetGUID(&MF_MT_SUBTYPE) {
                                if sub == MFVideoFormat_YUY2 {
                                    fmt = OutputFormat::Yuy2;
                                } else if sub == MFVideoFormat_NV12 {
                                    fmt = OutputFormat::Nv12;
                                } else if sub == MFVideoFormat_RGB32 {
                                    fmt = OutputFormat::Rgb32;
                                }
                            }
                            if let Ok(sz) = cur_mt.GetUINT64(&MF_MT_FRAME_SIZE) {
                                let (w, h) = unpack_u64(sz);
                                if w >= 2 && h > 0 {
                                    raw_w = w;
                                    raw_h = h;
                                }
                            }
                            if let Ok(fr) = cur_mt.GetUINT64(&MF_MT_FRAME_RATE) {
                                let (fn_u, fd_u) = unpack_u64(fr);
                                if fn_u > 0 && fd_u > 0 {
                                    fps_n = fn_u as i32;
                                    fps_d = fd_u as i32;
                                }
                            }
                            default_stride = cur_mt
                                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                                .map(|u| u as i32)
                                .unwrap_or(default_stride);
                            let w_even = raw_w & !1;
                            let fps_v = fps_n as f64 / fps_d.max(1) as f64;
                            *status_thread.lock().unwrap() =
                                format!("웹캠 {w_even}x{raw_h} {fps_v:.2}p");
                        }
                    }

                    let Some(sample) = sample else {
                        continue;
                    };

                    let Ok(contig_buf) = sample.ConvertToContiguousBuffer() else {
                        continue;
                    };

                    if let Some((w, h, data)) =
                        extract_frame_uyvy(&contig_buf, fmt, raw_w, raw_h, default_stride)
                    {
                        let stride = (w as usize) * 2;
                        sink(Frame {
                            alpha: None,
                            width: w,
                            height: h,
                            stride,
                            data,
                            fps_n,
                            fps_d,
                            tc_100ns,
                        });
                    }
                }

                drop(reader);
                let _ = media_source.Shutdown();
            }
        });

        match init_rx.recv() {
            Ok(Ok(())) => Ok(Box::new(WebcamCapture {
                stop,
                thread: Some(thread),
                status,
            })),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err("웹캠을 열 수 없습니다: 초기화 스레드가 예기치 않게 종료되었습니다.".into())
            }
        }
    }

    unsafe fn open_file_reader(path: &str) -> Result<(IMFSourceReader, OutputFormat, u32, u32, i32, i32, i32), String> {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2).map_err(|e| format!("영상 파일을 열 수 없습니다 (속성): {e}"))?;
        let attrs = attrs.ok_or_else(|| "영상 파일을 열 수 없습니다 (속성 없음)".to_string())?;
        let _ = attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1);
        let _ = attrs.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 0);
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path), &attrs)
            .map_err(|e| format!("영상 파일을 열 수 없습니다 (지원하지 않는 형식이거나 손상된 파일): {e}"))?;
        let (fmt, w, h, fps_n, fps_d, stride) = configure_output_type(&reader, None)
            .map_err(|e| format!("영상 파일을 열 수 없습니다: {e}"))?;
        Ok((reader, fmt, w, h, fps_n, fps_d, stride))
    }

    pub fn start_file(path: &str, sink: FrameSink) -> Result<Box<dyn Capture>, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string());
        let status = Arc::new(Mutex::new(format!("{name} 여는 중...")));
        let (stop_t, status_t, path_t) = (stop.clone(), status.clone(), path.to_string());
        let (init_tx, init_rx) = mpsc::channel::<Result<(), String>>();
        let thread = thread::spawn(move || {
            let _com = ComGuard::new();
            let _mf = match MfGuard::new() {
                Ok(g) => g,
                Err(e) => {
                    let _ = init_tx.send(Err(format!("영상 파일을 열 수 없습니다: {e}")));
                    return;
                }
            };
            unsafe {
                let (mut reader, mut fmt, mut raw_w, mut raw_h, fps_n, fps_d, mut stride) = match open_file_reader(&path_t) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };
                *status_t.lock().unwrap() = format!("{name} {}x{raw_h} {:.2}p", raw_w & !1, fps_n as f64 / fps_d.max(1) as f64);
                let _ = init_tx.send(Ok(()));
                let frame_100ns = (10_000_000i64 * fps_d as i64 / fps_n.max(1) as i64).max(1);
                let mut clock = std::time::Instant::now();
                let mut first_ts: Option<i64> = None;
                let mut loop_offset: i64 = 0;
                let mut last_ts: i64 = 0;
                while !stop_t.load(Ordering::SeqCst) {
                    let mut flags: u32 = 0;
                    let mut ts: i64 = 0;
                    let mut sample: Option<IMFSample> = None;
                    if let Err(e) = reader.ReadSample(STREAM_INDEX, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample)) {
                        *status_t.lock().unwrap() = format!("영상 읽기 오류: {e}");
                        break;
                    }
                    let flags = flags as i32;
                    if (flags & MF_SOURCE_READERF_ERROR.0) != 0 {
                        *status_t.lock().unwrap() = "영상 디코딩 오류".into();
                        break;
                    }
                    if (flags & MF_SOURCE_READERF_ENDOFSTREAM.0) != 0 {
                        drop(reader);
                        match open_file_reader(&path_t) {
                            Ok(v) => {
                                (reader, fmt, raw_w, raw_h, _, _, stride) = v;
                                loop_offset = last_ts + frame_100ns;
                                first_ts = first_ts.or(Some(0));
                                continue;
                            }
                            Err(e) => {
                                *status_t.lock().unwrap() = e;
                                break;
                            }
                        }
                    }
                    if (flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0) != 0 {
                        if let Ok(mt) = reader.GetCurrentMediaType(STREAM_INDEX) {
                            if let Ok(sz) = mt.GetUINT64(&MF_MT_FRAME_SIZE) {
                                let (w2, h2) = unpack_u64(sz);
                                if w2 >= 2 && h2 > 0 {
                                    raw_w = w2;
                                    raw_h = h2;
                                }
                            }
                            stride = mt.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|u| u as i32).unwrap_or(stride);
                        }
                    }
                    let Some(sample) = sample else { continue };
                    let t = ts + loop_offset;
                    let base = *first_ts.get_or_insert_with(|| {
                        clock = std::time::Instant::now();
                        t
                    });
                    last_ts = t;
                    let due = clock + std::time::Duration::from_nanos(((t - base).max(0) as u64) * 100);
                    let now = std::time::Instant::now();
                    if due > now {
                        std::thread::sleep(due - now);
                    }
                    let Ok(buf) = sample.ConvertToContiguousBuffer() else { continue };
                    if let Some((w2, h2, data)) = extract_frame_uyvy(&buf, fmt, raw_w, raw_h, stride) {
                        sink(Frame { width: w2, height: h2, stride: w2 as usize * 2, data, alpha: None, fps_n, fps_d, tc_100ns: now_100ns() });
                    }
                }
            }
        });
        match init_rx.recv() {
            Ok(Ok(())) => Ok(Box::new(WebcamCapture { stop, thread: Some(thread), status })),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err("영상 파일을 열 수 없습니다: 초기화 스레드 종료".into())
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::*;
    pub fn list_devices() -> Vec<String> {
        Vec::new()
    }
    pub fn start(_index: usize, _sink: FrameSink) -> Result<Box<dyn Capture>, String> {
        Err("Media Foundation 웹캠 캡처는 Windows에서만 지원됩니다.".into())
    }
    pub fn start_file(_path: &str, _sink: FrameSink) -> Result<Box<dyn Capture>, String> {
        Err("Media Foundation 파일 재생은 Windows에서만 지원됩니다.".into())
    }
}

pub use imp::{list_devices, start, start_file};
