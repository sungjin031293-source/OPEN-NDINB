#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use open_ndinb::gpu::{self, GpuState};
use open_ndinb::pipeline::{host_name, Pipeline, Preview, SourceKind};
use open_ndinb::runtime_check::{self, Item};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BASE_NAME: &str = "OPEN-NDINB";
const LOGO_PNG: &[u8] = include_bytes!("../assets/on-256.png");
const BG: Color32 = Color32::from_rgb(0xFA, 0xFA, 0xFB);
const CARD: Color32 = Color32::WHITE;
const SOFT: Color32 = Color32::from_rgb(0xF1, 0xF3, 0xF6);
const LINE: Color32 = Color32::from_rgb(0xE4, 0xE7, 0xEC);
const BLUE: Color32 = Color32::from_rgb(0x00, 0x52, 0xFF);
const BLUE_TINT: Color32 = Color32::from_rgb(0xE8, 0xEF, 0xFF);
const RED: Color32 = Color32::from_rgb(0xF0, 0x2D, 0x3A);
const GREEN: Color32 = Color32::from_rgb(0x05, 0xA6, 0x5A);
const DOT_GREEN: Color32 = Color32::from_rgb(0x1D, 0xC4, 0x5E);
const TEXT: Color32 = Color32::from_rgb(0x0A, 0x0B, 0x0D);
const MUTED: Color32 = Color32::from_rgb(0x7C, 0x83, 0x90);

#[derive(Default)]
struct Args {
    source: Option<SourceKind>,
    autostart: bool,
    mode: u32,
    screenshot: Option<String>,
    screenshot_after: f32,
    popup_screenshot: Option<String>,
    exit_after: Option<f32>,
    stats_json: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args { screenshot_after: 5.0, ..Default::default() };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--source" => {
                let v = it.next().unwrap_or_default();
                a.source = Some(if v == "pattern" {
                    SourceKind::Pattern
                } else if let Some(p) = v.strip_prefix("file:") {
                    SourceKind::File(p.to_string())
                } else if let Some(i) = v.strip_prefix("decklink:") {
                    SourceKind::DeckLink(i.parse().unwrap_or(0))
                } else if let Some(i) = v.strip_prefix("webcam:") {
                    SourceKind::Webcam(i.parse().unwrap_or(0))
                } else {
                    SourceKind::File(v)
                });
            }
            "--start" => a.autostart = true,
            "--fast" => a.mode = 1,
            "--screenshot" => a.screenshot = it.next(),
            "--screenshot-after" => a.screenshot_after = it.next().and_then(|v| v.parse().ok()).unwrap_or(5.0),
            "--popup-screenshot" => a.popup_screenshot = it.next(),
            "--exit-after" => a.exit_after = it.next().and_then(|v| v.parse().ok()),
            "--stats-json" => a.stats_json = it.next(),
            _ => {}
        }
    }
    a
}

fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}
fn fb(size: f32) -> FontId {
    FontId::new(size, bold())
}
fn fr(size: f32) -> FontId {
    FontId::proportional(size)
}

fn shadow() -> egui::epaint::Shadow {
    egui::epaint::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(16) }
}

fn card(p: &egui::Painter, r: Rect) {
    p.add(shadow().as_shape(r, CornerRadius::same(22)));
    p.rect_filled(r, 22.0, CARD);
}

