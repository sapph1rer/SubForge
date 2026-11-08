//! Motion tracking UI: place points on the video (Track tool), track them through the frames of
//! the selected lines in the background, preview the path, and attach lines to it.

use crate::app::App;
use crate::icons::{self, btn, ic_s};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Response, RichText, Stroke, Vec2};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use subforge_core::track::{self, ApplyOpts, Sample, Track, TrackParams};

pub struct TrackerState {
    /// Points in script coordinates (A, optional B)
    pub points: Vec<(f32, f32)>,
    /// Frame the points were placed on
    pub reference: Option<i64>,
    pub params: TrackParams,
    pub result: Option<Track>,
    pub opts: ApplyOpts,
    dragging: Option<usize>,
}

impl Default for TrackerState {
    fn default() -> Self {
        TrackerState { points: Vec::new(), reference: None, params: TrackParams::default(), result: None, opts: ApplyOpts::default(), dragging: None }
    }
}

pub struct TrackJob {
    pub progress: Arc<AtomicU32>,
    pub cancel: Arc<AtomicBool>,
    pub rx: Receiver<Result<Track, String>>,
    pub frames: usize,
}

const COL_A: Color32 = Color32::from_rgb(255, 120, 200);
const COL_B: Color32 = Color32::from_rgb(120, 220, 255);

impl App {
    /// Frames covered by the selected lines (frame numbers).
    fn track_range(&self) -> Option<(i64, i64)> {
        let p = self.player.as_ref()?;
        let fm = p.fc.frame_ms;
        let sel = self.selected();
        let lines: Vec<&subforge_core::Event> = sel.iter().filter_map(|&i| self.doc.sub.events.get(i)).collect();
        if lines.is_empty() {
            return None;
        }
        let start = lines.iter().map(|e| e.start).min()?;
        let end = lines.iter().map(|e| e.end).max()?;
        let k0 = (start as f64 / fm - 1e-6).ceil() as i64;
        let k1 = ((end as f64 / fm - 1e-6).ceil() as i64 - 1).max(k0);
        Some((k0, k1))
    }

    /// Track tool on the video: click = point A, Shift+click = point B, drag points to adjust.
    pub fn track_tool(&mut self, ui: &egui::Ui, resp: &Response, painter: &egui::Painter, vrect: Rect, t: i64) {
        let Some(p) = &self.player else { return };
        let (pw, ph) = self.doc.sub.play_res();
        let (sx, sy) = (vrect.width() / pw.max(1) as f32, vrect.height() / ph.max(1) as f32);
        let to_screen = |x: f32, y: f32| Pos2::new(vrect.left() + x * sx, vrect.top() + y * sy);
        let to_script = |q: Pos2| ((q.x - vrect.left()) / sx, (q.y - vrect.top()) / sy);
        let frame = p.fc.idx_of(t);
        let frame_px = vrect.width() / p.out_w.max(1) as f32; // screen px per tracking px
        let tr = &mut self.tracker;

        // path of the last result
        if let Some(res) = &tr.result {
            for (v, col) in [(Some(&res.a), COL_A), (res.b.as_ref(), COL_B)] {
                let Some(v) = v else { continue };
                let pts: Vec<Pos2> = v.iter().flatten().map(|s| to_screen(s.x, s.y)).collect();
                if pts.len() > 1 {
                    painter.add(egui::Shape::line(pts, Stroke::new(1.2, col.gamma_multiply(0.7))));
                }
                for s in v.iter().flatten().filter(|s| s.score < tr.params.min_score) {
                    painter.circle_filled(to_screen(s.x, s.y), 2.5, Color32::from_rgb(255, 60, 60));
                }
                let at = if col == COL_A { res.a_at(frame) } else { res.b_at(frame) };
                if let Some(s) = at {
                    painter.circle_stroke(to_screen(s.x, s.y), 6.0, Stroke::new(2.0, col));
                }
            }
        }
        // placed points with patch and search boxes
        let on_ref = tr.reference == Some(frame);
        for (k, (x, y)) in tr.points.iter().enumerate() {
            let c = to_screen(*x, *y);
            let col = if k == 0 { COL_A } else { COL_B };
            let col = if on_ref { col } else { col.gamma_multiply(0.45) };
            let patch = tr.params.patch as f32 * frame_px;
            let search = (tr.params.patch + 2 * tr.params.search) as f32 * frame_px;
            painter.rect_stroke(Rect::from_center_size(c, Vec2::splat(patch)), 0.0, Stroke::new(1.5, col), egui::StrokeKind::Middle);
            let sr = Rect::from_center_size(c, Vec2::splat(search));
            painter.add(egui::Shape::dashed_line(&[sr.left_top(), sr.right_top(), sr.right_bottom(), sr.left_bottom(), sr.left_top()], Stroke::new(1.0, col), 4.0, 4.0));
            painter.line_segment([c - Vec2::new(5.0, 0.0), c + Vec2::new(5.0, 0.0)], Stroke::new(1.0, col));
            painter.line_segment([c - Vec2::new(0.0, 5.0), c + Vec2::new(0.0, 5.0)], Stroke::new(1.0, col));
            painter.text(c + Vec2::new(patch / 2.0 + 4.0, -patch / 2.0), Align2::LEFT_TOP, if k == 0 { "A" } else { "B" }, FontId::proportional(13.0), col);
        }
        if !on_ref && !tr.points.is_empty() {
            painter.text(vrect.left_top() + Vec2::new(8.0, 8.0), Align2::LEFT_TOP, format!("points were placed on frame {}", tr.reference.unwrap_or(0)), FontId::proportional(12.0), Color32::from_white_alpha(200));
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);

        // interaction (only while paused)
        if self.player.as_ref().is_some_and(|p| p.is_playing()) {
            return;
        }
        let tr = &mut self.tracker;
        let near = |q: Pos2, pts: &[(f32, f32)]| pts.iter().position(|(x, y)| to_screen(*x, *y).distance(q) < 12.0);
        if resp.drag_started() {
            if let Some(q) = resp.interact_pointer_pos() {
                tr.dragging = if on_ref { near(q, &tr.points) } else { None };
            }
        }
        if let (Some(k), true, Some(q)) = (tr.dragging, resp.dragged(), resp.interact_pointer_pos()) {
            tr.points[k] = to_script(q);
            tr.result = None;
        }
        if resp.drag_stopped() {
            tr.dragging = None;
        }
        if resp.clicked() {
            if let Some(q) = resp.interact_pointer_pos() {
                let second = ui.input(|i| i.modifiers.shift);
                if tr.reference != Some(frame) {
                    // new reference frame: start over
                    tr.points.clear();
                    tr.reference = Some(frame);
                }
                let pt = to_script(q);
                if second && !tr.points.is_empty() {
                    if tr.points.len() == 1 {
                        tr.points.push(pt);
                    } else {
                        tr.points[1] = pt;
                    }
                } else if tr.points.is_empty() {
                    tr.points.push(pt);
                } else {
                    tr.points[0] = pt;
                }
                tr.result = None;
            }
        }
    }

