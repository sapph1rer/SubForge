//! Subtitle grid (virtualised table) and the QC issues panel.

use crate::app::App;
use crate::icons::{self, btn, ibtn, ic_s};
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use subforge_core::qc::{self, Severity};
use subforge_core::time::fmt_display;

pub fn sev_color(s: Severity) -> Color32 {
    match s {
        Severity::Error => Color32::from_rgb(220, 70, 70),
        Severity::Warning => Color32::from_rgb(225, 160, 50),
        Severity::Info => Color32::from_rgb(110, 150, 220),
    }
}

fn one_line(text: &str) -> String {
    let t = text.replace("\\N", " / ").replace("\\n", " / ");
    if t.chars().count() > 300 {
        t.chars().take(300).collect::<String>() + "…"
    } else {
        t
    }
}

impl App {
    pub fn grid_ui(&mut self, ui: &mut egui::Ui) {
        // toolbar
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(ic_s(icons::ICON_SEARCH));
            ui.add(egui::TextEdit::singleline(&mut self.grid_filter).hint_text("filter lines…").desired_width(180.0));
            if !self.grid_filter.is_empty() && ui.add(ibtn(icons::ICON_CLOSE)).on_hover_text("Clear filter").clicked() {
                self.grid_filter.clear();
            }
            ui.separator();
            if ui.add(btn(icons::ICON_ADD, "Insert")).on_hover_text("Insert after current (Ctrl+I)").clicked() {
                self.insert(true);
            }
            if ui.add(btn(icons::ICON_DELETE, "Delete")).on_hover_text("Delete selected (Del)").clicked() {
                self.delete_selected();
            }
            if ui.add(btn(icons::ICON_CONTENT_COPY, "Duplicate")).on_hover_text("Ctrl+D").clicked() {
                self.duplicate_selected();
            }
            if ui.add(btn(icons::ICON_CALL_SPLIT, "Split")).on_hover_text("Ctrl+K — at the playhead if it is inside the line").clicked() {
                self.split_current();
            }
            if ui.add(btn(icons::ICON_MERGE, "Join")).on_hover_text("Ctrl+J").clicked() {
                self.merge_selected(true);
            }
            if ui.add(btn(icons::ICON_COMMENT, "Comment")).on_hover_text("Toggle comment").clicked() {
                self.toggle_comment();
            }
            if ui.add(crate::icons::itoggle(self.settings.grid_hide_frames, icons::ICON_ANIMATION)).on_hover_text("Hide frame-by-frame animation frames (show only their source line)").clicked() {
                self.settings.grid_hide_frames = !self.settings.grid_hide_frames;
            }
            ui.separator();
            if ui.add_enabled(self.can_undo(), ibtn(icons::ICON_UNDO)).on_hover_text("Undo (Ctrl+Z)").clicked() {
                self.undo();
            }
            if ui.add_enabled(self.can_redo(), ibtn(icons::ICON_REDO)).on_hover_text("Redo (Ctrl+Y)").clicked() {
                self.redo();
            }
        });
        ui.add_space(2.0);

        let filter = self.grid_filter.to_lowercase();
        let hide_frames = self.settings.grid_hide_frames;
        let is_frame = |e: &subforge_core::Event| hide_frames && e.effect.starts_with(subforge_core::anim::EFFECT_GEN);
        let rows: Vec<usize> = if filter.is_empty() {
            (0..self.doc.sub.events.len()).filter(|&i| !is_frame(&self.doc.sub.events[i])).collect()
        } else {
            self.doc
                .sub
                .events
                .iter()
                .enumerate()
                .filter(|(_, e)| !is_frame(e))
                .filter(|(_, e)| e.plain().to_lowercase().contains(&filter) || e.actor.to_lowercase().contains(&filter) || e.style.to_lowercase().contains(&filter))
                .map(|(i, _)| i)
                .collect()
        };
        let per_event = self.issues().per_event.clone();
        let show_orig = self.reference.is_some() && self.settings.show_original_column;
        let originals: Vec<Option<String>> = if show_orig {
            let map = self.ref_map().cloned().unwrap_or_default();
            let r = self.reference.as_ref().unwrap();
            rows.iter().map(|&i| map.get(i).copied().flatten().and_then(|j| r.sub.events.get(j)).map(|e| one_line(&e.text))).collect()
        } else {
            Vec::new()
        };
        let cfg = self.settings.qc.clone();
        let has_actor = self.doc.sub.events.iter().any(|e| !e.actor.is_empty());
        let playhead = self.video_time();

        let scroll_row = self.scroll_to.take().and_then(|i| rows.iter().position(|&r| r == i));
        let mut clicked: Option<(usize, egui::Modifiers)> = None;
        let mut dbl: Option<usize> = None;
        let text_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;

        let mut tb = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(44.0))
            .column(Column::initial(92.0).at_least(70.0))
            .column(Column::initial(92.0).at_least(70.0))
            .column(Column::initial(52.0).at_least(40.0))
            .column(Column::initial(46.0).at_least(36.0))
            .column(Column::initial(80.0).at_least(40.0).clip(true));
        if has_actor {
            tb = tb.column(Column::initial(80.0).at_least(40.0).clip(true));
        }
        if show_orig {
            tb = tb.column(Column::initial(300.0).at_least(80.0).clip(true));
        }
        tb = tb.column(Column::remainder().at_least(120.0).clip(true));
        if let Some(r) = scroll_row {
            tb = tb.scroll_to_row(r, Some(egui::Align::Center));
        }
        tb.header(22.0, |mut h| {
            h.col(|ui| {
                ui.strong("#");
            });
            h.col(|ui| {
                ui.strong("Start");
            });
            h.col(|ui| {
                ui.strong("End");
            });
            h.col(|ui| {
                ui.strong("Dur");
            });
            h.col(|ui| {
                ui.strong("CPS");
            });
            h.col(|ui| {
                ui.strong("Style");
            });
            if has_actor {
                h.col(|ui| {
                    ui.strong("Actor");
                });
            }
            if show_orig {
                h.col(|ui| {
                    ui.strong("Original");
                });
            }
            h.col(|ui| {
                ui.strong("Text");
            });
        })
        .body(|body| {
            body.rows(text_h, rows.len(), |mut row| {
                let ri = row.index();
                let i = rows[ri];
                let e = &self.doc.sub.events[i];
                let st = qc::stats(e, cfg.count);
                row.set_selected(self.sel.contains(&i));
                let sev = per_event.get(i).copied().flatten();
                let active = playhead.is_some_and(|t| e.start <= t && t < e.end && !e.comment);
                let weak = e.comment;
                row.col(|ui| {
                    let mut t = RichText::new(format!("{}", i + 1)).monospace();
                    if let Some(s) = sev {
                        t = t.color(sev_color(s)).strong();
                    }
                    if active {
                        t = t.underline();
                    }
                    ui.label(t);
                });
                row.col(|ui| {
                    ui.label(RichText::new(fmt_display(e.start)).monospace());
                });
                row.col(|ui| {
                    ui.label(RichText::new(fmt_display(e.end)).monospace());
                });
                row.col(|ui| {
                    let mut t = RichText::new(format!("{:.2}", st.duration_ms as f64 / 1000.0)).monospace();
                    if st.duration_ms <= 0 || (st.duration_ms < cfg.min_duration_ms && st.chars > 0) {
                        t = t.color(sev_color(Severity::Warning));
                    }
                    ui.label(t);
                });
                row.col(|ui| {
                    let mut t = RichText::new(if st.cps.is_finite() { format!("{:.0}", st.cps) } else { "∞".into() }).monospace();
                    if cfg.max_cps > 0.0 && st.cps > cfg.max_cps {
                        t = t.color(sev_color(if st.cps > cfg.max_cps * 1.25 { Severity::Error } else { Severity::Warning })).strong();
                    }
                    ui.label(t);
                });
                row.col(|ui| {
                    ui.label(&e.style);
                });
                if has_actor {
                    row.col(|ui| {
                        ui.label(&e.actor);
                    });
                }
                if show_orig {
                    row.col(|ui| {
                        ui.label(RichText::new(originals[ri].clone().unwrap_or_default()).weak());
                    });
                }
                row.col(|ui| {
                    let anim = e.text.starts_with("{sftw:") || e.effect.starts_with(subforge_core::anim::EFFECT_SRC);
                    let frame = e.effect.starts_with(subforge_core::anim::EFFECT_GEN);
                    if anim || frame {
                        ui.label(ic_s(icons::ICON_ANIMATION).color(Color32::from_rgb(120, 140, 255)));
                    }
                    let shown = if anim { subforge_core::anim::strip_generated(&e.text) } else { e.text.clone() };
                    let mut t = RichText::new(one_line(&shown));
                    if weak && !anim {
                        t = t.weak().italics();
                    }
                    if frame {
                        t = t.weak();
                    }
                    ui.add(egui::Label::new(t).truncate().selectable(false));
                });
                let resp = row.response();
                if resp.clicked() {
                    clicked = Some((i, ui_mods(&resp)));
                }
                if resp.double_clicked() {
                    dbl = Some(i);
                }
            });
        });
        if let Some((i, m)) = clicked {
            self.click_row(i, m);
        }
        if let Some(i) = dbl {
            let t = self.doc.sub.events[i].start;
            self.seek_line(t);
        }
    }

    pub fn issues_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if ui.add(ibtn(icons::ICON_TUNE)).on_hover_text("QC settings").clicked() {
                self.dialogs.qc_open = true;
            }
            if ui.add(ibtn(icons::ICON_AUTO_FIX_HIGH)).on_hover_text("Auto-fix…").clicked() {
                self.dialogs.qc_open = true;
            }
            ui.separator();
            ui.selectable_value(&mut self.issue_filter_min, Severity::Info, "All");
            ui.selectable_value(&mut self.issue_filter_min, Severity::Warning, "Warnings");
            ui.selectable_value(&mut self.issue_filter_min, Severity::Error, "Errors");
        });
        let q = &self.settings.qc;
        ui.label(RichText::new(format!("max CPS {:.0} · line {} · lines {} · dur {}–{} ms", q.max_cps, q.max_line_len, q.max_lines, q.min_duration_ms, q.max_duration_ms)).small().weak());
        ui.separator();
        let min = self.issue_filter_min;
        let issues: Vec<_> = self.issues().issues.iter().filter(|i| i.severity >= min).cloned().collect();
        if issues.is_empty() {
            ui.horizontal(|ui| {
                ui.label(ic_s(icons::ICON_CHECK_CIRCLE).color(Color32::from_rgb(90, 190, 110)));
                ui.label(RichText::new("No issues").color(Color32::from_rgb(90, 190, 110)));
            });
            return;
        }
        let mut go = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 20.0, issues.len(), |ui, range| {
            for k in range {
                let is = &issues[k];
                let sel = self.cur == Some(is.index);
                let txt = RichText::new(format!("#{:<4} {}: {}", is.index + 1, is.kind.label(), is.message)).color(sev_color(is.severity));
                if ui.add(egui::Button::selectable(sel, txt).truncate()).clicked() {
                    go = Some(is.index);
                }
            }
        });
        if let Some(i) = go {
            self.select_only(Some(i));
            let t = self.doc.sub.events[i].start;
            self.seek_line(t);
        }
    }
}

fn ui_mods(r: &egui::Response) -> egui::Modifiers {
    r.ctx.input(|i| i.modifiers)
}
