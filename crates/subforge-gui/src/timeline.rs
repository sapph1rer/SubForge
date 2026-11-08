//! Audio waveform timeline with draggable subtitle blocks.
//!
//! * drag a block's edges to retime, drag the body to move it (snaps to neighbours / playhead)
//! * click empty space to seek, double-click a block to play it
//! * wheel = scroll, Ctrl+wheel = zoom, right-click = context menu

use crate::app::App;
use crate::icons::{self, ibtn, ic, ic_s, itoggle, menu_item};
use eframe::egui::RichText;
use crate::media::WAVE_BUCKET_MS;
use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use subforge_core::time::fmt_display;

#[derive(Clone, Copy, PartialEq, Debug)]
enum DragKind {
    Body,
    Start,
    End,
    Scrub,
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    kind: DragKind,
    idx: usize,
    orig_start: i64,
    orig_end: i64,
    press_ms: f64,
}

pub struct TimelineState {
    /// left edge of the view in ms
    pub view_start: f64,
    /// zoom: milliseconds per pixel
    pub ms_per_px: f64,
    drag: Option<Drag>,
    pub follow: bool,
    pub playhead_hint: i64,
    context_ms: i64,
    pub duration_ms: i64,
    last_cur: Option<usize>,
    spec_tex: Option<egui::TextureHandle>,
    /// (view start, ms/px, width, data length, gain, dark) the texture was built for
    spec_key: Option<(i64, u64, usize, usize, u32, bool)>,
}

impl Default for TimelineState {
    fn default() -> Self {
        TimelineState { view_start: 0.0, ms_per_px: 10.0, drag: None, follow: true, playhead_hint: 0, context_ms: 0, duration_ms: 0, last_cur: None, spec_tex: None, spec_key: None }
    }
}