struct App {
    decklinks: Vec<String>,
    webcams: Vec<String>,
    source: SourceKind,
    file_path: String,
    fast: bool,
    pipeline: Option<Pipeline>,
    error: Option<String>,
    tex_orig: Option<egui::TextureHandle>,
    tex_matte: Option<egui::TextureHandle>,
    tex_checker: Option<egui::TextureHandle>,
    tex_logo: Option<egui::TextureHandle>,
    toast: Option<(String, Instant)>,
    serial_orig: u64,
    serial_matte: u64,
    started: Instant,
    args: Args,
    shot_requested: bool,
    popup_shot_requested: bool,
    fps_window: (Instant, u64, u64, u64),
    fps: (f64, f64, f64),
    checks: Vec<Item>,
    show_checks: bool,
    gpu_name: String,
    gpu: Arc<Mutex<GpuState>>,
    source_open: bool,
    picking: Option<Arc<Mutex<Option<Option<std::path::PathBuf>>>>>,
    checks_opened: Option<Instant>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, args: Args) -> Self {
        install_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_theme(egui::ThemePreference::Light);
        cc.egui_ctx.global_style_mut(|s| {
            s.visuals.panel_fill = BG;
            s.visuals.window_fill = CARD;
            s.visuals.override_text_color = Some(TEXT);
            s.visuals.selection.bg_fill = BLUE_TINT;
            s.visuals.extreme_bg_color = CARD;
            s.visuals.window_corner_radius = CornerRadius::same(22);
            s.visuals.window_stroke = Stroke::NONE;
            s.visuals.window_shadow = shadow();
            s.visuals.widgets.inactive.corner_radius = CornerRadius::same(14);
            s.visuals.widgets.hovered.corner_radius = CornerRadius::same(14);
            s.visuals.widgets.active.corner_radius = CornerRadius::same(14);
            s.visuals.widgets.inactive.weak_bg_fill = SOFT;
            s.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
            s.visuals.widgets.hovered.weak_bg_fill = LINE;
            s.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, BLUE);
            s.text_styles.insert(egui::TextStyle::Body, fr(16.0));
            s.text_styles.insert(egui::TextStyle::Button, fr(16.0));
        });
        let decklinks = open_ndinb::decklink::list_devices();
        let webcams = open_ndinb::webcam::list_devices();
        let source = args.source.clone().unwrap_or(if !decklinks.is_empty() {
            SourceKind::DeckLink(0)
        } else if !webcams.is_empty() {
            SourceKind::Webcam(0)
        } else {
            SourceKind::File(String::new())
        });
        let file_path = match &source {
            SourceKind::File(p) if !p.is_empty() => p.clone(),
            _ => load_last_file(),
        };
        let checks = runtime_check::check_all();
        let show_checks = checks.iter().any(|c| !c.ok && c.required);
        let gpu_name = runtime_check::gpu_name().unwrap_or_else(|| "NVIDIA GPU 없음".into()).replace("NVIDIA GeForce ", "").replace("NVIDIA ", "");
        let mut app = App {
            decklinks,
            webcams,
            source,
            file_path,
            fast: args.mode == 1,
            pipeline: None,
            error: None,
            tex_orig: None,
            tex_matte: None,
            tex_checker: None,
            tex_logo: decode_png(LOGO_PNG).map(|img| cc.egui_ctx.load_texture("logo", img, egui::TextureOptions::LINEAR)),
            toast: None,
            serial_orig: 0,
            serial_matte: 0,
            started: Instant::now(),
            shot_requested: false,
            popup_shot_requested: false,
            fps_window: (Instant::now(), 0, 0, 0),
            fps: (0.0, 0.0, 0.0),
            checks,
            show_checks,
            gpu_name,
            gpu: gpu::start_monitor(),
            source_open: false,
            picking: None,
            checks_opened: None,
            args,
        };
        if app.args.autostart {
            app.start();
        }
        app
    }

    fn start(&mut self) {
        self.error = None;
        let kind = match &self.source {
            SourceKind::File(_) => SourceKind::File(self.file_path.clone()),
            k => k.clone(),
        };
        match Pipeline::start(&kind, if self.fast { 1 } else { 0 }, BASE_NAME) {
            Ok(p) => {
                self.pipeline = Some(p);
                self.fps_window = (Instant::now(), 0, 0, 0);
            }
            Err(e) => {
                self.toast = Some((e.clone(), Instant::now()));
                self.error = Some(e);
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut p) = self.pipeline.take() {
            p.shutdown();
        }
        self.tex_orig = None;
        self.tex_matte = None;
        self.fps = (0.0, 0.0, 0.0);
    }

    fn write_stats(&self) {
        let (Some(path), Some(p)) = (&self.args.stats_json, &self.pipeline) else { return };
        let s = &p.shared.stats;
        let g = self.gpu.lock().unwrap();
        let j = serde_json::json!({
            "host": host_name(),
            "source": format!("{:?}", self.source),
            "format": p.shared.fmt.lock().unwrap().clone(),
            "ndiNames": [format!("{} ({})", host_name(), p.names.0), format!("{} ({})", host_name(), p.names.1)],
            "framesIn": s.frames_in.load(Ordering::Relaxed),
            "framesOriginalSent": s.frames_orig.load(Ordering::Relaxed),
            "framesAlphaSent": s.frames_matte.load(Ordering::Relaxed),
            "fps": { "in": self.fps.0, "original": self.fps.1, "alpha": self.fps.2 },
            "mattingMsLast": s.matte_us_last.load(Ordering::Relaxed) as f64 / 1000.0,
            "captureToSendMsOriginalLast": s.orig_send_lat_us.load(Ordering::Relaxed) as f64 / 1000.0,
            "captureToSendMsAlphaLast": s.matte_send_lat_us.load(Ordering::Relaxed) as f64 / 1000.0,
            "connections": { "original": s.conn_orig.load(Ordering::Relaxed), "alpha": s.conn_matte.load(Ordering::Relaxed) },
            "gpuUtilAvailable": g.available,
            "gpuUtilLast": g.last,
            "gpuUtilSamples": g.util.len(),
            "webcams": self.webcams,
            "decklinks": self.decklinks,
            "runtimeChecks": self.checks.iter().map(|c| serde_json::json!({"key": c.key, "ok": c.ok, "required": c.required, "url": c.url})).collect::<Vec<_>>(),
            "error": p.shared.error.lock().unwrap().clone().or(self.error.clone()),
        });
        let _ = std::fs::write(path, serde_json::to_string_pretty(&j).unwrap());
    }

    fn update_textures(&mut self, ctx: &egui::Context) {
        if self.tex_checker.is_none() {
            let n = 64;
            let mut rgba = vec![0u8; n * n * 4];
            for y in 0..n {
                for x in 0..n {
                    let v = if ((x / 8) + (y / 8)) % 2 == 0 { 255 } else { 226 };
                    let o = (y * n + x) * 4;
                    rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([n, n], &rgba);
            self.tex_checker = Some(ctx.load_texture("checker", img, egui::TextureOptions { magnification: egui::TextureFilter::Nearest, wrap_mode: egui::TextureWrapMode::Repeat, ..Default::default() }));
        }
        let Some(p) = &self.pipeline else { return };
        let up = |pv: &Preview, serial: &mut u64, tex: &mut Option<egui::TextureHandle>, name: &str| {
            if pv.serial != *serial && pv.size[0] > 0 {
                *serial = pv.serial;
                let img = egui::ColorImage::from_rgba_unmultiplied(pv.size, &pv.rgba);
                match tex {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => *tex = Some(ctx.load_texture(name, img, egui::TextureOptions::LINEAR)),
                }
            }
        };
        up(&p.shared.preview_orig.lock().unwrap(), &mut self.serial_orig, &mut self.tex_orig, "orig");
        up(&p.shared.preview_matte.lock().unwrap(), &mut self.serial_matte, &mut self.tex_matte, "matte");
        let s = &p.shared.stats;
        let el = self.fps_window.0.elapsed().as_secs_f64();
        if el >= 1.0 {
            let (i, o, m) = (s.frames_in.load(Ordering::Relaxed), s.frames_orig.load(Ordering::Relaxed), s.frames_matte.load(Ordering::Relaxed));
            self.fps = ((i - self.fps_window.1) as f64 / el, (o - self.fps_window.2) as f64 / el, (m - self.fps_window.3) as f64 / el);
            self.fps_window = (Instant::now(), i, o, m);
        }
    }

    fn label_of(&self, k: &SourceKind) -> String {
        match k {
            SourceKind::DeckLink(i) => self.decklinks.get(*i).cloned().unwrap_or_else(|| format!("DeckLink {i}")),
            SourceKind::Webcam(i) => self.webcams.get(*i).cloned().unwrap_or_else(|| format!("웹캠 {i}")),
            SourceKind::Pattern => "테스트 패턴".into(),
            SourceKind::File(_) => match std::path::Path::new(&self.file_path).file_name() {
                Some(n) if !self.file_path.trim().is_empty() => n.to_string_lossy().into_owned(),
                _ => "영상 파일 선택…".into(),
            },
        }
    }

    fn pick_file(&mut self) {
        if self.picking.is_some() {
            return;
        }
        let start_dir = std::path::Path::new(&self.file_path).parent().filter(|d| d.is_dir()).map(|d| d.to_path_buf());
        let slot: Arc<Mutex<Option<Option<std::path::PathBuf>>>> = Arc::new(Mutex::new(None));
        let out = slot.clone();
        std::thread::spawn(move || {
            let mut dlg = rfd::FileDialog::new()
                .set_title("송출할 영상 파일 선택")
                .add_filter("영상 파일", &["mp4", "mov", "m4v", "mkv", "avi", "wmv", "webm", "mts", "m2ts", "ts"])
                .add_filter("모든 파일", &["*"]);
            if let Some(dir) = start_dir {
                dlg = dlg.set_directory(dir);
            }
            *out.lock().unwrap() = Some(dlg.pick_file());
        });
        self.picking = Some(slot);
    }

    fn poll_pick(&mut self) {
        let Some(slot) = &self.picking else { return };
        let done = slot.lock().unwrap().take();
        if let Some(result) = done {
            self.picking = None;
            if let Some(p) = result {
                self.file_path = p.to_string_lossy().into_owned();
                self.source = SourceKind::File(self.file_path.clone());
                save_last_file(&self.file_path);
            }
        }
    }

    fn sources(&self) -> Vec<SourceKind> {
        let mut v: Vec<SourceKind> = (0..self.decklinks.len()).map(SourceKind::DeckLink).collect();
        v.extend((0..self.webcams.len()).map(SourceKind::Webcam));
        v.push(SourceKind::File(String::new()));
        v
    }

    fn is_selected(&self, k: &SourceKind) -> bool {
        matches!((&self.source, k), (SourceKind::File(_), SourceKind::File(_))) || &self.source == k
    }

    fn header(&mut self, ui: &mut egui::Ui, r: Rect) {
        let p = ui.painter();
        let logo = Rect::from_center_size(pos2(r.left() + 40.0, r.center().y), Vec2::splat(64.0));
        p.add(shadow().as_shape(logo, CornerRadius::same(32)));
        p.circle_filled(logo.center(), 32.0, CARD);
        if let Some(t) = &self.tex_logo {
            let sq = Rect::from_center_size(logo.center(), Vec2::splat(40.0));
            egui::Image::new(t).paint_at(ui, sq);
        }
        p.text(pos2(logo.right() + 16.0, r.center().y), Align2::LEFT_CENTER, BASE_NAME, fb(34.0), TEXT);

        let missing_required = self.checks.iter().filter(|c| !c.ok && c.required).count();
        let missing_any = self.checks.iter().filter(|c| !c.ok).count();
        let label = "드라이버";
        let galley = p.layout_no_wrap(label.to_string(), fb(19.0), TEXT);
        let w = galley.size().x + 64.0;
        let chip = Rect::from_min_size(pos2(r.right() - 10.0 - w, r.center().y - 23.0), vec2(w, 46.0));
        let resp = ui.interact(chip, ui.scope_id().with("drivers"), Sense::click());
        ui.painter().rect_filled(chip, 23.0, if resp.hovered() { LINE } else { SOFT });
        ui.painter().galley(pos2(chip.left() + 21.0, chip.center().y - galley.size().y / 2.0), galley, TEXT);
        let dot = if missing_required > 0 { RED } else if missing_any > 0 { Color32::from_rgb(0xF5, 0xA5, 0x24) } else { DOT_GREEN };
        ui.painter().circle_filled(pos2(chip.right() - 24.0, chip.center().y), 7.0, dot);
        let hover = if missing_any == 0 { "필요한 드라이버·런타임 모두 설치됨".to_string() } else { format!("{missing_any}개 설치 안 됨 · 눌러서 확인") };
        if resp.on_hover_text(hover).clicked() {
            self.checks = runtime_check::check_all();
            self.show_checks = true;
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, col: Rect) {
        let running = self.pipeline.is_some();
        let gap = 18.0;
        let src_h = 345.0;
        let matte_h = 185.0;
        let src = Rect::from_min_size(col.min, vec2(col.width(), src_h));
        let mat = Rect::from_min_size(pos2(col.left(), src.bottom() + gap), vec2(col.width(), matte_h));
        let ndi_r = Rect::from_min_max(pos2(col.left(), mat.bottom() + gap), col.max);
        {
            let p = ui.painter();
            card(p, src);
            card(p, mat);
            card(p, ndi_r);
        }

        let p = ui.painter().clone();
        p.text(src.left_top() + vec2(20.0, 38.0), Align2::LEFT_CENTER, "입력 소스", fb(25.0), TEXT);
        let dd = Rect::from_min_size(src.left_top() + vec2(16.0, 72.0), vec2(src.width() - 32.0, 50.0));
        let dresp = ui.interact(dd, ui.scope_id().with("dd"), Sense::click());
        p.rect(dd, 12.0, CARD, Stroke::new(1.5, if dresp.hovered() { BLUE } else { LINE }), StrokeKind::Inside);
        let cur = self.label_of(&self.source);
        let cur_txt = match &self.source {
            SourceKind::Webcam(_) => format!("{cur} (웹캠)"),
            _ => cur,
        };
        p.text(dd.left_center() + vec2(18.0, 0.0), Align2::LEFT_CENTER, icon_of(&self.source), fr(18.0), TEXT);
        p.with_clip_rect(dd.shrink2(vec2(48.0, 0.0))).text(dd.left_center() + vec2(52.0, 0.0), Align2::LEFT_CENTER, &cur_txt, fr(18.0), TEXT);
        chevron(&p, dd.right_center() - vec2(24.0, 0.0), if self.source_open { -1.0 } else { 1.0 });
        if dresp.clicked() && !running {
            self.source_open = !self.source_open;
            if self.source_open {
                self.decklinks = open_ndinb::decklink::list_devices();
                self.webcams = open_ndinb::webcam::list_devices();
            }
        }

        let list = Rect::from_min_size(pos2(dd.left(), dd.bottom() + 18.0), vec2(dd.width(), 150.0));
        p.rect(list, 12.0, CARD, Stroke::new(1.0, LINE), StrokeKind::Inside);
        let sources = self.sources();
        let row_h = ((list.height() - 8.0) / sources.len().max(3) as f32).min(47.0);
        let mut y = list.top() + 4.0;
        for k in sources.iter() {
            let row = Rect::from_min_size(pos2(list.left() + 2.0, y), vec2(list.width() - 4.0, row_h));
            let resp = ui.interact(row, ui.scope_id().with(("src", format!("{k:?}"))), Sense::click());
            let sel = self.is_selected(k);
            if sel {
                p.rect_filled(row, 10.0, BLUE_TINT);
            } else if resp.hovered() && !running {
                p.rect_filled(row, 10.0, SOFT);
            }
            let col_t = if sel { BLUE } else { TEXT };
            p.text(row.left_center() + vec2(18.0, 0.0), Align2::LEFT_CENTER, icon_of(k), fr(18.0), col_t);
            p.with_clip_rect(row.shrink2(vec2(8.0, 0.0))).text(row.left_center() + vec2(52.0, 0.0), Align2::LEFT_CENTER, self.label_of(k), if sel { fb(18.0) } else { fr(18.0) }, col_t);
            if resp.clicked() && !running {
                if matches!(k, SourceKind::File(_)) {
                    self.source = SourceKind::File(self.file_path.clone());
                    self.pick_file();
                } else {
                    self.source = k.clone();
                }
                self.source_open = false;
            }
            y += row_h;
        }
        if self.decklinks.is_empty() && self.webcams.is_empty() {
            p.text(pos2(list.left() + 18.0, y + 14.0), Align2::LEFT_CENTER, "연결된 DeckLink·웹캠 없음", fr(14.0), MUTED);
        }
        let caption_y = list.bottom() + 24.0;
        if matches!(self.source, SourceKind::File(_)) && !running {
            let btn = Rect::from_min_size(pos2(list.right() - 118.0, caption_y - 16.0), vec2(118.0, 32.0));
            let bresp = ui.interact(btn, ui.scope_id().with("pick"), Sense::click());
            p.rect_filled(btn, 16.0, if bresp.hovered() { BLUE } else { BLUE_TINT });
            let btxt = if self.picking.is_some() { "선택 중…" } else if self.file_path.trim().is_empty() { "파일 선택" } else { "다른 파일" };
            p.text(btn.center(), Align2::CENTER_CENTER, btxt, fb(15.0), if bresp.hovered() { Color32::WHITE } else { BLUE });
            if bresp.clicked() {
                self.pick_file();
            }
            let cap = if self.file_path.trim().is_empty() { "mp4·mov·mkv, 반복 재생".to_string() } else { "선택한 영상 반복 재생".to_string() };
            p.text(pos2(src.left() + 20.0, caption_y), Align2::LEFT_CENTER, cap, fr(15.0), MUTED);
        } else {
            let fmt = self.pipeline.as_ref().map(|p| p.shared.fmt.lock().unwrap().clone()).unwrap_or_default();
            let caption = if fmt.is_empty() { "해상도 · fps 자동 감지".to_string() } else { format!("{} 자동 감지", fmt.replace('x', "×").replace(' ', " · ").replace("fps", "fps")) };
            p.text(pos2(src.left() + 20.0, caption_y), Align2::LEFT_CENTER, caption, fr(17.0), MUTED);
        }

        p.text(mat.left_top() + vec2(20.0, 40.0), Align2::LEFT_CENTER, "누끼", fb(25.0), TEXT);
        let seg = Rect::from_min_size(mat.left_top() + vec2(18.0, 70.0), vec2(mat.width() - 36.0, 48.0));
        p.rect_filled(seg, 24.0, SOFT);
        for (i, label) in ["품질", "저지연"].iter().enumerate() {
            let r = Rect::from_min_size(pos2(seg.left() + i as f32 * seg.width() / 2.0, seg.top()), vec2(seg.width() / 2.0, seg.height()));
            let sel = (i == 1) == self.fast;
            if sel {
                p.rect_filled(r, 24.0, BLUE);
            }
            p.text(r.center(), Align2::CENTER_CENTER, *label, fb(19.0), if sel { Color32::WHITE } else { TEXT });
            if ui.interact(r, ui.scope_id().with(("seg", i)), Sense::click()).clicked() && !running {
                self.fast = i == 1;
            }
        }
        let tg = Rect::from_min_size(pos2(mat.right() - 82.0, seg.bottom() + 16.0), vec2(62.0, 34.0));
        let tresp = ui.interact(tg, ui.scope_id().with("tg"), Sense::click());
        if tresp.clicked() && !running {
            self.fast = !self.fast;
        }
        p.rect_filled(tg, 17.0, if self.fast { BLUE } else { LINE });
        let knob_x = if self.fast { tg.right() - 17.0 } else { tg.left() + 17.0 };
        p.circle_filled(pos2(knob_x, tg.center().y), 13.0, Color32::WHITE);
        p.text(pos2(mat.left() + 20.0, tg.center().y), Align2::LEFT_CENTER, if self.fast { "저지연 모드 켜짐" } else { "고품질 모델" }, fr(15.0), MUTED);

        p.text(ndi_r.left_top() + vec2(20.0, 40.0), Align2::LEFT_CENTER, "NDI 출력", fb(25.0), TEXT);
        let (ca, co) = self
            .pipeline
            .as_ref()
            .map(|p| (p.shared.stats.conn_matte.load(Ordering::Relaxed), p.shared.stats.conn_orig.load(Ordering::Relaxed)))
            .unwrap_or((0, 0));
        let host = host_name();
        for (i, (name, conns)) in [(format!("{BASE_NAME} - Alpha"), ca), (format!("{BASE_NAME} - Original"), co)].into_iter().enumerate() {
            let row = Rect::from_min_size(pos2(ndi_r.left() + 14.0, ndi_r.top() + 72.0 + i as f32 * 56.0), vec2(ndi_r.width() - 28.0, 46.0));
            p.rect(row, 12.0, CARD, Stroke::new(1.0, LINE), StrokeKind::Inside);
            p.circle_filled(row.left_center() + vec2(20.0, 0.0), 7.5, if running { BLUE } else { LINE });
            p.text(row.left_center() + vec2(38.0, 0.0), Align2::LEFT_CENTER, &name, fr(17.0), TEXT);
            let copy = Rect::from_center_size(row.right_center() - vec2(22.0, 0.0), Vec2::splat(28.0));
            p.rect_stroke(Rect::from_center_size(copy.center() + vec2(2.0, 2.0), vec2(12.0, 14.0)), 2.0, Stroke::new(1.6, TEXT), StrokeKind::Inside);
            p.rect_filled(Rect::from_center_size(copy.center() + vec2(-2.0, -2.0), vec2(12.0, 14.0)), 2.0, CARD);
            p.rect_stroke(Rect::from_center_size(copy.center() + vec2(-2.0, -2.0), vec2(12.0, 14.0)), 2.0, Stroke::new(1.6, TEXT), StrokeKind::Inside);
            p.line_segment([pos2(copy.left() - 10.0, row.top() + 12.0), pos2(copy.left() - 10.0, row.bottom() - 12.0)], Stroke::new(1.0, LINE));
            p.text(pos2(copy.left() - 22.0, row.center().y), Align2::RIGHT_CENTER, format!("수신 {conns}"), fr(16.0), MUTED);
            if ui.interact(copy, ui.scope_id().with(("copy", i)), Sense::click()).on_hover_text("NDI 이름 복사").clicked() {
                ui.ctx().copy_text(format!("{host} ({name})"));
            }
        }

        let btn = Rect::from_min_size(pos2(ndi_r.left() + 14.0, ndi_r.bottom() - 90.0), vec2(ndi_r.width() - 28.0, 72.0));
        let bresp = ui.interact(btn, ui.scope_id().with("go"), Sense::click());
        let fill = if running { RED } else { BLUE };
        p.rect_filled(btn, 18.0, if bresp.hovered() { fill.gamma_multiply(0.9) } else { fill });
        let (txt, dot) = if running { ("송출 중지", false) } else { ("송출 시작", true) };
        let galley = p.layout_no_wrap(txt.to_string(), fb(29.0), Color32::WHITE);
        let total = galley.size().x + 40.0;
        let start = btn.center().x - total / 2.0;
        if dot {
            p.circle_filled(pos2(start + 11.0, btn.center().y), 10.0, Color32::WHITE);
        } else {
            p.rect_filled(Rect::from_center_size(pos2(start + 11.0, btn.center().y), Vec2::splat(18.0)), 2.0, Color32::WHITE);
        }
        p.galley(pos2(start + 40.0, btn.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
        if bresp.clicked() {
            if running {
                self.stop()
            } else {
                self.start()
            }
        }

    }

    fn preview(&self, ui: &egui::Ui, r: Rect, title: &str, tex: &Option<egui::TextureHandle>, checker: bool, fps: f64) {
        let p = ui.painter();
        card(p, r);
        p.text(r.left_top() + vec2(22.0, 40.0), Align2::LEFT_CENTER, title, fb(27.0), TEXT);
        let img = Rect::from_min_max(r.left_top() + vec2(14.0, 72.0), r.right_bottom() - vec2(14.0, 14.0));
        p.rect_filled(img, 14.0, SOFT);
        if checker {
            if let Some(c) = &self.tex_checker {
                let uv = Rect::from_min_size(Pos2::ZERO, vec2(img.width() / 64.0, img.height() / 64.0));
                egui::Image::new(c).uv(uv).corner_radius(14).paint_at(ui, img);
            }
        }
        match tex {
            Some(t) => {
                let ts = t.size_vec2();
                let scale = (img.width() / ts.x).max(img.height() / ts.y);
                let shown = ts * scale;
                let uv = Rect::from_center_size(pos2(0.5, 0.5), vec2((img.width() / shown.x).min(1.0), (img.height() / shown.y).min(1.0)));
                egui::Image::new(t).uv(uv).corner_radius(14).paint_at(ui, img);
            }
            None => {
                p.text(img.center(), Align2::CENTER_CENTER, "송출을 시작하면 화면이 나옵니다", fr(17.0), MUTED);
            }
        }
        if self.pipeline.is_some() {
            let live = Rect::from_min_size(img.left_top() + vec2(12.0, 12.0), vec2(92.0, 36.0));
            p.rect_filled(live, 9.0, RED);
            p.circle_filled(live.left_center() + vec2(18.0, 0.0), 6.5, Color32::WHITE);
            p.text(live.left_center() + vec2(32.0, 0.0), Align2::LEFT_CENTER, "LIVE", fb(20.0), Color32::WHITE);
            let t = format!("{fps:.2} fps");
            let g = p.layout_no_wrap(t, fr(19.0), TEXT);
            let fr_r = Rect::from_min_size(pos2(img.right() - 12.0 - g.size().x - 24.0, img.top() + 12.0), vec2(g.size().x + 24.0, 36.0));
            p.rect_filled(fr_r, 9.0, Color32::from_white_alpha(240));
            p.galley(pos2(fr_r.left() + 12.0, fr_r.center().y - g.size().y / 2.0), g, TEXT);
        }
    }

    fn gpu_card(&self, ui: &egui::Ui, r: Rect) {
        let p = ui.painter();
        card(p, r);
        let (hist, last, available) = {
            let g = self.gpu.lock().unwrap();
            (g.util.iter().copied().collect::<Vec<f32>>(), g.last, g.available)
        };
        p.text(r.left_top() + vec2(26.0, 38.0), Align2::LEFT_CENTER, "GPU 사용률", fb(27.0), TEXT);
        p.text(r.left_top() + vec2(22.0, 104.0), Align2::LEFT_CENTER, if available { format!("{last:.0}%") } else { "–".into() }, fb(76.0), TEXT);
        let s = self.pipeline.as_ref().map(|p| &p.shared.stats);
        let ms = |v: Option<u64>| v.map(|x| format!("{:.0}ms", x as f64 / 1000.0)).unwrap_or_else(|| "–".into());
        let matte = s.map(|s| format!("{:.1}ms", s.matte_us_last.load(Ordering::Relaxed) as f64 / 1000.0)).unwrap_or_else(|| "–".into());
        let line_y = r.top() + 162.0;
        let g1 = p.layout_no_wrap(format!("↗ {}", self.gpu_name), fb(21.0), GREEN);
        let w1 = g1.size().x;
        p.galley(pos2(r.left() + 26.0, line_y - g1.size().y / 2.0), g1, GREEN);
        p.text(
            pos2(r.left() + 26.0 + w1 + 12.0, line_y),
            Align2::LEFT_CENTER,
            format!(
                "·  누끼 {matte}  ·  지연 원본 {}  ·  알파 {}",
                ms(s.map(|s| s.orig_send_lat_us.load(Ordering::Relaxed))),
                ms(s.map(|s| s.matte_send_lat_us.load(Ordering::Relaxed)))
            ),
            fr(21.0),
            MUTED,
        );
        let chart = Rect::from_min_max(pos2(r.left() + 20.0, line_y + 26.0), pos2(r.right() - 20.0, r.bottom() - 22.0));
        step_chart(&ui.painter_at(chart), chart, &hist);
    }

    fn draw_toast(&mut self, ctx: &egui::Context) {
        let Some((msg, at)) = self.toast.clone() else { return };
        if at.elapsed() > Duration::from_secs(12) {
            self.toast = None;
            return;
        }
        egui::Area::new(egui::Id::unique("toast"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -70.0))
            .show(ctx, |ui| {
                egui::Frame::new().fill(Color32::from_rgb(0x1C, 0x1D, 0x22)).corner_radius(16).inner_margin(egui::Margin::symmetric(22, 16)).shadow(shadow()).show(ui, |ui| {
                    ui.set_max_width(720.0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 11.0, RED);
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, "!", fb(15.0), Color32::WHITE);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new("송출을 시작하지 못했습니다").font(fb(17.0)).color(Color32::WHITE));
                            ui.add(egui::Label::new(egui::RichText::new(&msg).font(fr(15.0)).color(Color32::from_gray(210))).wrap());
                        });
                        if ui.add(egui::Button::new(egui::RichText::new("닫기").font(fb(14.0)).color(Color32::WHITE)).fill(Color32::from_gray(60)).corner_radius(14)).clicked() {
                            self.toast = None;
                        }
                    });
                });
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn checks_modal(&mut self, ctx: &egui::Context) {
        if !self.show_checks {
            return;
        }
        let items: Vec<Item> = self.checks.clone();
        let missing = items.iter().filter(|c| !c.ok).count();
        let frame = egui::Frame::new().fill(CARD).corner_radius(22).inner_margin(egui::Margin::same(26)).shadow(shadow());
        let resp = egui::Modal::new(egui::Id::unique("checks")).frame(frame).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.label(egui::RichText::new("드라이버 · 런타임").font(fb(26.0)));
            ui.add_space(4.0);
            let intro = if missing > 0 { "빠진 항목은 [다운로드]로 공식 페이지에서 설치한 뒤 [다시 검사]를 누르세요." } else { "필요한 드라이버와 런타임이 모두 설치되어 있습니다." };
            ui.label(egui::RichText::new(intro).font(fr(16.0)).color(MUTED));
            ui.add_space(14.0);
            for c in &items {
                egui::Frame::new().fill(if c.ok { CARD } else { Color32::from_rgb(0xFA, 0xFB, 0xFD) }).stroke(Stroke::new(1.0, LINE)).corner_radius(14).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 6.0, if c.ok { DOT_GREEN } else if c.required { RED } else { MUTED });
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&c.name).font(fb(18.0)));
                                let (tag, col) = if c.ok { ("설치됨", GREEN) } else if c.required { ("필수", RED) } else { ("DeckLink 쓸 때만", MUTED) };
                                ui.label(egui::RichText::new(tag).font(fr(14.0)).color(col));
                            });
                            ui.add(egui::Label::new(egui::RichText::new(&c.detail).font(fr(14.0)).color(MUTED)).wrap());
                        });
                        if !c.ok {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let b = egui::Button::new(egui::RichText::new("다운로드").font(fb(16.0)).color(Color32::WHITE)).fill(BLUE).corner_radius(20).min_size(vec2(110.0, 40.0));
                                if ui.add(b).on_hover_text(&c.url).clicked() {
                                    runtime_check::open_url(&c.url);
                                }
                            });
                        }
                    });
                });
                ui.add_space(8.0);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(egui::RichText::new("닫기").font(fb(16.0))).corner_radius(20).min_size(vec2(96.0, 40.0))).clicked() {
                        self.show_checks = false;
                    }
                    if ui.add(egui::Button::new(egui::RichText::new("다시 검사").font(fb(16.0)).color(BLUE)).fill(BLUE_TINT).corner_radius(20).min_size(vec2(120.0, 40.0))).clicked() {
                        self.checks = runtime_check::check_all();
                    }
                });
            });
        });
        if resp.should_close() {
            self.show_checks = false;
        }
    }
}

