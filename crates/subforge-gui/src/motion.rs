//! "Position & Motion" pane: easy placement (numpad alignment, X/Y, nudging, drag on video),
//! fades, and a tween editor with presets, easing curves and live preview.

use crate::app::App;
use crate::icons::{self, btn, ibtn, ic, ic_s};
use eframe::egui::{self, Color32, Pos2, RichText, Sense, Stroke, Vec2};
use subforge_core::anim::{self, EaseDir, EaseKind, Mode, Preset, State, Tween};
use subforge_core::tags;

#[derive(Default)]
pub struct MotionState {
    pub draft: Tween,
    /// (line, doc generation) the draft was loaded from
    pub loaded_for: Option<(usize, u64)>,
    pub has_tween: bool,
    pub warnings: Vec<String>,
    pub apply_to_selection: bool,
    pub live: bool,
    /// resting state the draft was built from (to follow edits made elsewhere)
    pub base: Option<State>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoTool {
    /// click = play/pause
    #[default]
    Pointer,
    /// drag = move the selected lines (\pos)
    Move,
    /// drag around the anchor = \frz
    Rotate,
    /// drag away from / towards the anchor = \fscx \fscy
    Scale,
    /// edit \clip / \iclip
    Clip,
    /// place points for motion tracking
    Track,
}

fn opacity_pct(a: u8) -> f32 {
    (255.0 - a as f32) / 255.0 * 100.0
}

fn color_edit(ui: &mut egui::Ui, c: &mut subforge_core::Color) -> bool {
    let mut c32 = Color32::from_rgb(c.r, c.g, c.b);
    let r = egui::color_picker::color_edit_button_srgba(ui, &mut c32, egui::color_picker::Alpha::Opaque);
    if r.changed() {
        let [r, g, b, _] = c32.to_srgba_unmultiplied();
        *c = subforge_core::Color { r, g, b, a: 0 };
        true
    } else {
        false
    }
}

/// Small plot of an easing curve.
fn curve_preview(ui: &mut egui::Ui, e: anim::Easing, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    let vis = ui.visuals();
    p.rect_filled(rect, 4.0, vis.extreme_bg_color);
    // leave room for overshoot (back/elastic)
    let inner = rect.shrink2(Vec2::new(6.0, size.y * 0.18));
    p.line_segment([inner.left_bottom(), inner.right_bottom()], Stroke::new(1.0, vis.weak_text_color().gamma_multiply(0.5)));
    p.line_segment([inner.left_top(), inner.right_top()], Stroke::new(1.0, vis.weak_text_color().gamma_multiply(0.5)));
    let pts: Vec<Pos2> = (0..=60)
        .map(|i| {
            let t = i as f32 / 60.0;
            let v = e.apply(t);
            Pos2::new(inner.left() + t * inner.width(), inner.bottom() - v * inner.height())
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(2.0, Color32::from_rgb(120, 170, 255))));
    // animated dot
    let t = (ui.input(|i| i.time) as f32 * 0.6) % 1.3;
    let t = t.min(1.0);
    let v = e.apply(t);
    p.circle_filled(Pos2::new(inner.left() + t * inner.width(), inner.bottom() - v * inner.height()), 3.5, Color32::from_rgb(255, 190, 90));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

impl App {
    pub fn fps_or_default(&self) -> f64 {
        self.player.as_ref().map(|p| p.info.fps).filter(|f| *f > 1.0).unwrap_or(24000.0 / 1001.0)
    }

    /// Reload the tween draft when the current line changes.
    fn motion_sync(&mut self) {
        let Some(c) = self.cur.filter(|&c| c < self.doc.sub.events.len()) else { return };
        match self.motion.loaded_for {
            Some((i, g)) if i == c && g == self.doc.gen => {}
            Some((i, _)) if i == c && !self.motion.has_tween => {
                // the line changed (moved, restyled...): keep untouched draft values in sync
                let src = anim::find_source(&self.doc.sub, c).unwrap_or(c);
                let here = State::of_event(&self.doc.sub, &self.doc.sub.events[src]);
                if let Some(base) = self.motion.base {
                    if self.motion.draft.from == base {
                        self.motion.draft.from = here;
                    }
                    if self.motion.draft.to == base {
                        self.motion.draft.to = here;
                    }
                }
                self.motion.base = Some(here);
                self.motion.loaded_for = Some((c, self.doc.gen));
            }
            Some((i, _)) if i == c => self.motion.loaded_for = Some((c, self.doc.gen)),
            _ => self.reload_motion_draft(),
        }
    }

    pub fn reload_motion_draft(&mut self) {
        let Some(c) = self.cur.filter(|&c| c < self.doc.sub.events.len()) else { return };
        let sub = &self.doc.sub;
        match anim::read_tween(sub, c) {
            Some(t) => {
                self.motion.draft = t;
                self.motion.has_tween = true;
            }
            None => {
                let src = anim::find_source(sub, c).unwrap_or(c);
                let here = State::of_event(sub, &sub.events[src]);
                let keep = self.motion.draft.clone();
                self.motion.draft = Tween { from: here, to: here, easing: keep.easing, mode: keep.mode, ..Default::default() };
                self.motion.has_tween = false;
                self.motion.base = Some(here);
            }
        }
        self.motion.warnings.clear();
        self.motion.loaded_for = Some((c, self.doc.gen));
    }

    /// Apply the draft tween to the current line (and optionally the selection).
    pub fn apply_motion(&mut self, play_after: bool) {
        let Some(c) = self.cur else { return };
        let fps = self.fps_or_default();
        let tw = self.motion.draft.clone();
        if !tw.animate.any() && tw.fade_in == 0 && tw.fade_out == 0 {
            self.flash_err("Nothing to animate — tick at least one property or set a fade");
            return;
        }
        let mut targets: Vec<usize> = if self.motion.apply_to_selection { self.selected() } else { vec![c] };
        targets.sort_unstable();
        targets.dedup();
        let single = targets.len() == 1;
        let res = self.modify(|s| {
            let mut warnings = Vec::new();
            let mut first = None;
            for &i in targets.iter().rev() {
                let i = anim::find_source(s, i).unwrap_or(i);
                let mut t = tw.clone();
                if !single {
                    // keep each line's own resting position; apply the draft's offsets
                    let here = State::of_event(s, &s.events[i]);
                    let (dx0, dy0) = (tw.from.x - tw.to.x, tw.from.y - tw.to.y);
                    t.to.x = here.x;
                    t.to.y = here.y;
                    t.from.x = here.x + dx0;
                    t.from.y = here.y + dy0;
                }
                match anim::apply_tween(s, i, &t, fps) {
                    Ok(r) => {
                        warnings.extend(r.warnings);
                        first = Some(r.index);
                    }
                    Err(e) => warnings.push(format!("line {}: {e}", i + 1)),
                }
            }
            (first, warnings)
        });
        let (first, mut warnings) = res;
        warnings.sort();
        warnings.dedup();
        self.motion.warnings = warnings;
        self.motion.has_tween = true;
        if let Some(i) = first {
            self.select_only(Some(i));
            self.motion.loaded_for = Some((i, self.doc.gen));
        }
        self.buf_for = None;
        if play_after {
            if let Some(src) = self.cur.and_then(|c| anim::find_source(&self.doc.sub, c)) {
                let e = &self.doc.sub.events[src];
                let (a, b) = (e.start, e.end);
                self.play_range(a, b);
            }
        }
    }

    pub fn remove_motion(&mut self) {
        let Some(c) = self.cur else { return };
        let r = self.modify(|s| anim::remove_tween(s, c));
        if let Some(i) = r {
            self.select_only(Some(i));
        }
        self.motion.loaded_for = None;
        self.reload_motion_draft();
        self.buf_for = None;
    }

    /// Move the selected lines by a delta in script pixels (one undo step per drag/session).
    pub fn nudge_selected(&mut self, dx: f32, dy: f32) {
        let v = self.selected();
        if v.is_empty() || (dx == 0.0 && dy == 0.0) {
            return;
        }
        if !self.text_session {
            self.checkpoint();
            self.text_session = true;
        }
        let snapshot = self.doc.sub.clone();
        for i in v {
            if let Some(e) = self.doc.sub.events.get_mut(i) {
                tags::nudge(&snapshot, e, dx, dy);
            }
        }
        self.touch();
        self.buf_for = None;
    }

    fn set_selected_alignment(&mut self, an: i32) {
        let v = self.selected();
        self.modify(|s| {
            for i in v {
                tags::set_alignment(&mut s.events[i], an);
            }
        });
        self.buf_for = None;
    }

    pub fn motion_ui(&mut self, ui: &mut egui::Ui) {
        self.motion_sync();
        let Some(c) = self.cur.filter(|&c| c < self.doc.sub.events.len()) else {
            ui.label(RichText::new("Select a line to position or animate it.").weak());
            return;
        };
        let (pw, ph) = self.doc.sub.play_res();
        let t_now = self.video_time().unwrap_or(self.doc.sub.events[c].start);
        let src = anim::find_source(&self.doc.sub, c).unwrap_or(c);
        let line = self.doc.sub.events[src].clone();
        let is_generated = src != c;

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("#{}", src + 1)).strong());
            if self.motion.has_tween {
                ui.label(ic_s(icons::ICON_ANIMATION).color(Color32::from_rgb(120, 170, 255)));
                ui.label(RichText::new(if self.motion.draft.mode == Mode::FrameByFrame { "animated (frame-by-frame)" } else { "animated (tags)" }).color(Color32::from_rgb(120, 170, 255)).small());
            }
            if is_generated {
                ui.label(RichText::new("generated frame — editing the source line").weak().small());
            }
            ui.label(RichText::new(format!("script {pw}×{ph}")).weak().small());
        });
        ui.add_space(2.0);

        // ------------------------------------------------ position
        egui::CollapsingHeader::new(RichText::new("Position").strong()).id_salt("pos_hdr").default_open(true).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                // numpad alignment
                let cur_an = tags::alignment(&self.doc.sub, &line);
                let has_an = tags::get_tag(&line.text, "an").is_some();
                let left = ui.vertical(|ui| {
                    ui.label(RichText::new("Alignment").small().weak());
                    egui::Grid::new("an_grid").spacing([2.0, 2.0]).show(ui, |ui| {
                        for row in [[7, 8, 9], [4, 5, 6], [1, 2, 3]] {
                            for a in row {
                                let r = ui.add(egui::Button::selectable(cur_an == a, RichText::new(a.to_string()).monospace()).min_size(Vec2::new(24.0, 22.0)));
                                if r.on_hover_text(format!("\\an{a}")).clicked() {
                                    self.set_selected_alignment(a);
                                }
                            }
                            ui.end_row();
                        }
                    });
                    if has_an {
                        ui.label(RichText::new("override").small().color(Color32::from_rgb(120, 170, 255)));
                    }
                });
                // a plain separator here would stretch to the whole pane height
                ui.add_sized([8.0, left.response.rect.height()], egui::Separator::default().vertical());
                ui.vertical(|ui| {
                    let explicit = tags::explicit_position(&line);
                    let (mut x, mut y) = explicit.unwrap_or_else(|| tags::default_anchor(&self.doc.sub, &line));
                    let (ox, oy) = (x, y);
                    ui.horizontal(|ui| {
                        ui.label("X");
                        ui.add(egui::DragValue::new(&mut x).speed(1.0).range(-(pw as f32)..=(pw as f32 * 2.0)).max_decimals(2));
                        ui.label("Y");
                        ui.add(egui::DragValue::new(&mut y).speed(1.0).range(-(ph as f32)..=(ph as f32 * 2.0)).max_decimals(2));
                    });
                    if (x, y) != (ox, oy) {
                        self.nudge_selected(x - ox, y - oy);
                    }
                    ui.label(RichText::new(match (explicit, tags::parse_move(&line.text)) {
                        (_, Some(_)) => "\\move — both ends move together",
                        (Some(_), None) => "\\pos",
                        _ => "from style margins (drag to set \\pos)",
                    }).small().weak());
                    ui.horizontal_wrapped(|ui| {
                        let step = if ui.input(|i| i.modifiers.shift) { 10.0 } else { 1.0 };
                        for (icon, dx, dy, tip) in [
                            (icons::ICON_ARROW_BACK, -step, 0.0, "left"),
                            (icons::ICON_ARROW_FORWARD, step, 0.0, "right"),
                            (icons::ICON_VERTICAL_ALIGN_TOP, 0.0, -step, "up"),
                            (icons::ICON_VERTICAL_ALIGN_BOTTOM, 0.0, step, "down"),
                        ] {
                            if ui.add(ibtn(icon)).on_hover_text(format!("Nudge {tip} 1 px (Shift = 10 px)")).clicked() {
                                self.nudge_selected(dx, dy);
                                self.end_text_session();
                            }
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        let move_on = self.vtool == VideoTool::Move;
                        if ui.add(egui::Button::selectable(move_on, (ic(icons::ICON_OPEN_WITH), "Drag on video"))).on_hover_text("Drag the selected lines directly on the video (M)").clicked() {
                            self.vtool = if move_on { VideoTool::Pointer } else { VideoTool::Move };
                        }
                        if ui.add(btn(icons::ICON_CENTER_FOCUS_STRONG, "Center")).clicked() {
                            let v = self.selected();
                            self.modify(|s| {
                                let snap = s.clone();
                                for i in v {
                                    let e = &mut s.events[i];
                                    tags::set_alignment(e, 5);
                                    tags::set_position(&snap, e, pw as f32 / 2.0, ph as f32 / 2.0);
                                }
                            });
                            self.buf_for = None;
                        }
                        if ui.add(btn(icons::ICON_RESTART_ALT, "Reset")).on_hover_text("Remove \\pos/\\move/\\an — the style decides").clicked() {
                            let v = self.selected();
                            self.modify(|s| {
                                for i in v {
                                    tags::clear_position(&mut s.events[i]);
                                }
                            });
                            self.buf_for = None;
                        }
                    });
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Margins L/R/V").small());
                let e = &self.doc.sub.events[src];
                let (mut l, mut r, mut v) = (e.margin_l, e.margin_r, e.margin_v);
                let a = ui.add(egui::DragValue::new(&mut l).range(0..=9999)).on_hover_text("0 = style margin");
                let b = ui.add(egui::DragValue::new(&mut r).range(0..=9999));
                let cc = ui.add(egui::DragValue::new(&mut v).range(0..=9999));
                if a.changed() || b.changed() || cc.changed() {
                    if !self.text_session {
                        self.checkpoint();
                        self.text_session = true;
                    }
                    let e = &mut self.doc.sub.events[src];
                    e.margin_l = l;
                    e.margin_r = r;
                    e.margin_v = v;
                    self.touch();
                }
                if a.drag_stopped() || b.drag_stopped() || cc.drag_stopped() || a.lost_focus() || b.lost_focus() || cc.lost_focus() {
                    self.end_text_session();
                }
            });
        });

        // ------------------------------------------------ presets
        egui::CollapsingHeader::new(RichText::new("Quick animations").strong()).id_salt("preset_hdr").default_open(true).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for p in Preset::ALL {
                    if ui.button(p.name()).on_hover_text("Applies to the selected lines and plays the line").clicked() {
                        let here = State::of_event(&self.doc.sub, &self.doc.sub.events[src]);
                        let mut t = p.build(here, (pw, ph), line.duration());
                        t.fade_in = self.motion.draft.fade_in;
                        t.fade_out = self.motion.draft.fade_out;
                        self.motion.draft = t;
                        let sel = self.motion.apply_to_selection;
                        self.motion.apply_to_selection = self.sel.len() > 1 || sel;
                        self.apply_motion(true);
                        self.motion.apply_to_selection = sel;
                    }
                }
            });
        });

        // ------------------------------------------------ tween editor
        let mut changed = false;
        egui::CollapsingHeader::new(RichText::new("Tween editor").strong()).id_salt("tween_hdr").default_open(true).show(ui, |ui| {
            let d = &mut self.motion.draft;
            let dur = line.duration();
            ui.horizontal_wrapped(|ui| {
                ui.label("Fade in");
                changed |= ui.add(egui::DragValue::new(&mut d.fade_in).range(0..=dur).speed(5).suffix(" ms")).changed();
                ui.label("out");
                changed |= ui.add(egui::DragValue::new(&mut d.fade_out).range(0..=dur).speed(5).suffix(" ms")).changed();
            });
            ui.add_space(4.0);
            egui::Grid::new("tween_grid").num_columns(4).spacing([8.0, 4.0]).striped(true).show(ui, |ui| {
                ui.label("");
                ui.label(RichText::new("Property").small().weak());
                ui.label(RichText::new("Start").small().weak());
                ui.label(RichText::new("End").small().weak());
                ui.end_row();

                // position
                changed |= ui.checkbox(&mut d.animate.pos, "").changed();
                ui.label("Position");
                ui.horizontal(|ui| {
                    changed |= ui.add(egui::DragValue::new(&mut d.from.x).speed(1.0).prefix("x ").max_decimals(1)).changed();
                    changed |= ui.add(egui::DragValue::new(&mut d.from.y).speed(1.0).prefix("y ").max_decimals(1)).changed();
                });
                ui.horizontal(|ui| {
                    changed |= ui.add(egui::DragValue::new(&mut d.to.x).speed(1.0).prefix("x ").max_decimals(1)).changed();
                    changed |= ui.add(egui::DragValue::new(&mut d.to.y).speed(1.0).prefix("y ").max_decimals(1)).changed();
                });
                ui.end_row();

                let row = |ui: &mut egui::Ui, on: &mut bool, name: &str, a: &mut f32, b: &mut f32, speed: f64, suffix: &str| {
                    let mut ch = ui.checkbox(on, "").changed();
                    ui.label(name);
                    ch |= ui.add(egui::DragValue::new(a).speed(speed).suffix(suffix).max_decimals(2)).changed();
                    ch |= ui.add(egui::DragValue::new(b).speed(speed).suffix(suffix).max_decimals(2)).changed();
                    ui.end_row();
                    ch
                };
                let mut on = d.animate.scale;
                let (mut a, mut b) = (d.from.scale_x, d.to.scale_x);
                changed |= row(ui, &mut on, "Scale", &mut a, &mut b, 0.5, " %");
                if on != d.animate.scale || a != d.from.scale_x || b != d.to.scale_x {
                    d.animate.scale = on;
                    // keep aspect: scale both axes together
                    let (ra, rb) = (a / d.from.scale_x.max(0.01), b / d.to.scale_x.max(0.01));
                    d.from.scale_y = if d.from.scale_x > 0.01 { d.from.scale_y * ra } else { a };
                    d.to.scale_y = if d.to.scale_x > 0.01 { d.to.scale_y * rb } else { b };
                    d.from.scale_x = a;
                    d.to.scale_x = b;
                }
                changed |= row(ui, &mut d.animate.rot_z, "Rotate Z", &mut d.from.rot_z, &mut d.to.rot_z, 0.5, "°");
                changed |= row(ui, &mut d.animate.rot_x, "Rotate X", &mut d.from.rot_x, &mut d.to.rot_x, 0.5, "°");
                changed |= row(ui, &mut d.animate.rot_y, "Rotate Y", &mut d.from.rot_y, &mut d.to.rot_y, 0.5, "°");
                changed |= row(ui, &mut d.animate.fs, "Font size", &mut d.from.fs, &mut d.to.fs, 0.5, "");
                changed |= row(ui, &mut d.animate.bord, "Border", &mut d.from.bord, &mut d.to.bord, 0.1, "");
                changed |= row(ui, &mut d.animate.blur, "Blur", &mut d.from.blur, &mut d.to.blur, 0.1, "");

                // opacity (shown as %, stored as ASS alpha)
                changed |= ui.checkbox(&mut d.animate.alpha, "").changed();
                ui.label("Opacity");
                for st in [&mut d.from, &mut d.to] {
                    let mut o = opacity_pct(st.alpha);
                    if ui.add(egui::DragValue::new(&mut o).range(0.0..=100.0).speed(0.5).suffix(" %").max_decimals(0)).changed() {
                        st.alpha = (255.0 - o / 100.0 * 255.0).round().clamp(0.0, 255.0) as u8;
                        changed = true;
                    }
                }
                ui.end_row();

                changed |= ui.checkbox(&mut d.animate.color, "").changed();
                ui.label("Colour");
                changed |= color_edit(ui, &mut d.from.color);
                changed |= color_edit(ui, &mut d.to.color);
                ui.end_row();
            });

            // grab positions from the line / video
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Set from current position:").small());
                let here = tags::position_at(&self.doc.sub, &line, t_now);
                if ui.small_button("→ Start").clicked() {
                    d.from.x = here.0;
                    d.from.y = here.1;
                    d.animate.pos = true;
                    changed = true;
                }
                if ui.small_button("→ End").clicked() {
                    d.to.x = here.0;
                    d.to.y = here.1;
                    d.animate.pos = true;
                    changed = true;
                }
                if ui.small_button("Swap start/end").clicked() {
                    std::mem::swap(&mut d.from, &mut d.to);
                    changed = true;
                }
            });

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(ic_s(icons::ICON_SCHEDULE));
                ui.label("From");
                changed |= ui.add(egui::DragValue::new(&mut d.t1).range(0..=dur).speed(5).suffix(" ms")).changed();
                let mut until_end = d.t2.is_none();
                ui.label("to");
                if until_end {
                    ui.label(RichText::new("line end").weak());
                } else {
                    let mut t2 = d.t2.unwrap_or(dur);
                    if ui.add(egui::DragValue::new(&mut t2).range(d.t1..=dur).speed(5).suffix(" ms")).changed() {
                        d.t2 = Some(t2);
                        changed = true;
                    }
                }
                if ui.checkbox(&mut until_end, "until end").changed() {
                    d.t2 = if until_end { None } else { Some((dur / 2).max(d.t1)) };
                    changed = true;
                }
                let local = (t_now - line.start).clamp(0, dur);
                if ui.small_button("start = playhead").on_hover_text("Animation starts at the current video time").clicked() {
                    d.t1 = local;
                    changed = true;
                }
                if ui.small_button("end = playhead").clicked() {
                    d.t2 = Some(local.max(d.t1));
                    changed = true;
                }
            });

            ui.add_space(4.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(ic_s(icons::ICON_LINE_CURVE));
                        ui.label("Easing");
                        egui::ComboBox::from_id_salt("ease_kind").selected_text(d.easing.kind.name()).width(130.0).show_ui(ui, |ui| {
                            for k in EaseKind::ALL {
                                changed |= ui.selectable_value(&mut d.easing.kind, k, k.name()).changed();
                            }
                        });
                    });
                    ui.horizontal_wrapped(|ui| {
                        for (dir, n) in [(EaseDir::In, "In"), (EaseDir::Out, "Out"), (EaseDir::InOut, "In-Out")] {
                            changed |= ui.selectable_value(&mut d.easing.dir, dir, n).changed();
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Output");
                        changed |= ui.selectable_value(&mut d.mode, Mode::Tags, "Tags").on_hover_text("\\move + \\t — one line, editable by hand").changed();
                        changed |= ui.selectable_value(&mut d.mode, Mode::FrameByFrame, "Frame-by-frame").on_hover_text("one line per frame — exact for every curve and for eased movement").changed();
                    });
                    if d.mode == Mode::Tags && (!d.easing.tag_compatible() || (d.animate.pos && d.easing.kind != EaseKind::Linear && d.from.x != d.to.x || d.animate.pos && d.easing.kind != EaseKind::Linear && d.from.y != d.to.y)) {
                        ui.label(RichText::new("Tip: this curve/movement is only exact in Frame-by-frame").small().color(Color32::from_rgb(225, 160, 50)));
                    }
                });
                curve_preview(ui, d.easing, Vec2::new(120.0, 78.0));
            });
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add(btn(icons::ICON_DONE, RichText::new("Apply").strong())).on_hover_text("Write the animation into the line").clicked() {
                self.apply_motion(false);
            }
            if ui.add(btn(icons::ICON_PLAY_ARROW, "Apply & play")).clicked() {
                self.apply_motion(true);
            }
            if ui.add_enabled(self.motion.has_tween, btn(icons::ICON_DELETE, "Remove animation")).clicked() {
                self.remove_motion();
            }
            if ui.add(btn(icons::ICON_REFRESH, "Reload")).on_hover_text("Reset the editor from the line").clicked() {
                self.motion.loaded_for = None;
                self.reload_motion_draft();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.motion.apply_to_selection, format!("Apply to all {} selected lines", self.sel.len().max(1)));
            ui.checkbox(&mut self.motion.live, "Live update").on_hover_text("Re-apply automatically while you edit values");
        });
        if changed && self.motion.live && self.motion.has_tween {
            self.apply_motion(false);
        }
        ui.add_space(6.0);
        self.tracking_ui(ui);
        for w in &self.motion.warnings {
            ui.label(RichText::new(w).small().color(Color32::from_rgb(225, 160, 50)));
        }
        ui.label(
            RichText::new(format!(
                "Frame-by-frame uses {:.3} fps{}. Animation settings are kept in a {{sftw:…}} comment block so you can edit them later.",
                self.fps_or_default(),
                if self.player.is_some() { " (video)" } else { " (no video — default)" }
            ))
            .small()
            .weak(),
        );
    }
}
