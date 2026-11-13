use open_ndinb::ndi::{self, Finder, Receiver, FOURCC_BGRA};
use open_ndinb::source::now_100ns;
use std::time::{Duration, Instant};

fn stats(v: &mut Vec<f64>) -> serde_json::Value {
    if v.is_empty() {
        return serde_json::Value::Null;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    serde_json::json!({ "n": v.len(), "min": v[0], "p50": p(0.5), "p95": p(0.95), "max": v[v.len() - 1], "mean": v.iter().sum::<f64>() / v.len() as f64 })
}

fn save_png(path: &str, w: u32, h: u32, stride: usize, bgra: &[u8]) {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let o = y * stride + x * 4;
            rgba.extend_from_slice(&[bgra[o + 2], bgra[o + 1], bgra[o], bgra[o + 3]]);
        }
    }
    if let Ok(f) = std::fs::File::create(path) {
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        if let Ok(mut wr) = e.write_header() {
            let _ = wr.write_image_data(&rgba);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let secs: f64 = get("--seconds").and_then(|v| v.parse().ok()).unwrap_or(10.0);
    let filter = get("--filter").unwrap_or_else(|| "OPEN-NDINB".into());
    let out = get("--out").unwrap_or_else(|| "ndi-probe.json".into());
    let png_dir = get("--png-dir");

    let lib = match ndi::get() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let finder = Finder::new().expect("finder");
    let t0 = Instant::now();
    let mut found = Vec::new();
    while t0.elapsed() < Duration::from_secs(15) {
        finder.wait(1000);
        found = finder.sources().into_iter().filter(|(n, _)| n.contains(&filter)).collect();
        if found.len() >= 2 {
            break;
        }
    }
    let all: Vec<String> = finder.sources().into_iter().map(|(n, _)| n).collect();
    println!("sources: {all:?}");

    let mut results = Vec::new();
    let handles: Vec<_> = found
        .iter()
        .cloned()
        .map(|(name, url)| {
            let png_dir = png_dir.clone();
            std::thread::spawn(move || {
                let rx = Receiver::new(&name, &url).expect("receiver");
                let start = Instant::now();
                let mut lat = Vec::new();
                let mut frames = 0u64;
                let mut first_frame_ms = None;
                let (mut a0, mut a255, mut amid, mut px) = (0u64, 0u64, 0u64, 0u64);
                let mut fmt = String::new();
                let mut saved = false;
                while start.elapsed().as_secs_f64() < secs {
                    if let Some(f) = rx.capture(500) {
                        let now = now_100ns();
                        let fr = &f.frame;
                        frames += 1;
                        if first_frame_ms.is_none() {
                            first_frame_ms = Some(start.elapsed().as_millis() as u64);
                        }
                        if frames > 5 {
                            lat.push((now - fr.timecode) as f64 / 10_000.0);
                        }
                        let fc = fr.fourcc.to_le_bytes();
                        fmt = format!("{}x{} {} {}/{}", fr.xres, fr.yres, String::from_utf8_lossy(&fc), fr.frame_rate_n, fr.frame_rate_d);
                        if fr.fourcc == FOURCC_BGRA && frames % 10 == 0 {
                            let b = f.bytes();
                            let stride = fr.line_stride_in_bytes as usize;
                            for y in (0..fr.yres as usize).step_by(4) {
                                for x in (0..fr.xres as usize).step_by(4) {
                                    let a = b[y * stride + x * 4 + 3];
                                    px += 1;
                                    match a {
                                        0..=7 => a0 += 1,
                                        248..=255 => a255 += 1,
                                        _ => amid += 1,
                                    }
                                }
                            }
                        }
                        if !saved && frames == 60 {
                            if let Some(d) = &png_dir {
                                let safe: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
                                save_png(&format!("{d}/{safe}.png"), fr.xres as u32, fr.yres as u32, fr.line_stride_in_bytes as usize, f.bytes());
                            }
                            saved = true;
                        }
                    }
                }
                let el = start.elapsed().as_secs_f64();
                serde_json::json!({
                    "name": name,
                    "format": fmt,
                    "frames": frames,
                    "fps": frames as f64 / el,
                    "firstFrameMs": first_frame_ms,
                    "latencyMsCaptureToReceive": stats(&mut lat),
                    "alphaSampled": px,
                    "alphaTransparentPct": if px > 0 { a0 as f64 * 100.0 / px as f64 } else { 0.0 },
                    "alphaOpaquePct": if px > 0 { a255 as f64 * 100.0 / px as f64 } else { 0.0 },
                    "alphaEdgePct": if px > 0 { amid as f64 * 100.0 / px as f64 } else { 0.0 },
                })
            })
        })
        .collect();
    for h in handles {
        results.push(h.join().unwrap());
    }
    let j = serde_json::json!({ "ndiRuntime": lib.dll_path, "allSources": all, "matched": results.len(), "receivers": results });
    let s = serde_json::to_string_pretty(&j).unwrap();
    println!("{s}");
    let _ = std::fs::write(&out, s);
    std::process::exit(if results.len() >= 2 { 0 } else { 1 });
}
