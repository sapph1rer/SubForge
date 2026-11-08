//! Video preview: libass-exact rendering (paused and while playing), a fast overlay fallback,
//! the drag-to-position tool, safe-area guides and transport controls.

use crate::app::App;
use crate::icons::{self, ibtn, ic, ic_s, itoggle};
use crate::media::PlaySubs;
use crate::motion::VideoTool;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use subforge_core::tags;
use subforge_core::time::fmt_display;

impl App {
    pub fn video_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.update_accurate_preview(&ctx);
        self.sync_play_subs();
        // pick the frame to show: libass render for the current doc version, else the plain frame
        let mut showing_rendered = false;
        if let Some(p) = &self.player {
            let gen = self.doc.gen;
            let pick = match (&p.rendered, &p.plain) {
                (Some(r), _) if r.rendered == Some(gen) && self.settings.libass_preview => {
                    showing_rendered = true;
                    Some(r)
                }
                (_, Some(f)) => Some(f),
                _ => None,
            };
            if p.is_playing() && p.play_subs.is_some() {
                showing_rendered = true; // playback frames already contain libass subtitles
            }
            if let Some(f) = pick {
                // zero-copy: egui takes the shared Arc<ColorImage>; only upload when the image changed
                let ptr = std::sync::Arc::as_ptr(&f.image) as usize;
                if ptr != self.video_tex_ptr {
                    match &mut self.video_tex {
                        Some(t) => t.set(f.image.clone(), egui::TextureOptions::LINEAR),
                        None => self.video_tex = Some(ctx.load_texture("video", f.image.clone(), egui::TextureOptions::LINEAR)),
                    }
                    self.video_tex_ptr = ptr;
                }
            }
        }

