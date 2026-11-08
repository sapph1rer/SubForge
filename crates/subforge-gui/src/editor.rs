//! Line editor panel: times, style, actor, text with live CPS / line-length feedback.

use crate::app::App;
use crate::icons::{self, btn, ibtn};
use crate::grid::sev_color;
use eframe::egui::{self, RichText};
use subforge_core::qc::Severity;
use subforge_core::text::{count_chars, line_lengths};
use subforge_core::time::{fmt_display, parse_clock, parse_offset};

#[derive(Default)]
pub struct EditorState {
    pub last_sel: Option<(usize, usize)>,
}

/// The `{sftw:...}` settings block of a frame-by-frame source line.
fn fbf_meta(e: &subforge_core::Event) -> Option<String> {
    if !e.effect.starts_with(subforge_core::anim::EFFECT_SRC) || !e.text.starts_with("{sftw:") {
        return None;
    }
    e.text.find('}').map(|j| e.text[..=j].to_string())
}

/// Text for the editor: frame-by-frame sources hide their settings block.
fn editable_without_meta(e: &subforge_core::Event) -> String {
    match fbf_meta(e) {
        Some(m) => subforge_core::text::ass_to_editable(&e.text[m.len()..]),
        None => e.editable(),
    }
}

fn char_to_byte(s: &str, c: usize) -> usize {
    s.char_indices().nth(c).map(|(b, _)| b).unwrap_or(s.len())
}

impl App {
    fn sync_buffers(&mut self, ctx: &egui::Context) {
        let Some(c) = self.cur else {
            self.buf_for = None;
            return;
        };
        let Some(e) = self.doc.sub.events.get(c) else { return };
        let gen = self.doc.gen;
        let reload_all = self.buf_for.map(|(i, _)| i != c).unwrap_or(true);
        let reload_some = self.buf_for.map(|(_, g)| g != gen).unwrap_or(true);
        let focused = |id: &str| ctx.memory(|m| m.has_focus(egui::Id::new(id)));
        if reload_all || reload_some {
            if reload_all || !focused("ed_start") {
                self.start_buf = fmt_display(e.start);
            }
            if reload_all || !focused("ed_end") {
                self.end_buf = fmt_display(e.end);
            }
            if reload_all || !focused("ed_dur") {
                self.dur_buf = format!("{:.3}", e.duration() as f64 / 1000.0);
            }
            if reload_all || !focused("ed_text") {
                self.text_buf = editable_without_meta(e);
            }
            self.buf_for = Some((c, gen));
        }
    }