    pub fn start_tracking(&mut self) {
        let Some(p) = &self.player else {
            self.flash_err("Open the video first");
            return;
        };
        let (Some(reference), false) = (self.tracker.reference, self.tracker.points.is_empty()) else {
            self.flash_err("Place a point on the video with the Track tool first");
            return;
        };
        let Some((mut k0, mut k1)) = self.track_range() else {
            self.flash_err("Select the line(s) to track");
            return;
        };
        k0 = k0.min(reference);
        k1 = k1.max(reference);
        if k1 - k0 > 20_000 {
            self.flash_err("Range too long to track (max ~20 000 frames)");
            return;
        }
        let (pw, ph) = self.doc.sub.play_res();
        let (fw, fh) = (p.out_w as usize, p.out_h as usize);
        let (to_fx, to_fy) = (fw as f32 / pw as f32, fh as f32 / ph as f32);
        let pts: Vec<(f32, f32)> = self.tracker.points.iter().map(|(x, y)| (x * to_fx, y * to_fy)).collect();
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p2, c2) = (progress.clone(), cancel.clone());
        let (ff, video, fps, params) = (self.tools.ffmpeg.clone(), p.path.clone(), p.info.fps, self.tracker.params);
        std::thread::spawn(move || {
            let r = track::track_video(&ff, &video, fps, (fw, fh), k0, k1, reference, &pts, params, |d, t| {
                p2.store((d * 1000 / t.max(1)) as u32, Ordering::Relaxed);
                !c2.load(Ordering::Relaxed)
            })
            .map(|mut v| {
                // frame pixels -> script coordinates
                for s in v.iter_mut().flatten().flatten() {
                    *s = Sample { x: s.x / to_fx, y: s.y / to_fy, score: s.score };
                }
                let b = if v.len() > 1 { Some(v.remove(1)) } else { None };
                Track { fps, first: k0, reference, a: v.remove(0), b }
            });
            let _ = tx.send(r);
        });
        self.loading.queue.push_back(crate::loading::Task::Track(TrackJob { progress, cancel, rx, frames: (k1 - k0 + 1) as usize }));
    }

    pub fn apply_tracking(&mut self) {
        let Some(tr) = self.tracker.result.clone() else { return };
        let opts = self.tracker.opts;
        let mut v = self.selected();
        v.sort_unstable();
        let mut warnings = Vec::new();
        let mut done = 0;
        self.modify(|s| {
            for &i in v.iter().rev() {
                let i = subforge_core::anim::find_source(s, i).unwrap_or(i);
                match track::apply_track(s, i, &tr, opts) {
                    Ok(r) => {
                        done += 1;
                        warnings.extend(r.warnings);
                    }
                    Err(e) => warnings.push(e),
                }
            }
        });
        self.motion.loaded_for = None;
        self.buf_for = None;
        if warnings.is_empty() {
            self.flash(format!("Attached {done} line(s) to the track"));
        } else {
            self.flash_err(format!("Attached {done} line(s); {}", warnings.join("; ")));
        }
    }

    /// "Motion tracking" section of the Position & Motion pane.
    pub fn tracking_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(RichText::new("Motion tracking").strong()).id_salt("track_hdr").default_open(true).show(ui, |ui| {
            if self.player.is_none() {
                ui.label(RichText::new("Open the video to track motion.").weak());
                return;
            }
            ui.label(
                RichText::new("1. Select the line(s) to attach  2. Pick the Track tool (T) and click a distinct, high-contrast spot (Shift+click a 2nd spot for scale & rotation)  3. Track  4. Apply")
                    .small()
                    .weak(),
            );
            ui.horizontal_wrapped(|ui| {
                let on = self.vtool == crate::motion::VideoTool::Track;
                if ui.add(egui::Button::selectable(on, (crate::icons::ic(icons::ICON_GPS_FIXED), "Track tool"))).clicked() {
                    self.vtool = if on { crate::motion::VideoTool::Pointer } else { crate::motion::VideoTool::Track };
                }
                let tr = &self.tracker;
                match (tr.points.len(), tr.reference) {
                    (0, _) => {
                        ui.label(RichText::new("no points yet").weak());
                    }
                    (n, Some(r)) => {
                        ui.label(format!("{n} point{} on frame {r}", if n > 1 { "s" } else { "" }));
                        if ui.small_button("go there").clicked() {
                            if let Some(p) = &self.player {
                                let t = p.fc.time_of(r);
                                self.seek(t);
                            }
                        }
                    }
                    _ => {}
                }
            });
            match self.track_range() {
                Some((a, b)) => ui.label(RichText::new(format!("Range: frames {a}–{b} ({} frames, from the selected lines)", b - a + 1)).small()),
                None => ui.label(RichText::new("Select the line(s) to attach").small().color(Color32::from_rgb(225, 160, 50))),
            };
            let pr = &mut self.tracker.params;
            ui.horizontal_wrapped(|ui| {
                ui.label("Patch");
                ui.add(egui::DragValue::new(&mut pr.patch).range(9..=121).speed(1).suffix(" px")).on_hover_text("Size of the area that is matched (in preview pixels). Bigger = steadier, smaller = follows small details");
                ui.label("Search");
                ui.add(egui::DragValue::new(&mut pr.search).range(4..=160).speed(1).suffix(" px")).on_hover_text("How far the point may move between frames (beyond the predicted motion)");
                ui.label("Min match");
                ui.add(egui::DragValue::new(&mut pr.min_score).range(0.2..=0.99).speed(0.01).max_decimals(2));
            });
            pr.patch |= 1;
            ui.horizontal_wrapped(|ui| {
                let ready = !self.tracker.points.is_empty() && self.track_range().is_some();
                if ui.add_enabled(ready, btn(icons::ICON_TRACK_CHANGES, RichText::new("Track").strong())).clicked() {
                    self.start_tracking();
                }
                if ui.add_enabled(!self.tracker.points.is_empty() || self.tracker.result.is_some(), btn(icons::ICON_DELETE, "Clear")).clicked() {
                    self.tracker.points.clear();
                    self.tracker.reference = None;
                    self.tracker.result = None;
                }
            });
            if let Some(res) = &self.tracker.result {
                let weak = res.weak_frames(self.tracker.params.min_score);
                ui.horizontal_wrapped(|ui| {
                    ui.label(ic_s(icons::ICON_CHECK_CIRCLE).color(Color32::from_rgb(90, 190, 110)));
                    ui.label(format!("Tracked frames {}–{}", res.first, res.last()));
                    if weak > 0 {
                        ui.label(RichText::new(format!("{weak} weak frame(s) (red dots) — try a bigger patch or another spot")).small().color(Color32::from_rgb(225, 160, 50)));
                    }
                });
                let two = res.b.is_some();
                let o = &mut self.tracker.opts;
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut o.position, "Position");
                    ui.add_enabled(two, egui::Checkbox::new(&mut o.scale, "Scale")).on_disabled_hover_text("Track 2 points for scale");
                    ui.add_enabled(two, egui::Checkbox::new(&mut o.rotation, "Rotation")).on_disabled_hover_text("Track 2 points for rotation");
                });
                if ui.add(btn(icons::ICON_DONE, RichText::new(format!("Apply to {} selected line(s)", self.sel.len().max(1))).strong())).clicked() {
                    self.apply_tracking();
                }
                ui.label(RichText::new("Lines become one event per frame (the original is kept as a comment — Typeset ▸ Remove animation undoes it).").small().weak());
            }
        });
    }
}