        // ---- toolbar
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if ui.add(itoggle(self.vtool == VideoTool::Pointer, icons::ICON_ARROW_SELECTOR_TOOL)).on_hover_text("Pointer — click to play/pause").clicked() {
                self.vtool = VideoTool::Pointer;
            }
            if ui.add(itoggle(self.vtool == VideoTool::Move, icons::ICON_OPEN_WITH)).on_hover_text("Move — drag selected lines to position them (\\pos). Shift = lock axis. (M)").clicked() {
                self.vtool = VideoTool::Move;
            }
            if ui.add(itoggle(self.vtool == VideoTool::Rotate, icons::ICON_ROTATE_RIGHT)).on_hover_text("Rotate — drag around the line's anchor (\\frz). Shift = 15° steps. (R)").clicked() {
                self.vtool = VideoTool::Rotate;
            }
            if ui.add(itoggle(self.vtool == VideoTool::Scale, icons::ICON_OPEN_IN_FULL)).on_hover_text("Scale — drag away from / towards the anchor (\\fscx \\fscy). Shift = X and Y separately. (S)").clicked() {
                self.vtool = VideoTool::Scale;
            }
            if ui.add(itoggle(self.vtool == VideoTool::Clip, icons::ICON_POLYLINE)).on_hover_text("Clip — draw \\clip on the video (C)").clicked() {
                self.vtool = VideoTool::Clip;
            }
            if ui.add(itoggle(self.vtool == VideoTool::Track, icons::ICON_GPS_FIXED)).on_hover_text("Track — click a point to follow (Shift+click = 2nd point for scale/rotation) (T)").clicked() {
                self.vtool = VideoTool::Track;
                self.show_pane(crate::layout::Pane::Motion);
            }
            if self.vtool == VideoTool::Clip {
                ui.separator();
                ui.selectable_value(&mut self.typeset.clip_mode, crate::typeset::ClipMode::Vector, (ic_s(icons::ICON_POLYLINE), "Vector")).on_hover_text("Click to add points (on an edge to insert), drag points, right-click a point to delete");
                ui.selectable_value(&mut self.typeset.clip_mode, crate::typeset::ClipMode::Rect, (ic_s(icons::ICON_CROP_SQUARE), "Rect")).on_hover_text("Drag to draw; drag corners to resize, inside to move");
                if ui.add(ibtn(icons::ICON_SWAP_HORIZ)).on_hover_text("Invert (\\clip ↔ \\iclip)").clicked() {
                    self.toggle_clip_inverse();
                }
                if ui.add(ibtn(icons::ICON_DELETE)).on_hover_text("Remove clip from selected lines").clicked() {
                    self.clear_clip();
                }
            }
            ui.separator();
            if ui.add(itoggle(self.settings.show_safe_area, icons::ICON_GRID_ON)).on_hover_text("Safe-area guides & centre lines").clicked() {
                self.settings.show_safe_area = !self.settings.show_safe_area;
            }
            ui.add_enabled_ui(self.has_libass, |ui| {
                if ui.add(itoggle(self.settings.libass_preview, icons::ICON_SUBTITLES)).on_hover_text("Exact libass rendering when paused").on_disabled_hover_text("this ffmpeg has no libass").clicked() {
                    self.settings.libass_preview = !self.settings.libass_preview;
                }
                if ui.add(itoggle(self.settings.libass_playback, icons::ICON_PLAYLIST_PLAY)).on_hover_text("Exact libass rendering while playing (heavier on CPU)").clicked() {
                    self.settings.libass_playback = !self.settings.libass_playback;
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(p) = &self.player {
                    let (pw, ph) = self.doc.sub.play_res();
                    ui.label(RichText::new(format!("{}×{} · {:.3} fps · script {}×{}", p.info.width, p.info.height, p.info.fps, pw, ph)).small().weak());
                }
            });
        });

        let controls_h = 64.0;
        let avail = ui.available_size();
        let area = Vec2::new(avail.x, (avail.y - controls_h).max(60.0));
        let sense = if self.vtool == VideoTool::Pointer { Sense::click() } else { Sense::click_and_drag() };
        let (rect, resp) = ui.allocate_exact_size(area, sense);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_gray(12));

        let Some(player) = &self.player else {
            painter.text(rect.center() - Vec2::new(0.0, 30.0), Align2::CENTER_CENTER, ic(icons::ICON_MOVIE).size(42.0).text(), FontId::new(42.0, egui::FontFamily::Name(egui_material_icons::FONT_FAMILY.into())), Color32::from_gray(90));
            painter.text(rect.center() + Vec2::new(0.0, 6.0), Align2::CENTER_CENTER, "No video loaded", FontId::proportional(17.0), Color32::from_gray(150));
            painter.text(
                rect.center() + Vec2::new(0.0, 28.0),
                Align2::CENTER_CENTER,
                if self.ffmpeg_ok { "Click to open a video, or drop one here" } else { "ffmpeg not found — set its folder in Settings" },
                FontId::proportional(13.0),
                Color32::from_gray(110),
            );
            if resp.clicked() {
                self.open_video_dialog(&ctx);
            }
            return;
        };

        // fit video
        let info = &player.info;
        let aspect = if player.out_h > 0 { player.out_w as f32 / player.out_h as f32 } else { 16.0 / 9.0 };
        let mut size = Vec2::new(rect.width(), rect.width() / aspect);
        if size.y > rect.height() {
            size = Vec2::new(rect.height() * aspect, rect.height());
        }
        let vrect = Rect::from_center_size(rect.center(), size);
        if let Some(tex) = &self.video_tex {
            painter.image(tex.id(), vrect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        } else if !info.has_video {
            painter.text(vrect.center(), Align2::CENTER_CENTER, "audio only", FontId::proportional(18.0), Color32::from_gray(140));
        }
        let t = player.time();
        let playing = player.is_playing();
        if showing_rendered {
            painter.text(vrect.right_top() + Vec2::new(-6.0, 6.0), Align2::RIGHT_TOP, "libass", FontId::proportional(10.0), Color32::from_white_alpha(140));
        } else {
            self.draw_overlay(&painter, vrect, t);
        }
        if self.settings.show_safe_area {
            draw_guides(&painter, vrect);
        }

        // ---- position handles + move tool
        let (pw, ph) = self.doc.sub.play_res();
        let sx = vrect.width() / pw.max(1) as f32;
        let sy = vrect.height() / ph.max(1) as f32;
        let to_screen = |x: f32, y: f32| Pos2::new(vrect.left() + x * sx, vrect.top() + y * sy);
        let show_handles = matches!(self.vtool, VideoTool::Move | VideoTool::Rotate | VideoTool::Scale) || self.settings.show_safe_area;
        if show_handles && !playing {
            for &i in self.sel.iter().take(50) {
                let Some(e) = self.doc.sub.events.get(i) else { continue };
                if e.comment {
                    continue;
                }
                let visible = e.start <= t && t < e.end;
                if let Some(m) = tags::parse_move(&e.text) {
                    let (a, b) = (to_screen(m.x1, m.y1), to_screen(m.x2, m.y2));
                    painter.add(egui::Shape::dashed_line(&[a, b], Stroke::new(1.5, Color32::from_rgb(255, 190, 90)), 6.0, 4.0));
                    painter.circle_stroke(a, 4.0, Stroke::new(1.5, Color32::from_rgb(255, 190, 90)));
                    painter.circle_filled(b, 4.0, Color32::from_rgb(255, 190, 90));
                }
                let (x, y) = tags::position_at(&self.doc.sub, e, t.clamp(e.start, e.end - 1));
                let p = to_screen(x, y);
                let col = if visible { Color32::from_rgb(90, 200, 255) } else { Color32::from_rgba_unmultiplied(90, 200, 255, 110) };
                painter.circle_stroke(p, 7.0, Stroke::new(2.0, col));
                painter.line_segment([p - Vec2::new(11.0, 0.0), p + Vec2::new(11.0, 0.0)], Stroke::new(1.0, col));
                painter.line_segment([p - Vec2::new(0.0, 11.0), p + Vec2::new(0.0, 11.0)], Stroke::new(1.0, col));
                if self.cur == Some(i) {
                    painter.text(p + Vec2::new(10.0, 10.0), Align2::LEFT_TOP, format!("{:.0}, {:.0}  \\an{}", x, y, tags::alignment(&self.doc.sub, e)), FontId::monospace(11.0), col);
                }
            }
        }

        match self.vtool {
            VideoTool::Pointer => {
                if resp.clicked() {
                    if let Some(p) = &mut self.player {
                        p.toggle();
                    }
                }
            }
            VideoTool::Rotate | VideoTool::Scale => {
                if resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let best = self
                            .doc
                            .sub
                            .active_at(t)
                            .map(|(i, e)| {
                                let (x, y) = tags::position_at(&self.doc.sub, e, t);
                                (i, to_screen(x, y).distance(pos))
                            })
                            .filter(|(_, d)| *d < 120.0)
                            .min_by(|a, b| a.1.total_cmp(&b.1));
                        if let Some((i, _)) = best {
                            let m = ui.input(|i| i.modifiers);
                            self.click_row(i, m);
                            self.scroll_to = Some(i);
                        }
                    }
                }
                if !playing {
                    let rotate = self.vtool == VideoTool::Rotate;
                    self.transform_tool(ui, &resp, &painter, vrect, t, rotate);
                }
            }
            VideoTool::Clip => {
                if !playing {
                    self.clip_tool(ui, &resp, &painter, vrect);
                }
            }
            VideoTool::Track => {
                self.track_tool(ui, &resp, &painter, vrect, t);
            }
            VideoTool::Move => {
                if resp.hovered() {
                    ctx.set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Move });
                }
                if resp.clicked() {
                    // select the visible line whose anchor is closest to the click
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let best = self
                            .doc
                            .sub
                            .active_at(t)
                            .map(|(i, e)| {
                                let (x, y) = tags::position_at(&self.doc.sub, e, t);
                                (i, to_screen(x, y).distance(pos))
                            })
                            .filter(|(_, d)| *d < 120.0)
                            .min_by(|a, b| a.1.total_cmp(&b.1));
                        if let Some((i, _)) = best {
                            let m = ui.input(|i| i.modifiers);
                            self.click_row(i, m);
                            self.scroll_to = Some(i);
                        }
                    }
                }
                if resp.drag_started() {
                    self.drag_axis = None;
                    // dragging an unselected visible line picks it first
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let hit_sel = self.sel.iter().any(|&i| {
                            self.doc.sub.events.get(i).is_some_and(|e| {
                                let (x, y) = tags::position_at(&self.doc.sub, e, t);
                                to_screen(x, y).distance(pos) < 120.0
                            })
                        });
                        if !hit_sel {
                            let best = self
                                .doc
                                .sub
                                .active_at(t)
                                .map(|(i, e)| {
                                    let (x, y) = tags::position_at(&self.doc.sub, e, t);
                                    (i, to_screen(x, y).distance(pos))
                                })
                                .min_by(|a, b| a.1.total_cmp(&b.1));
                            if let Some((i, _)) = best {
                                self.select_only(Some(i));
                            }
                        }
                    }
                }
                if resp.dragged() {
                    let mut d = resp.drag_delta();
                    if ui.input(|i| i.modifiers.shift) {
                        let total = resp.total_drag_delta().unwrap_or(d);
                        let axis = *self.drag_axis.get_or_insert(if total.x.abs() >= total.y.abs() { 0 } else { 1 });
                        if axis == 0 {
                            d.y = 0.0;
                        } else {
                            d.x = 0.0;
                        }
                    }
                    if d != Vec2::ZERO {
                        self.nudge_selected(d.x / sx, d.y / sy);
                    }
                }
                if resp.drag_stopped() {
                    self.end_text_session();
                    self.drag_axis = None;
                }
            }
        }

        // ---- controls
        ui.add_space(2.0);
        let dur = self.player.as_ref().map(|p| p.info.duration_ms).unwrap_or(0);
        let mut seek_to: Option<i64> = None;
        let mut step = 0i64;
        let mut pos = t as f64;
        ui.spacing_mut().slider_width = (ui.available_width() - 8.0).max(50.0);
        let sl = ui.add(egui::Slider::new(&mut pos, 0.0..=dur.max(1) as f64).show_value(false).trailing_fill(true));
        if sl.changed() {
            seek_to = Some(pos as i64);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let kfs = self.keyframes();
            if ui.add_enabled(kfs.is_some(), ibtn(icons::ICON_FIRST_PAGE)).on_hover_text("Previous keyframe (Alt+←)").clicked() {
                seek_to = kfs.as_ref().and_then(|k| subforge_core::timing::prev_keyframe(k, t));
            }
            if ui.add(ibtn(icons::ICON_FAST_REWIND)).on_hover_text("−5 s").clicked() {
                seek_to = Some(t - 5000);
            }
            if ui.add(ibtn(icons::ICON_CHEVRON_LEFT)).on_hover_text("Previous frame (Ctrl+←)").clicked() {
                step = -1;
            }
            if ui.add(ibtn(if playing { icons::ICON_PAUSE } else { icons::ICON_PLAY_ARROW })).on_hover_text("Play / pause (Space)").clicked() {
                if let Some(p) = &mut self.player {
                    p.toggle();
                }
            }
            if ui.add(ibtn(icons::ICON_CHEVRON_RIGHT)).on_hover_text("Next frame (Ctrl+→)").clicked() {
                step = 1;
            }
            if ui.add(ibtn(icons::ICON_FAST_FORWARD)).on_hover_text("+5 s").clicked() {
                seek_to = Some(t + 5000);
            }
            if ui.add_enabled(kfs.is_some(), ibtn(icons::ICON_LAST_PAGE)).on_hover_text("Next keyframe (Alt+→)").clicked() {
                seek_to = kfs.as_ref().and_then(|k| subforge_core::timing::next_keyframe(k, t));
            }
            ui.add_space(6.0);
            ui.label(RichText::new(format!("{} / {}", fmt_display(t), fmt_display(dur))).monospace());
            if let Some(p) = &self.player {
                use std::sync::atomic::Ordering::Relaxed;
                let frame = p.fc.idx_of(t);
                let on_kf = kfs.as_ref().is_some_and(|k| k.binary_search_by(|x| p.fc.idx_of(*x).cmp(&frame)).is_ok());
                let mut lbl = RichText::new(format!("f{}", frame)).monospace();
                lbl = if on_kf { lbl.color(Color32::from_rgb(190, 110, 230)) } else { lbl.weak() };
                ui.label(lbl).on_hover_text(format!(
                    "{}\ndecoder spawns: {}  cache hits: {}  last seek: {:.1} ms",
                    if on_kf { "keyframe" } else { "frame" },
                    p.stats.spawns.load(Relaxed),
                    p.stats.cache_hits.load(Relaxed),
                    p.stats.last_seek_us.load(Relaxed) as f64 / 1000.0
                ));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(p) = &mut self.player {
                    let mut v = p.volume();
                    ui.spacing_mut().slider_width = 80.0;
                    if ui.add(egui::Slider::new(&mut v, 0.0..=1.5).show_value(false)).on_hover_text("Volume").changed() {
                        p.set_volume(v);
                        self.settings.volume = v;
                    }
                    ui.label(ic_s(if !p.has_audio_out() || v == 0.0 { icons::ICON_VOLUME_OFF } else { icons::ICON_VOLUME_UP }));
                }
            });
        });
        if let Some(s) = seek_to {
            self.seek(s);
        }
        if step != 0 {
            self.step_frame(step);
        }
    }

    /// Keyframes for the timeline/snapping: a loaded keyframe file wins over the video's own.
    pub fn keyframes(&self) -> Option<std::sync::Arc<Vec<i64>>> {
        self.kf_override.clone().or_else(|| self.player.as_ref().and_then(|p| p.keyframes()))
    }

    /// Write `preview.ass` for libass if the document changed since the last write.
    pub fn ensure_preview_file(&mut self) -> Option<std::path::PathBuf> {
        let dir = self.player.as_ref()?.work_dir.clone()?;
        if self.preview.written_gen != self.doc.gen || !dir.join("preview.ass").exists() {
            let mut sub = self.doc.sub.clone();
            subforge_core::prepare_for(&mut sub, subforge_core::Format::Ass);
            let _ = std::fs::create_dir_all(&dir);
            let _ = subforge_core::save(dir.join("preview.ass"), &sub, Some(subforge_core::Format::Ass), &Default::default());
            self.preview.written_gen = self.doc.gen;
        }
        Some(dir)
    }

    /// Keep libass playback in sync with settings and edits (restarts only the video decoder).
    fn sync_play_subs(&mut self) {
        let Some(p) = &self.player else { return };
        let want = self.settings.libass_playback && self.has_libass && p.info.has_video && p.work_dir.is_some();
        let playing = p.is_playing();
        if !want {
            if p.play_subs.is_some() {
                let p = self.player.as_mut().unwrap();
                p.play_subs = None;
                p.restart_video();
            }
            self.preview.was_playing = playing;
            return;
        }
        let just_started = playing && !self.preview.was_playing;
        self.preview.was_playing = playing;
        let stale = self.preview.play_gen != self.doc.gen;
        if self.preview.last_edit_gen != self.doc.gen {
            self.preview.last_edit_gen = self.doc.gen;
            self.preview.last_edit = std::time::Instant::now();
        }
        let settled = self.preview.last_edit.elapsed() > std::time::Duration::from_millis(350);
        if p.play_subs.is_none() || just_started && stale || playing && stale && settled {
            let Some(dir) = self.ensure_preview_file() else { return };
            let p = self.player.as_mut().unwrap();
            let cfg = PlaySubs { dir, start_offset_ms: p.info.start_ms, orig_w: p.info.width, orig_h: p.info.height };
            p.play_subs = Some(cfg);
            p.restart_video();
            self.preview.play_gen = self.doc.gen;
        }
    }

    /// When paused, request a libass render of the current frame once things settle (debounced).
    fn update_accurate_preview(&mut self, ctx: &egui::Context) {
        let Some(p) = &self.player else { return };
        if !self.settings.libass_preview || !self.has_libass || p.is_playing() || !p.info.has_video || p.work_dir.is_none() {
            return;
        }
        let t = p.time();
        let key = (p.fc.idx_of(t), self.doc.gen);
        if p.rendered.as_ref().is_some_and(|r| r.rendered == Some(key.1) && r.idx == key.0) {
            return;
        }
        if self.preview.last_key != key {
            self.preview.last_key = key;
            self.preview.since = std::time::Instant::now();
        }
        if self.preview.requested == Some(key) {
            return;
        }
        let wait = std::time::Duration::from_millis(200);
        let el = self.preview.since.elapsed();
        if el < wait {
            ctx.request_repaint_after(wait - el);
            return;
        }
        if self.ensure_preview_file().is_none() {
            return;
        }
        let p = self.player.as_ref().unwrap();
        p.request_render(p.fc.time_of(key.0), key.1);
        self.preview.requested = Some(key);
    }

    /// Fast approximate renderer (used while editing/playing when libass output isn't current).
    /// Honours style colours/borders/margins, \an, \pos, \move, \fs and \1c.
    fn draw_overlay(&self, painter: &egui::Painter, vrect: Rect, t: i64) {
        let sub = &self.doc.sub;
        let (pw, ph) = sub.play_res();
        let scale = vrect.height() / ph.max(1) as f32;
        let sx = vrect.width() / pw.max(1) as f32;
        let mut stack_offset = [0.0f32; 10];
        let active: Vec<_> = sub.active_at(t).map(|(_, e)| e).collect();
        for e in active.iter().rev() {
            let plain = e.plain().replace('\u{a0}', " ");
            if plain.trim().is_empty() {
                continue;
            }
            let style = sub.style(&e.style).cloned().unwrap_or_default();
            let align = tags::alignment(sub, e).clamp(1, 9) as usize;
            let fs = tags::get_tag(&e.text, "fs").and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(style.fontsize);
            let size = (fs * scale * style.scale_y / 100.0).clamp(6.0, 300.0);
            let pc = tags::get_tag(&e.text, "1c").and_then(|c| subforge_core::Color::parse_ass(&c)).map(|c| subforge_core::Color { a: style.primary.a, ..c }).unwrap_or(style.primary);
            let col = Color32::from_rgba_unmultiplied(pc.r, pc.g, pc.b, 255 - pc.a);
            let outline = Color32::from_rgba_unmultiplied(style.outline.r, style.outline.g, style.outline.b, 255 - style.outline.a);
            let ml = (if e.margin_l > 0 { e.margin_l } else { style.margin_l }) as f32 * sx;
            let mr = (if e.margin_r > 0 { e.margin_r } else { style.margin_r }) as f32 * sx;
            let mv = (if e.margin_v > 0 { e.margin_v } else { style.margin_v }) as f32 * scale;
            let explicit = tags::explicit_position(e).is_some();
            let wrap = if explicit { vrect.width() } else { (vrect.width() - ml - mr).max(50.0) };
            let mut job = egui::text::LayoutJob::simple(plain, FontId::proportional(size), col, wrap);
            job.halign = match align {
                1 | 4 | 7 => egui::Align::Min,
                3 | 6 | 9 => egui::Align::Max,
                _ => egui::Align::Center,
            };
            let galley = painter.layout_job(job);
            let gs = galley.rect.size();
            let pos = if explicit {
                let (x, y) = tags::position_at(sub, e, t);
                let (ax, ay) = (vrect.left() + x * sx, vrect.top() + y * scale);
                // galley x is the alignment anchor already (halign); y from the top of the text block
                let y0 = match align {
                    7..=9 => ay,
                    4..=6 => ay - gs.y / 2.0,
                    _ => ay - gs.y,
                };
                Pos2::new(ax, y0)
            } else {
                let x = match align {
                    1 | 4 | 7 => vrect.left() + ml,
                    3 | 6 | 9 => vrect.right() - mr,
                    _ => vrect.left() + ml + wrap / 2.0,
                };
                let off = stack_offset[align];
                let y = match align {
                    7..=9 => vrect.top() + mv + off,
                    4..=6 => vrect.center().y - gs.y / 2.0 + off,
                    _ => vrect.bottom() - mv - gs.y - off,
                };
                stack_offset[align] += gs.y + 2.0;
                Pos2::new(x, y)
            };
            let ow = (style.outline_w * scale).clamp(0.8, 6.0);
            if style.border_style == 3 {
                let r = galley.rect.translate(pos.to_vec2()).expand(ow);
                painter.rect_filled(r, 2.0, Color32::from_rgba_unmultiplied(style.outline.r, style.outline.g, style.outline.b, 200));
            } else {
                let sh = (style.shadow * scale).min(6.0);
                if sh > 0.0 {
                    painter.galley_with_override_text_color(pos + Vec2::splat(sh), galley.clone(), Color32::from_black_alpha(160));
                }
                for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (-0.7, -0.7), (0.7, -0.7), (-0.7, 0.7), (0.7, 0.7)] {
                    painter.galley_with_override_text_color(pos + Vec2::new(dx * ow, dy * ow), galley.clone(), outline);
                }
            }
            painter.galley(pos, galley, col);
        }
    }
}

/// Title/action safe areas (80% / 90%) and centre lines.
fn draw_guides(p: &egui::Painter, r: Rect) {
    let c = Color32::from_rgba_unmultiplied(255, 255, 255, 70);
    let dash = |a: Pos2, b: Pos2| egui::Shape::dashed_line(&[a, b], Stroke::new(1.0, c), 5.0, 5.0);
    for f in [0.9f32, 0.8] {
        let s = Rect::from_center_size(r.center(), r.size() * f);
        p.rect_stroke(s, 0.0, Stroke::new(1.0, c), egui::StrokeKind::Inside);
    }
    p.add(dash(Pos2::new(r.center().x, r.top()), Pos2::new(r.center().x, r.bottom())));
    p.add(dash(Pos2::new(r.left(), r.center().y), Pos2::new(r.right(), r.center().y)));
}