fn last_file_store() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME")).map(|d| std::path::PathBuf::from(d).join("OPEN-NDINB").join("last-file.txt"))
}

fn load_last_file() -> String {
    last_file_store()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| std::path::Path::new(s).is_file())
        .unwrap_or_default()
}

fn save_last_file(path: &str) {
    if let Some(p) = last_file_store() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, path);
    }
}

fn icon_of(k: &SourceKind) -> &'static str {
    match k {
        SourceKind::DeckLink(_) => "▭",
        SourceKind::Webcam(_) => "📷",
        SourceKind::File(_) => "🎞",
        SourceKind::Pattern => "▦",
    }
}

fn chevron(p: &egui::Painter, c: Pos2, dir: f32) {
    let s = Stroke::new(2.2, TEXT);
    p.line_segment([c + vec2(-7.0, -3.5 * dir), c + vec2(0.0, 3.5 * dir)], s);
    p.line_segment([c + vec2(0.0, 3.5 * dir), c + vec2(7.0, -3.5 * dir)], s);
}

fn step_chart(p: &egui::Painter, r: Rect, hist: &[f32]) {
    if hist.is_empty() || r.height() < 20.0 {
        p.text(r.center(), Align2::CENTER_CENTER, "GPU 사용률 측정 중…", fr(15.0), MUTED);
        return;
    }
    let n = gpu::HISTORY as f32;
    let step = r.width() / (n - 1.0);
    let x0 = r.right() - step * (hist.len() - 1) as f32;
    let y_of = |v: f32| r.bottom() - 4.0 - (v.clamp(0.0, 100.0) / 100.0) * (r.height() - 8.0);
    let mut pts: Vec<Pos2> = Vec::with_capacity(hist.len() * 2);
    for (i, v) in hist.iter().enumerate() {
        let x = x0 + step * i as f32;
        let y = y_of(*v);
        if let Some(last) = pts.last().copied() {
            pts.push(pos2(x, last.y));
        }
        pts.push(pos2(x, y));
    }
    let dot = 5.0;
    let mut x = (x0 / dot).ceil() * dot;
    let mut idx = 0usize;
    while x <= r.right() {
        while idx + 1 < pts.len() && pts[idx + 1].x <= x {
            idx += 1;
        }
        let top = pts[idx].y;
        let mut y = top + 5.0;
        while y < r.bottom() {
            let fade = 1.0 - ((y - top) / (r.bottom() - top + 1.0)).clamp(0.0, 1.0);
            let a = (25.0 + 170.0 * fade) as u8;
            p.circle_filled(pos2(x, y), 1.25, Color32::from_rgba_unmultiplied(0x00, 0x52, 0xFF, a));
            y += dot;
        }
        x += dot;
    }
    p.add(egui::Shape::line(pts, Stroke::new(3.0, BLUE)));
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let vr = ctx.viewport_rect();
        let px = vr.size() * ctx.zoom_factor();
        let want = (px.x / 1536.0).min(px.y / 1024.0).clamp(0.6, 1.0);
        if (want - ctx.zoom_factor()).abs() > 0.01 {
            ctx.set_zoom_factor(want);
        }
        self.update_textures(&ctx);
        self.poll_pick();
        if let Some(p) = &self.pipeline {
            if let Some(e) = p.shared.error.lock().unwrap().clone() {
                if self.error.as_deref() != Some(e.as_str()) {
                    self.toast = Some((e.clone(), Instant::now()));
                }
                self.error = Some(e);
            }
        }
        let running = self.pipeline.is_some();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
            let full = ui.max_rect();
            let m = 24.0;
            let header = Rect::from_min_size(full.min + vec2(m - 10.0, 0.0), vec2(full.width() - 2.0 * m + 20.0, 76.0));
            self.header(ui, header);
            let footer_h = 44.0;
            let body = Rect::from_min_max(pos2(full.left() + m, header.bottom() + 18.0), pos2(full.right() - m, full.bottom() - footer_h));
            let side_w = 362.0;
            let gap = 18.0;
            let side = Rect::from_min_size(body.min, vec2(side_w, body.height()));
            let main = Rect::from_min_max(pos2(side.right() + gap, body.top()), body.max);
            let prev_h = (main.height() * 0.50).max(260.0);
            let pw = (main.width() - gap) / 2.0;
            let r_orig = Rect::from_min_size(main.min, vec2(pw, prev_h));
            let r_matte = Rect::from_min_size(pos2(r_orig.right() + gap, main.top()), vec2(pw, prev_h));
            let r_gpu = Rect::from_min_max(pos2(main.left(), r_orig.bottom() + gap), main.max);
            self.preview(ui, r_orig, "원본", &self.tex_orig, false, self.fps.1);
            self.preview(ui, r_matte, "누끼 (알파)", &self.tex_matte, true, self.fps.2);
            self.gpu_card(ui, r_gpu);
            self.sidebar(ui, side);
            ui.painter().text(pos2(full.center().x, full.bottom() - footer_h / 2.0), Align2::CENTER_CENTER, "@YUSUNGJIN", fr(17.0), MUTED);
        });

        self.checks_modal(&ctx);
        self.draw_toast(&ctx);

        let t = self.started.elapsed().as_secs_f32();
        if let Some(path) = self.args.popup_screenshot.clone() {
            if !self.show_checks {
                self.checks_opened = None;
            } else if self.checks_opened.is_none() {
                self.checks_opened = Some(Instant::now());
            }
            let settled = self.checks_opened.map(|o| o.elapsed().as_secs_f32() >= 0.8).unwrap_or(false);
            if settled && !self.popup_shot_requested && t >= 2.0 {
                self.popup_shot_requested = true;
                ctx.request_screenshot(move |img| {
                    let _ = save_png(&path, &img);
                });
            }
        }
        if let Some(path) = self.args.screenshot.clone() {
            if !self.shot_requested && t >= self.args.screenshot_after {
                self.shot_requested = true;
                ctx.request_screenshot(move |img| {
                    let _ = save_png(&path, &img);
                });
            }
        }
        if let Some(ex) = self.args.exit_after {
            if t >= ex {
                self.write_stats();
                self.stop();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        ctx.request_repaint_after(Duration::from_millis(if running { 16 } else if self.picking.is_some() { 100 } else { 250 }));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop();
    }
}