    pub fn editor_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.sync_buffers(&ctx);
        let Some(c) = self.cur.filter(|&c| c < self.doc.sub.events.len()) else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label("No subtitle lines yet.");
                if ui.add(btn(icons::ICON_ADD, "Add first line")).clicked() {
                    self.insert(true);
                }
            });
            return;
        };

        // ---- row 1: index, comment, style, actor
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("#{}", c + 1)).strong().size(16.0));
            let mut comment = self.doc.sub.events[c].comment;
            if ui.checkbox(&mut comment, "Comment").changed() {
                self.modify(|s| s.events[c].comment = comment);
            }
            ui.label("Style");
            let cur_style = self.doc.sub.events[c].style.clone();
            let names: Vec<String> = self.doc.sub.styles.iter().map(|s| s.name.clone()).collect();
            let mut picked = None;
            egui::ComboBox::from_id_salt("ed_style").selected_text(&cur_style).width(110.0).show_ui(ui, |ui| {
                for n in &names {
                    if ui.selectable_label(*n == cur_style, n).clicked() {
                        picked = Some(n.clone());
                    }
                }
            });
            if let Some(n) = picked {
                let v = self.selected();
                self.modify(|s| {
                    for i in v {
                        s.events[i].style = n.clone();
                    }
                });
            }
            if ui.add(ibtn(icons::ICON_STYLE)).on_hover_text("Styles manager").clicked() {
                self.dialogs.styles.open = true;
            }
            ui.label("Actor");
            let mut actor = self.doc.sub.events[c].actor.clone();
            let r = ui.add(egui::TextEdit::singleline(&mut actor).desired_width(100.0).id(egui::Id::new("ed_actor")));
            if r.changed() {
                if !self.text_session {
                    self.checkpoint();
                    self.text_session = true;
                }
                self.doc.sub.events[c].actor = actor;
                self.touch();
            }
        });

        // ---- row 2: times
        ui.horizontal(|ui| {
            let vt = self.video_time();
            let mut commit: Option<(&str, String)> = None;
            for (label, id, which) in [("Start", "ed_start", 0), ("End", "ed_end", 1), ("Dur", "ed_dur", 2)] {
                ui.label(label);
                let buf = match which {
                    0 => &mut self.start_buf,
                    1 => &mut self.end_buf,
                    _ => &mut self.dur_buf,
                };
                let r = ui.add(egui::TextEdit::singleline(buf).id(egui::Id::new(id)).desired_width(if which == 2 { 56.0 } else { 92.0 }).font(egui::TextStyle::Monospace));
                if r.lost_focus() {
                    commit = Some((id, buf.clone()));
                }
                if which < 2 && ui.add_enabled(vt.is_some(), ibtn(icons::ICON_TIMER)).on_hover_text(if which == 0 { "Set start to video time (F7)" } else { "Set end to video time (F8)" }).clicked() {
                    self.set_cur_time(which == 0);
                }
            }
            if let Some((id, val)) = commit {
                let e = self.doc.sub.events[c].clone();
                let new = match id {
                    "ed_start" => parse_clock(&val).map(|t| (t, e.end.max(t))),
                    "ed_end" => parse_clock(&val).map(|t| (e.start, t.max(e.start))),
                    _ => parse_offset(&format!("{}s", val.trim().trim_end_matches('s'))).or_else(|| parse_offset(&val)).map(|d| (e.start, e.start + d.max(0))),
                };
                match new {
                    Some((s0, e0)) if (s0, e0) != (e.start, e.end) => {
                        self.modify(|s| {
                            s.events[c].start = s0;
                            s.events[c].end = e0;
                        });
                        self.buf_for = None;
                    }
                    Some(_) => self.buf_for = None,
                    None => {
                        self.flash_err(format!("Invalid time: {val}"));
                        self.buf_for = None;
                    }
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if ui.add(btn(icons::ICON_PLAY_ARROW, "Play line")).on_hover_text("F5 / Ctrl+P").clicked() {
                self.play_current();
            }
            if ui.add(ibtn(icons::ICON_SKIP_PREVIOUS)).on_hover_text("Previous line (Ctrl+Up)").clicked() {
                self.go_prev();
            }
            if ui.add(ibtn(icons::ICON_SKIP_NEXT)).on_hover_text("Next line (Ctrl+Down; Ctrl+Enter also creates one at the end)").clicked() {
                self.go_next(false);
            }
            if ui.add(ibtn(icons::ICON_MY_LOCATION)).on_hover_text("Seek video to line start").clicked() {
                let t = self.doc.sub.events[c].start;
                self.seek_line(t);
            }
            ui.separator();
            // quick tags
            for (lbl, open, close, tip) in [
                ("I", "{\\i1}", "{\\i0}", "Italic"),
                ("B", "{\\b1}", "{\\b0}", "Bold"),
                ("U", "{\\u1}", "{\\u0}", "Underline"),
                ("an8", "{\\an8}", "", "Toggle top position (\\an8)"),
            ] {
                let rt = match lbl {
                    "I" => RichText::new(lbl).italics(),
                    "B" => RichText::new(lbl).strong(),
                    "U" => RichText::new(lbl).underline(),
                    _ => RichText::new(lbl),
                };
                if ui.small_button(rt).on_hover_text(tip).clicked() {
                    let buf = self.text_buf.clone();
                    let new = if close.is_empty() {
                        if buf.starts_with(open) { buf[open.len()..].to_string() } else { format!("{open}{buf}") }
                    } else if let Some((a, b)) = self.dialogs.editor.last_sel.filter(|(a, b)| a != b) {
                        let (a, b) = (a.min(b), a.max(b));
                        let (ba, bb) = (char_to_byte(&buf, a), char_to_byte(&buf, b));
                        format!("{}{}{}{}{}", &buf[..ba], open, &buf[ba..bb], close, &buf[bb..])
                    } else {
                        format!("{open}{buf}{close}")
                    };
                    self.modify(|s| s.events[c].set_editable(&new));
                    self.buf_for = None;
                }
            }
        });

        // ---- original (translation mode)
        if let Some(orig) = self.original_text(c) {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Original").small().weak());
                    if ui.add(btn(icons::ICON_CONTENT_COPY, "copy")).on_hover_text("Copy original into the editor").clicked() {
                        let o = orig.clone();
                        self.modify(|s| s.events[c].text = o);
                        self.buf_for = None;
                    }
                });
                ui.add(egui::Label::new(RichText::new(orig.replace("\\N", "\n")).size(15.0)).selectable(true));
            });
        }

        // ---- text
        let text_id = egui::Id::new("ed_text");
        let rows = if self.reference.is_some() { 3 } else { 4 };
        // spell-check underline (dictionaries load in the background on first use)
        let spell_on = self.settings.spell_underline && (self.settings.spell_en || self.settings.spell_th);
        if spell_on {
            self.speller(ui.ctx());
        }
        let font = egui::FontId::proportional(17.0);
        let color = ui.visuals().text_color();
        let checker = if spell_on { self.speller.checker.take() } else { None };
        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            let job = crate::spell_dlg::spell_layout_job(checker.as_ref(), buf.as_str(), font.clone(), color, wrap);
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let mut te = egui::TextEdit::multiline(&mut self.text_buf);
        let has_checker = checker.is_some();
        if has_checker {
            te = te.layouter(&mut layouter);
        }
        let out = te
            .id(text_id)
            .desired_rows(rows)
            .desired_width(f32::INFINITY)
            .font(egui::FontId::proportional(17.0))
            .hint_text("Type subtitle text… (Enter = new line, Ctrl+Enter = next line)")
            .show(ui);
        if has_checker {
            self.speller.checker = checker;
        }
        if self.dialogs.focus_text {
            out.response.request_focus();
            self.dialogs.focus_text = false;
        }
        if let Some(r) = out.cursor_range {
            let (a, b) = (r.primary.index, r.secondary.index);
            self.dialogs.editor.last_sel = Some((a.into(), b.into()));
        }
        if out.response.changed() {
            if !self.text_session {
                self.checkpoint();
                self.text_session = true;
            }
            let buf = self.text_buf.clone();
            let e = &mut self.doc.sub.events[c];
            match fbf_meta(e) {
                // frame-by-frame source: keep the hidden settings block, regenerate on commit
                Some(meta) => {
                    e.set_editable(&buf);
                    e.text = format!("{meta}{}", e.text);
                    self.regen_pending = Some(c);
                }
                None => e.set_editable(&buf),
            }
            self.touch();
            self.buf_for = Some((c, self.doc.gen));
        }
        if out.response.lost_focus() {
            if let Some(i) = self.regen_pending.take() {
                self.regenerate_animation(i);
            }
            self.end_text_session();
        }

        // ---- live stats
        let e = &self.doc.sub.events[c];
        let cfg = &self.settings.qc;
        let plain = e.plain();
        let chars = count_chars(&plain, cfg.count);
        let cps = if e.duration() > 0 { chars as f64 * 1000.0 / e.duration() as f64 } else { f64::INFINITY };
        let lens = line_lengths(&plain);
        ui.horizontal_wrapped(|ui| {
            let ok = egui::Color32::from_rgb(90, 190, 110);
            let cps_col = if cfg.max_cps > 0.0 && cps > cfg.max_cps * 1.25 {
                sev_color(Severity::Error)
            } else if cfg.max_cps > 0.0 && cps > cfg.max_cps {
                sev_color(Severity::Warning)
            } else {
                ok
            };
            ui.label(RichText::new(format!("CPS {:.1} / {:.0}", cps, cfg.max_cps)).color(cps_col).strong());
            ui.separator();
            ui.label(format!("{} chars", chars));
            ui.separator();
            for (k, l) in lens.iter().enumerate() {
                let col = if cfg.max_line_len > 0 && *l > cfg.max_line_len { sev_color(Severity::Warning) } else { ok };
                ui.label(RichText::new(format!("L{}: {}", k + 1, l)).color(col));
            }
            if cfg.max_lines > 0 && lens.len() > cfg.max_lines {
                ui.label(RichText::new(format!("> {} lines", cfg.max_lines)).color(sev_color(Severity::Warning)));
            }
            ui.separator();
            let d = e.duration();
            let dcol = if d < cfg.min_duration_ms || (cfg.max_duration_ms > 0 && d > cfg.max_duration_ms) { sev_color(Severity::Warning) } else { ok };
            ui.label(RichText::new(format!("{:.2}s", d as f64 / 1000.0)).color(dcol));
            if cfg.max_cps > 0.0 && cps > cfg.max_cps {
                let need = (chars as f64 / cfg.max_cps * 1000.0).ceil() as i64;
                ui.label(RichText::new(format!("(needs at least {:.2}s)", need as f64 / 1000.0)).weak());
            }
        });
    }
}