/// "magma"-like colour ramp for the spectrogram.
fn spec_lut(gain: f32, dark: bool) -> [Color32; 256] {
    const STOPS: [(f32, [f32; 3]); 6] = [(0.0, [0.0, 0.0, 4.0]), (0.2, [40.0, 11.0, 84.0]), (0.4, [114.0, 31.0, 129.0]), (0.6, [205.0, 63.0, 113.0]), (0.8, [251.0, 136.0, 97.0]), (1.0, [252.0, 253.0, 191.0])];
    let mut lut = [Color32::BLACK; 256];
    for (i, c) in lut.iter_mut().enumerate() {
        // contrast: stretch the upper part of the dB range
        let v = ((i as f32 / 255.0 - 0.25) / 0.75 * gain).clamp(0.0, 1.0);
        let k = STOPS.iter().position(|s| s.0 >= v).unwrap_or(5).max(1);
        let (a, b) = (STOPS[k - 1], STOPS[k]);
        let f = ((v - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
        let ch = |j: usize| (a.1[j] + (b.1[j] - a.1[j]) * f) as u8;
        let rgb = [ch(0), ch(1), ch(2)];
        *c = if dark { Color32::from_rgb(rgb[0], rgb[1], rgb[2]) } else { Color32::from_rgb(255 - rgb[0] / 2, 255 - rgb[1] / 2, 255 - rgb[2] / 3) };
    }
    lut
}

impl TimelineState {
    pub fn zoom_by(&mut self, f: f64) {
        self.ms_per_px = (self.ms_per_px / f).clamp(0.5, 5000.0);
    }
    pub fn reset_for(&mut self, dur: i64) {
        self.duration_ms = dur;
        self.view_start = 0.0;
        self.ms_per_px = 10.0;
    }
}

fn nice_step(ms_per_px: f64) -> (i64, i64) {
    // (major tick, minor tick) in ms targeting ~100 px between majors
    let target = ms_per_px * 100.0;
    let steps = [100, 200, 500, 1000, 2000, 5000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000];
    let major = *steps.iter().find(|&&s| s as f64 >= target).unwrap_or(&600_000);
    (major, (major / 5).max(20))
}

impl App {
    fn timeline_toolbar(&mut self, ui: &mut egui::Ui) {
        let kfs = self.keyframes();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            // zoom
            if ui.add(ibtn(icons::ICON_ZOOM_OUT)).on_hover_text("Zoom out (Ctrl+wheel)").clicked() {
                self.tl.zoom_by(1.0 / 1.5);
            }
            // zoom slider: visible seconds, logarithmic
            let w = ui.available_width().max(200.0) as f64;
            let mut secs = (self.tl.ms_per_px * w / 1000.0).clamp(0.5, 7200.0);
            let r = ui.add(egui::Slider::new(&mut secs, 0.5..=3600.0).logarithmic(true).show_value(false).clamping(egui::SliderClamping::Always)).on_hover_text("Visible span");
            if r.changed() {
                let center = self.tl.view_start + self.tl.ms_per_px * w * 0.5;
                self.tl.ms_per_px = (secs * 1000.0 / w).clamp(0.5, 5000.0);
                self.tl.view_start = (center - self.tl.ms_per_px * w * 0.5).max(0.0);
            }
            if ui.add(ibtn(icons::ICON_ZOOM_IN)).on_hover_text("Zoom in").clicked() {
                self.tl.zoom_by(1.5);
            }
            ui.label(RichText::new(format!("{:.1}s", secs)).small().weak().monospace());
            if ui.add(ibtn(icons::ICON_FIT_WIDTH)).on_hover_text("Fit current line").clicked() {
                if let Some(e) = self.cur.and_then(|c| self.doc.sub.events.get(c)) {
                    let span = (e.duration().max(500) as f64) * 3.0;
                    self.tl.ms_per_px = (span / w).clamp(0.5, 2000.0);
                    self.tl.view_start = (e.start as f64 - span / 3.0).max(0.0);
                }
            }
            if ui.add(ibtn(icons::ICON_ZOOM_OUT_MAP)).on_hover_text("Fit whole file").clicked() {
                let total = self.doc.sub.last_end().max(self.tl.duration_ms).max(10_000) as f64;
                self.tl.ms_per_px = (total / w).clamp(0.5, 5000.0);
                self.tl.view_start = 0.0;
            }
            if ui.add(itoggle(self.tl.follow, icons::ICON_MY_LOCATION)).on_hover_text("Follow playhead while playing").clicked() {
                self.tl.follow = !self.tl.follow;
            }
            ui.separator();

            // snapping
            let any_snap = self.settings.tl_snap_keyframes || self.settings.tl_snap_lines || self.settings.tl_snap_playhead;
            ui.menu_button((ic(icons::ICON_ANCHOR).color(if any_snap { Color32::from_rgb(120, 170, 255) } else { ui.visuals().weak_text_color() }), "Snap"), |ui| {
                ui.checkbox(&mut self.settings.tl_snap_keyframes, (ic_s(icons::ICON_DIAMOND), "Keyframes"));
                ui.checkbox(&mut self.settings.tl_snap_lines, (ic_s(icons::ICON_LIST), "Other lines' start/end"));
                ui.checkbox(&mut self.settings.tl_snap_playhead, (ic_s(icons::ICON_MY_LOCATION), "Playhead"));
                ui.horizontal(|ui| {
                    ui.label("Distance");
                    ui.add(egui::DragValue::new(&mut self.settings.tl_snap_px).range(2.0..=40.0).suffix(" px"));
                });
                ui.label(RichText::new("Hold Alt while dragging to snap nothing").small().weak());
            });

            // keyframes
            let kf_label = match (&kfs, self.kf_override.is_some()) {
                (Some(k), true) => format!("{} (file)", k.len()),
                (Some(k), false) => format!("{}", k.len()),
                (None, _) => "—".into(),
            };
            ui.menu_button((ic(icons::ICON_DIAMOND).color(Color32::from_rgb(190, 110, 230)), kf_label), |ui| {
                ui.label(RichText::new("Keyframes (scene changes)").strong());
                ui.checkbox(&mut self.settings.tl_show_keyframes, "Show on timeline");
                ui.separator();
                if ui.add_enabled(kfs.is_some(), menu_item(icons::ICON_FIRST_PAGE, "Previous keyframe", "Alt+←")).clicked() {
                    self.seek_keyframe(false);
                    ui.close();
                }
                if ui.add_enabled(kfs.is_some(), menu_item(icons::ICON_LAST_PAGE, "Next keyframe", "Alt+→")).clicked() {
                    self.seek_keyframe(true);
                    ui.close();
                }
                if ui.add_enabled(kfs.is_some() && self.cur.is_some(), menu_item(icons::ICON_ANCHOR, "Snap selected lines", "F6")).clicked() {
                    self.snap_selected_to_keyframes();
                    ui.close();
                }
                ui.horizontal(|ui| {
                    ui.label("Window");
                    ui.add(egui::DragValue::new(&mut self.settings.kf_before_ms).range(0..=2000).suffix(" ms before"));
                    ui.add(egui::DragValue::new(&mut self.settings.kf_after_ms).range(0..=2000).suffix(" ms after"));
                });
                ui.separator();
                if ui.add(menu_item(icons::ICON_UPLOAD_FILE, "Load keyframe file…", "")).on_hover_text("Aegisub keyframes (v1), XviD/x264 stats-style frame lists, or times").clicked() {
                    self.load_keyframe_file();
                    ui.close();
                }
                if ui.add_enabled(self.kf_override.is_some(), menu_item(icons::ICON_MOVIE, "Use video's keyframes", "")).clicked() {
                    self.kf_override = None;
                    ui.close();
                }
                if ui.add_enabled(kfs.is_some(), menu_item(icons::ICON_DOWNLOAD, "Export (Aegisub format)…", "")).clicked() {
                    self.export_keyframes();
                    ui.close();
                }
            });
            if ui.add_enabled(kfs.is_some(), ibtn(icons::ICON_FIRST_PAGE)).on_hover_text("Previous keyframe (Alt+←)").clicked() {
                self.seek_keyframe(false);
            }
            if ui.add_enabled(kfs.is_some(), ibtn(icons::ICON_LAST_PAGE)).on_hover_text("Next keyframe (Alt+→)").clicked() {
                self.seek_keyframe(true);
            }
            ui.separator();

            // view
            ui.menu_button((ic(icons::ICON_VISIBILITY), "View"), |ui| {
                ui.checkbox(&mut self.settings.tl_show_wave, (ic_s(icons::ICON_GRAPHIC_EQ), "Audio"));
                ui.indent("audio_view", |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.settings.tl_audio_view, 0, "Waveform");
                        ui.selectable_value(&mut self.settings.tl_audio_view, 1, "Spectrum");
                        ui.selectable_value(&mut self.settings.tl_audio_view, 2, "Both");
                    });
                    if self.settings.tl_audio_view > 0 {
                        ui.horizontal(|ui| {
                            ui.label("Spectrum contrast");
                            ui.add(egui::Slider::new(&mut self.settings.tl_spec_gain, 0.5..=4.0).max_decimals(2));
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Waveform gain");
                    ui.add(egui::Slider::new(&mut self.settings.tl_wave_gain, 0.25..=6.0).logarithmic(true).max_decimals(2));
                });
                ui.checkbox(&mut self.settings.tl_show_keyframes, (ic_s(icons::ICON_DIAMOND), "Keyframes"));
                ui.checkbox(&mut self.settings.tl_show_comments, (ic_s(icons::ICON_COMMENT), "Comment lines"));
                ui.checkbox(&mut self.settings.tl_show_text, (ic_s(icons::ICON_TEXT_FIELDS), "Text in blocks"));
                ui.horizontal(|ui| {
                    ui.label("Rows");
                    ui.add(egui::DragValue::new(&mut self.settings.tl_lanes).range(1..=8));
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(p) = &self.player {
                    let w = p.warm_status();
                    if !w.keyframes && p.info.has_video {
                        ui.label(RichText::new(format!("keyframes {:.0}%", w.keyframe_progress * 100.0)).small().weak());
                        ui.spinner();
                    }
                    if !w.waveform {
                        ui.label(RichText::new(format!("waveform {:.0}%", w.waveform_progress * 100.0)).small().weak());
                        ui.spinner();
                    }
                    if self.settings.tl_show_wave && self.settings.tl_audio_view > 0 && p.spectrum.as_ref().is_some_and(|s| !s.done.load(std::sync::atomic::Ordering::Relaxed)) {
                        ui.label(RichText::new("spectrum…").small().weak());
                        ui.spinner();
                    }
                }
                if let Some(c) = self.cur.and_then(|c| self.doc.sub.events.get(c)) {
                    ui.label(RichText::new(format!("{} → {}  {:.2}s", fmt_display(c.start), fmt_display(c.end), c.duration() as f64 / 1000.0)).small().monospace());
                }
            });
        });
    }

    pub fn load_keyframe_file(&mut self) {
        let Some(p) = rfd::FileDialog::new().add_filter("Keyframes", &["txt", "log", "kf", "pass"]).add_filter("All files", &["*"]).set_directory(self.last_dir()).pick_file() else { return };
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|c| subforge_core::timing::parse_keyframes(&c, Some(self.fps_or_default()))) {
            Ok(k) => {
                self.flash(format!("Loaded {} keyframes from {}", k.len(), p.display()));
                self.kf_override = Some(std::sync::Arc::new(k));
            }
            Err(e) => self.flash_err(format!("Cannot read keyframes: {e}")),
        }
    }

    pub fn export_keyframes(&mut self) {
        let Some(k) = self.keyframes() else { return };
        let fps = self.fps_or_default();
        if let Some(out) = rfd::FileDialog::new().set_file_name("keyframes.txt").add_filter("Keyframes", &["txt"]).save_file() {
            match std::fs::write(&out, subforge_core::timing::keyframes_to_aegisub(&k, fps)) {
                Ok(_) => self.flash(format!("Saved {} keyframes", k.len())),
                Err(e) => self.flash_err(format!("Save failed: {e}")),
            }
        }
    }

    pub fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        self.timeline_toolbar(ui);

        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let vis = ui.visuals().clone();
        let dark = vis.dark_mode;
        painter.rect_filled(rect, 3.0, if dark { Color32::from_gray(22) } else { Color32::from_gray(235) });

        let playhead = self.player.as_ref().map(|p| p.time()).unwrap_or(self.tl.playhead_hint);
        let playing = self.player.as_ref().is_some_and(|p| p.is_playing());
        let w = rect.width() as f64;
        let ruler_h = 16.0;

        // follow playhead / selected line
        if playing && self.tl.follow && self.tl.drag.is_none() {
            let rel = (playhead as f64 - self.tl.view_start) / self.tl.ms_per_px;
            if rel < 0.0 || rel > w * 0.85 {
                self.tl.view_start = (playhead as f64 - w * 0.1 * self.tl.ms_per_px).max(0.0);
            }
        }
        if self.cur != self.tl.last_cur && !playing && self.tl.drag.is_none() {
            if let Some(e) = self.cur.and_then(|c| self.doc.sub.events.get(c)) {
                let a = (e.start as f64 - self.tl.view_start) / self.tl.ms_per_px;
                let b = (e.end as f64 - self.tl.view_start) / self.tl.ms_per_px;
                if a < 0.0 || b > w {
                    self.tl.view_start = (e.start as f64 - w * 0.25 * self.tl.ms_per_px).max(0.0);
                }
            }
            self.tl.last_cur = self.cur;
        }

        // wheel: scroll / zoom
        if resp.hovered() {
            let (zoom, scroll, hover) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
            if zoom != 1.0 {
                let anchor_px = hover.map(|p| (p.x - rect.left()) as f64).unwrap_or(w / 2.0);
                let anchor_ms = self.tl.view_start + anchor_px * self.tl.ms_per_px;
                self.tl.ms_per_px = (self.tl.ms_per_px / zoom as f64).clamp(0.5, 5000.0);
                self.tl.view_start = (anchor_ms - anchor_px * self.tl.ms_per_px).max(0.0);
            } else {
                let d = if scroll.x.abs() > scroll.y.abs() { scroll.x } else { scroll.y };
                if d != 0.0 {
                    self.tl.view_start = (self.tl.view_start - d as f64 * self.tl.ms_per_px * 1.5).max(0.0);
                }
            }
        }

        let vs = self.tl.view_start;
        let mpp = self.tl.ms_per_px;
        let x_of = |ms: f64| rect.left() + ((ms - vs) / mpp) as f32;
        let ms_of = |x: f32| vs + (x - rect.left()) as f64 * mpp;
        let view_end = vs + w * mpp;

        // ruler
        let (major, minor) = nice_step(mpp);
        let mut t = (vs as i64 / minor) * minor;
        let tick_col = if dark { Color32::from_gray(90) } else { Color32::from_gray(150) };
        while (t as f64) < view_end {
            let x = x_of(t as f64);
            if t % major == 0 {
                painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.top() + ruler_h)], Stroke::new(1.0, tick_col));
                let label = if major >= 1000 { fmt_display(t).trim_end_matches(".000").to_string() } else { fmt_display(t) };
                painter.text(Pos2::new(x + 3.0, rect.top() + 1.0), Align2::LEFT_TOP, label, FontId::monospace(10.0), tick_col);
                painter.line_segment([Pos2::new(x, rect.top() + ruler_h), Pos2::new(x, rect.bottom())], Stroke::new(1.0, tick_col.gamma_multiply(0.25)));
            } else {
                painter.line_segment([Pos2::new(x, rect.top() + ruler_h - 5.0), Pos2::new(x, rect.top() + ruler_h)], Stroke::new(1.0, tick_col));
            }
            t += minor;
        }
        let area = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + ruler_h), rect.max);

        // spectrum (behind the waveform)
        if self.settings.tl_show_wave && self.settings.tl_audio_view > 0 {
            let ctx = ui.ctx().clone();
            if let Some(p) = self.player.as_mut() {
                p.ensure_spectrum(&ctx);
            }
            let spec = self.player.as_ref().and_then(|p| p.spectrum.as_ref()).map(|s| s.data.clone());
            if let Some(data) = spec {
                use subforge_core::audio::{COL_MS, ROWS};
                let data = data.lock().unwrap();
                let cols = data.len() / ROWS;
                let pw = rect.width().max(1.0) as usize;
                let key = (vs as i64, mpp.to_bits(), pw, cols, (self.settings.tl_spec_gain * 100.0) as u32, dark);
                if cols > 0 && self.tl.spec_key != Some(key) {
                    let lut = spec_lut(self.settings.tl_spec_gain, dark);
                    let mut img = egui::ColorImage::new([pw, ROWS], vec![Color32::TRANSPARENT; pw * ROWS]);
                    for x in 0..pw {
                        let t0 = vs + x as f64 * mpp;
                        let c0 = (t0 / COL_MS as f64) as usize;
                        if c0 >= cols {
                            break;
                        }
                        let c1 = (((t0 + mpp) / COL_MS as f64) as usize).clamp(c0 + 1, cols);
                        let step = ((c1 - c0) / 16).max(1);
                        for r in 0..ROWS {
                            let mut v = 0u8;
                            let mut c = c0;
                            while c < c1 {
                                v = v.max(data[c * ROWS + r]);
                                c += step;
                            }
                            img.pixels[(ROWS - 1 - r) * pw + x] = lut[v as usize];
                        }
                    }
                    match &mut self.tl.spec_tex {
                        Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                        None => self.tl.spec_tex = Some(ui.ctx().load_texture("spectrum", img, egui::TextureOptions::LINEAR)),
                    }
                    self.tl.spec_key = Some(key);
                }
                if let Some(t) = &self.tl.spec_tex {
                    let tint = if self.settings.tl_audio_view == 2 { Color32::from_white_alpha(170) } else { Color32::WHITE };
                    painter.image(t.id(), area, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), tint);
                }
            }
        }

        // waveform
        let gain = self.settings.tl_wave_gain;
        if let Some(wf) = self.player.as_ref().and_then(|p| p.waveform.as_ref()).filter(|_| self.settings.tl_show_wave && self.settings.tl_audio_view != 1) {
            let peaks = wf.peaks.lock().unwrap();
            if !peaks.is_empty() {
                let mid = area.center().y;
                let half = area.height() / 2.0 - 2.0;
                let wave_col = if dark { Color32::from_rgb(60, 140, 120) } else { Color32::from_rgb(70, 150, 130) };
                let mut x = 0.0f32;
                while x < rect.width() {
                    let t0 = ms_of(rect.left() + x).max(0.0);
                    let t1 = ms_of(rect.left() + x + 1.0).max(0.0);
                    let b0 = (t0 as i64 / WAVE_BUCKET_MS) as usize;
                    let b1 = ((t1 as i64 / WAVE_BUCKET_MS) as usize).max(b0 + 1);
                    if b0 >= peaks.len() {
                        break;
                    }
                    let m = (peaks[b0..b1.min(peaks.len())].iter().copied().max().unwrap_or(0) as f32 / 255.0 * gain).min(1.0);
                    if m > 0.01 {
                        let hgt = m * half;
                        painter.line_segment([Pos2::new(rect.left() + x, mid - hgt), Pos2::new(rect.left() + x, mid + hgt)], Stroke::new(1.0, wave_col));
                    }
                    x += 1.0;
                }
            }
        }

        // keyframes (scene changes)
        let kfs = self.keyframes();
        if let Some(k) = kfs.as_ref().filter(|_| self.settings.tl_show_keyframes) {
            let a = k.partition_point(|&t| (t as f64) < vs);
            let b = k.partition_point(|&t| (t as f64) <= view_end);
            let kcol = Color32::from_rgb(190, 110, 230);
            // when zoomed far out, thin them so the timeline doesn't turn solid
            let min_px = 3.0;
            let mut last_x = f32::NEG_INFINITY;
            for &t in &k[a..b] {
                let x = x_of(t as f64);
                if x - last_x < min_px {
                    continue;
                }
                last_x = x;
                painter.line_segment([Pos2::new(x, rect.top() + ruler_h), Pos2::new(x, rect.bottom())], Stroke::new(1.0, kcol.gamma_multiply(0.85)));
            }
        }

        // events (lanes for overlaps)
        let lanes = self.settings.tl_lanes.clamp(1, 8);
        let show_comments = self.settings.tl_show_comments;
        let lane_h = (area.height() - 6.0) / lanes as f32;
        let mut lane_end = vec![i64::MIN; lanes];
        let mut boxes: Vec<(usize, Rect)> = Vec::new();
        let mut order: Vec<usize> = (0..self.doc.sub.events.len()).filter(|&i| {
            let e = &self.doc.sub.events[i];
            let src = e.comment && e.effect.starts_with(subforge_core::anim::EFFECT_SRC);
            let frame = e.effect.starts_with(subforge_core::anim::EFFECT_GEN);
            // animations show as one block (their source line); generated frames stay hidden
            (!e.comment || show_comments || src) && !frame && (e.end as f64) >= vs - 60_000.0 && (e.start as f64) <= view_end
        }).collect();
        order.sort_by_key(|&i| (self.doc.sub.events[i].start, self.doc.sub.events[i].end));
        let per_event = self.issues().per_event.clone();
        let hover = resp.hover_pos();
        for &i in &order {
            let e = &self.doc.sub.events[i];
            let lane = (0..lanes).find(|&l| lane_end[l] <= e.start).unwrap_or(lanes - 1);
            lane_end[lane] = lane_end[lane].max(e.end);
            if (e.end as f64) < vs {
                continue;
            }
            let x0 = x_of(e.start as f64);
            let x1 = x_of(e.end as f64).max(x0 + 2.0);
            let y0 = area.top() + 3.0 + lane as f32 * lane_h;
            boxes.push((i, Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y0 + lane_h - 2.0))));
        }
        for &(i, r) in &boxes {
            let e = &self.doc.sub.events[i];
            let selected = self.sel.contains(&i);
            let is_cur = self.cur == Some(i);
            let base = match per_event.get(i).copied().flatten() {
                Some(subforge_core::qc::Severity::Error) => Color32::from_rgb(170, 60, 60),
                Some(subforge_core::qc::Severity::Warning) => Color32::from_rgb(170, 120, 40),
                _ => Color32::from_rgb(60, 100, 170),
            };
            let is_anim = e.effect.starts_with(subforge_core::anim::EFFECT_SRC) || e.text.starts_with("{sftw:");
            let base = if is_anim {
                Color32::from_rgb(80, 90, 190)
            } else if e.comment {
                Color32::from_gray(90)
            } else {
                base
            };
            let hovered = hover.is_some_and(|h| r.contains(h)) && self.tl.drag.is_none();
            let fill = if is_cur { base.gamma_multiply(1.3) } else if selected || hovered { base } else { base.gamma_multiply(0.65) };
            painter.rect_filled(r, 3.0, fill.gamma_multiply(0.8));
            painter.rect_stroke(r, 3.0, Stroke::new(if is_cur { 2.0 } else { 1.0 }, if is_cur { Color32::WHITE } else { fill }), StrokeKind::Inside);
            if is_cur {
                // edge handles
                for x in [r.left(), r.right()] {
                    painter.rect_filled(Rect::from_center_size(Pos2::new(x, r.center().y), Vec2::new(4.0, (r.height() * 0.5).max(8.0))), 1.0, Color32::WHITE);
                }
            }
            if is_anim && r.width() > 18.0 {
                painter.text(
                    Pos2::new(r.right() - 3.0, r.center().y),
                    Align2::RIGHT_CENTER,
                    <&str>::from(icons::ICON_ANIMATION),
                    FontId::new(13.0, egui::FontFamily::Name(egui_material_icons::FONT_FAMILY.into())),
                    Color32::WHITE,
                );
            }
            if self.settings.tl_show_text && r.width() > 24.0 {
                let txt = e.plain().replace('\n', " / ");
                let clip = painter.with_clip_rect(r.shrink(3.0).intersect(rect));
                clip.text(Pos2::new(r.left() + 4.0, r.center().y), Align2::LEFT_CENTER, txt, FontId::proportional(12.0), Color32::WHITE);
            }
        }

        // playhead
        let px = x_of(playhead as f64);
        if px >= rect.left() && px <= rect.right() {
            painter.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(1.5, Color32::from_rgb(255, 80, 80)));
        }

        // ---- interaction
        let cur_idx = self.cur;
        let hit = |p: Pos2| -> Option<(usize, DragKind)> {
            // prefer current line when stacked
            let mut found = None;
            for &(i, r) in boxes.iter().rev() {
                if r.expand2(Vec2::new(4.0, 0.0)).contains(p) {
                    let kind = if (p.x - r.left()).abs() <= 6.0 {
                        DragKind::Start
                    } else if (p.x - r.right()).abs() <= 6.0 {
                        DragKind::End
                    } else {
                        DragKind::Body
                    };
                    if cur_idx == Some(i) {
                        return Some((i, kind));
                    }
                    found.get_or_insert((i, kind));
                }
            }
            found
        };
        if let Some(p) = hover {
            if self.tl.drag.is_none() {
                match hit(p).map(|h| h.1) {
                    Some(DragKind::Start) | Some(DragKind::End) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
                    Some(DragKind::Body) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
                    _ => {}
                }
            }
            // hover time tooltip
            let ht = ms_of(p.x).max(0.0) as i64;
            painter.text(Pos2::new(rect.right() - 4.0, rect.top() + 1.0), Align2::RIGHT_TOP, fmt_display(ht), FontId::monospace(10.0), vis.weak_text_color());
        }

        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let press_ms = ms_of(p.x);
                if let Some((i, kind)) = hit(p) {
                    let e = &self.doc.sub.events[i];
                    self.tl.drag = Some(Drag { kind, idx: i, orig_start: e.start, orig_end: e.end, press_ms });
                    if !self.sel.contains(&i) || self.sel.len() == 1 {
                        self.select_only(Some(i));
                        self.scroll_to = Some(i);
                    }
                    self.checkpoint();
                } else {
                    self.tl.drag = Some(Drag { kind: DragKind::Scrub, idx: 0, orig_start: 0, orig_end: 0, press_ms });
                    self.seek(press_ms.max(0.0) as i64);
                }
            }
        }
        if resp.dragged() {
            if let (Some(d), Some(p)) = (self.tl.drag, resp.interact_pointer_pos()) {
                let now_ms = ms_of(p.x);
                if d.kind == DragKind::Scrub {
                    self.seek(now_ms.max(0.0) as i64);
                } else {
                    // snapping targets (within 8 px)
                    let snap_px = self.settings.tl_snap_px as f64 * mpp;
                    let mut targets: Vec<i64> = Vec::new();
                    if self.settings.tl_snap_playhead {
                        targets.push(playhead);
                    }
                    if let Some(k) = kfs.as_ref().filter(|_| self.settings.tl_snap_keyframes) {
                        let a = k.partition_point(|&t| (t as f64) < vs);
                        let b = k.partition_point(|&t| (t as f64) <= view_end);
                        targets.extend_from_slice(&k[a..b]);
                    }
                    for &(j, _) in boxes.iter().filter(|_| self.settings.tl_snap_lines) {
                        if j != d.idx {
                            let o = &self.doc.sub.events[j];
                            targets.push(o.start);
                            targets.push(o.end);
                        }
                    }
                    let snap = |t: i64| -> i64 {
                        targets.iter().copied().filter(|&s| ((s - t).abs() as f64) <= snap_px).min_by_key(|&s| (s - t).abs()).unwrap_or(t)
                    };
                    let delta = (now_ms - d.press_ms).round() as i64;
                    let free = ui.input(|i| i.modifiers.alt); // Alt = no snapping
                    let sn = |t: i64| if free { t } else { snap(t) };
                    let (mut s, mut en) = (d.orig_start, d.orig_end);
                    match d.kind {
                        DragKind::Start => s = sn(d.orig_start + delta).min(d.orig_end - 10).max(0),
                        DragKind::End => en = sn(d.orig_end + delta).max(d.orig_start + 10),
                        DragKind::Body => {
                            let ns = sn(d.orig_start + delta);
                            let ne = sn(d.orig_end + delta);
                            // snap whichever edge moved less
                            let shift = if (ns - (d.orig_start + delta)).abs() <= (ne - (d.orig_end + delta)).abs() { ns - d.orig_start } else { ne - d.orig_end };
                            let shift = shift.max(-d.orig_start);
                            s = d.orig_start + shift;
                            en = d.orig_end + shift;
                        }
                        DragKind::Scrub => {}
                    }
                    if let Some(ev) = self.doc.sub.events.get_mut(d.idx) {
                        if ev.start != s || ev.end != en {
                            ev.start = s;
                            ev.end = en;
                            self.doc.gen += 1;
                            self.doc.dirty = true;
                        }
                    }
                    ui.ctx().set_cursor_icon(if d.kind == DragKind::Body { CursorIcon::Grabbing } else { CursorIcon::ResizeHorizontal });
                    // tooltip with times
                    painter.text(
                        Pos2::new(p.x + 10.0, area.top() + 2.0),
                        Align2::LEFT_TOP,
                        format!("{} – {}  ({:.2}s)", fmt_display(s), fmt_display(en), (en - s) as f64 / 1000.0),
                        FontId::monospace(11.0),
                        Color32::WHITE,
                    );
                }
            }
        }
        if resp.drag_stopped() {
            // retimed an animation: regenerate its frames so they follow the source line
            if let Some(d) = self.tl.drag.filter(|d| d.kind != DragKind::Scrub) {
                let fps = self.fps_or_default();
                if let Some(tw) = subforge_core::anim::read_tween(&self.doc.sub, d.idx) {
                    if self.doc.sub.events[d.idx].effect.starts_with(subforge_core::anim::EFFECT_SRC) {
                        let moved = (self.doc.sub.events[d.idx].start, self.doc.sub.events[d.idx].end) != (d.orig_start, d.orig_end);
                        if moved {
                            // re-apply from the restored source with the new timing
                            let (ns, ne) = (self.doc.sub.events[d.idx].start, self.doc.sub.events[d.idx].end);
                            if let Some(i) = subforge_core::anim::remove_tween(&mut self.doc.sub, d.idx) {
                                self.doc.sub.events[i].start = ns;
                                self.doc.sub.events[i].end = ne;
                                if let Ok(r) = subforge_core::anim::apply_tween(&mut self.doc.sub, i, &tw, fps) {
                                    self.select_only(Some(r.index.saturating_sub(1)));
                                }
                            }
                            self.touch();
                            self.motion.loaded_for = None;
                        }
                    }
                }
            }
            if self.tl.drag.is_some_and(|d| d.kind == DragKind::Scrub) {
                // exact frame after a fast scrub
                self.seek(self.tl.playhead_hint);
            }
            self.tl.drag = None;
            self.end_text_session();
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                match hit(p) {
                    Some((i, _)) => {
                        let mods = ui.input(|i| i.modifiers);
                        self.click_row(i, mods);
                        self.scroll_to = Some(i);
                    }
                    None => self.seek(ms_of(p.x).max(0.0) as i64),
                }
            }
        }
        if resp.double_clicked() {
            if let Some((i, _)) = resp.interact_pointer_pos().and_then(|p| hit(p)) {
                let e = &self.doc.sub.events[i];
                let (a, b) = (e.start, e.end);
                self.play_range(a, b);
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.tl.context_ms = ms_of(p.x).max(0.0) as i64;
                if let Some((i, _)) = hit(p) {
                    if !self.sel.contains(&i) {
                        self.select_only(Some(i));
                    }
                }
            }
        }
        resp.context_menu(|ui| {
            let t = self.tl.context_ms;
            ui.label(RichText::new(fmt_display(t)).weak().monospace());
            if ui.add(menu_item(icons::ICON_ADD, "Insert line here", "")).clicked() {
                self.insert_at(t);
                ui.close();
            }
            let has = self.cur.is_some();
            if ui.add_enabled(has, menu_item(icons::ICON_FIRST_PAGE, "Set start here", "")).clicked() {
                if let Some(c) = self.cur {
                    self.modify(|s| {
                        s.events[c].start = t;
                        if s.events[c].end <= t {
                            s.events[c].end = t + 1000;
                        }
                    });
                }
                ui.close();
            }
            if ui.add_enabled(has, menu_item(icons::ICON_LAST_PAGE, "Set end here", "")).clicked() {
                if let Some(c) = self.cur {
                    self.modify(|s| s.events[c].end = t.max(s.events[c].start + 10));
                }
                ui.close();
            }
            if ui.add_enabled(has, menu_item(icons::ICON_CALL_SPLIT, "Split current line here", "")).clicked() {
                if let Some(c) = self.cur {
                    self.modify(|s| subforge_core::edit::split(s, c, Some(t)));
                }
                ui.close();
            }
            ui.separator();
            if ui.add_enabled(has && kfs.is_some(), menu_item(icons::ICON_ANCHOR, "Snap selected to keyframes", "F6")).clicked() {
                self.snap_selected_to_keyframes();
                ui.close();
            }
            if let Some(k) = &kfs {
                if let Some(kt) = subforge_core::timing::nearest_keyframe(k, t, 60_000, 60_000) {
                    if ui.add(menu_item(icons::ICON_DIAMOND, &format!("Seek to nearest keyframe ({})", fmt_display(kt)), "")).clicked() {
                        self.seek(kt);
                        ui.close();
                    }
                }
            }
            ui.separator();
            if ui.add(menu_item(icons::ICON_MY_LOCATION, "Seek here", "")).clicked() {
                self.seek(t);
                ui.close();
            }
            if ui.add(menu_item(icons::ICON_PLAY_ARROW, "Play from here", "")).clicked() {
                if let Some(p) = &mut self.player {
                    p.play_from(t, None);
                }
                ui.close();
            }
        });
    }
}