fn decode_png(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
        png::ColorType::Rgb => buf[..w * h * 3].chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        _ => return None,
    };
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

fn app_icon() -> egui::IconData {
    decode_png(LOGO_PNG)
        .map(|img| egui::IconData { rgba: img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect(), width: img.size[0] as u32, height: img.size[1] as u32 })
        .unwrap_or_default()
}

fn save_png(path: &str, img: &egui::ColorImage) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    w.write_image_data(&bytes).map_err(|e| e.to_string())
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let latin = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut load = |key: &str, paths: &[&str]| -> bool {
        for p in paths {
            if let Ok(bytes) = std::fs::read(p) {
                fonts.font_data.insert(key.into(), Arc::new(egui::FontData::from_owned(bytes)));
                return true;
            }
        }
        false
    };
    let has_kr = load("kr", &[r"C:\Windows\Fonts\malgun.ttf", "/usr/share/fonts/truetype/nanum/NanumGothic.ttf"]);
    let has_krb = load("kr-bold", &[r"C:\Windows\Fonts\malgunbd.ttf", "/usr/share/fonts/truetype/nanum/NanumGothicBold.ttf"]);
    let has_sym = load("sym", &[r"C:\Windows\Fonts\seguisym.ttf"]);
    let has_emoji = load("emoji", &[r"C:\Windows\Fonts\seguiemj.ttf"]);
    let mut regular = Vec::new();
    if has_kr {
        regular.push("kr".to_string());
    }
    regular.extend(latin.iter().cloned());
    if has_sym {
        regular.push("sym".into());
    }
    if has_emoji {
        regular.push("emoji".into());
    }
    let mut bold_list = Vec::new();
    if has_krb {
        bold_list.push("kr-bold".to_string());
    }
    bold_list.extend(regular.iter().cloned());
    fonts.families.insert(FontFamily::Proportional, regular.clone());
    fonts.families.entry(FontFamily::Monospace).or_default().extend(regular.iter().cloned());
    fonts.families.insert(bold(), bold_list);
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result {
    let args = parse_args();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title(BASE_NAME).with_icon(app_icon()).with_inner_size([1536.0, 1024.0]).with_min_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(BASE_NAME, opts, Box::new(|cc| Ok(Box::new(App::new(cc, args)))))
}
