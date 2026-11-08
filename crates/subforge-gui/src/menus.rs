//! Main menu bar and status bar.

use crate::app::{apply_theme, App, Pending, Reference};
use crate::icons::{self, ic, ic_s, menu_item};
use crate::layout::{self, Pane};
use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;
use std::time::Duration;
use subforge_core::qc::Severity;
use subforge_core::{edit, Format};

impl App {
    pub fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(menu_item(icons::ICON_NOTE_ADD, "New", "Ctrl+N")).clicked() {
                    self.guard(Pending::New, &ctx);
                }
                if ui.add(menu_item(icons::ICON_FOLDER_OPEN, "Open…", "Ctrl+O")).on_hover_text("Subtitle files, or a video to import one of its subtitle tracks").clicked() {
                    self.guard(Pending::Open(None), &ctx);
                }
                ui.menu_button((ic_s(icons::ICON_SCHEDULE), "Open recent"), |ui| {
                    if self.settings.recent.is_empty() {
                        ui.label(RichText::new("(none)").weak());
                    }
                    for r in self.settings.recent.clone() {
                        if ui.button(&r).clicked() {
                            self.guard(Pending::Open(Some(PathBuf::from(&r))), &ctx);
                        }
                    }
                    ui.separator();
                    if ui.button("Clear list").clicked() {
                        self.settings.recent.clear();
                    }
                });
                if ui.add(menu_item(icons::ICON_SUBTITLES, "Import subtitles from video…", "")).on_hover_text("Extract a subtitle track from MKV / MP4 / TS (like Subtitle Edit)").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Video", crate::media::VIDEO_EXTS).add_filter("All files", &["*"]).set_directory(self.last_dir()).pick_file() {
                        self.guard(Pending::ImportVideo(p), &ctx);
                    }
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_SAVE, "Save", "Ctrl+S")).clicked() {
                    self.save();
                }
                if ui.add(menu_item(icons::ICON_SAVE_AS, "Save as…", "Ctrl+Shift+S")).clicked() {
                    self.save_as(None);
                }
                ui.menu_button((ic_s(icons::ICON_DOWNLOAD), "Export as"), |ui| {
                    for f in Format::ALL {
                        if ui.button(f.name()).clicked() {
                            self.export(f);
                        }
                    }
                });
                ui.separator();
                if ui.add(menu_item(icons::ICON_MOVIE, "Open video / audio…", "")).clicked() {
                    self.open_video_dialog(&ctx);
                }
                if ui.add_enabled(self.player.is_some(), menu_item(icons::ICON_CLOSE, "Close video", "")).clicked() {
                    self.player = None;
                    self.video_tex = None;
                    self.video_tex_ptr = 0;
                }
                if ui.add(menu_item(icons::ICON_MOVIE, "Mux into MKV…", "")).on_hover_text("Add this subtitle and its fonts to a video (no re-encoding)").clicked() {
                    self.open_mux_dialog();
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_SWAP_HORIZ, "Batch convert…", "")).clicked() {
                    self.dialogs.batch.open = true;
                }
                if ui.add(menu_item(icons::ICON_DESCRIPTION, "Script properties…", "")).clicked() {
                    self.dialogs.props.open = true;
                }
                if ui.add(menu_item(icons::ICON_SETTINGS, "Settings…", "")).clicked() {
                    self.dialogs.settings_open = true;
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_LOGOUT, "Quit", "")).clicked() {
                    self.guard(Pending::Quit, &ctx);
                }
            });

            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(self.can_undo(), menu_item(icons::ICON_UNDO, "Undo", "Ctrl+Z")).clicked() {
                    self.undo();
                }
                if ui.add_enabled(self.can_redo(), menu_item(icons::ICON_REDO, "Redo", "Ctrl+Y")).clicked() {
                    self.redo();
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_ADD, "Insert after", "Ctrl+I")).clicked() {
                    self.insert(true);
                }
                if ui.add(menu_item(icons::ICON_ADD, "Insert before", "")).clicked() {
                    self.insert(false);
                }
                if ui.add(menu_item(icons::ICON_MY_LOCATION, "Insert at video time", "")).clicked() {
                    let t = self.video_time().unwrap_or(0);
                    self.insert_at(t);
                }
                if ui.add(menu_item(icons::ICON_CONTENT_COPY, "Duplicate", "Ctrl+D")).clicked() {
                    self.duplicate_selected();
                }
                if ui.add(menu_item(icons::ICON_DELETE, "Delete", "Del")).clicked() {
                    self.delete_selected();
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_CALL_SPLIT, "Split line", "Ctrl+K")).clicked() {
                    self.split_current();
                }
                if ui.add(menu_item(icons::ICON_MERGE, "Join (new line)", "Ctrl+J")).clicked() {
                    self.merge_selected(true);
                }
                if ui.add(menu_item(icons::ICON_MERGE, "Join (same line)", "")).clicked() {
                    self.merge_selected(false);
                }
                if ui.add(menu_item(icons::ICON_COMMENT, "Toggle comment", "")).clicked() {
                    self.toggle_comment();
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_SEARCH, "Find…", "Ctrl+F")).clicked() {
                    self.action("find", &ctx);
                }
                if ui.add(menu_item(icons::ICON_FIND_REPLACE, "Replace…", "Ctrl+H")).clicked() {
                    self.action("replace", &ctx);
                }
                if ui.add(menu_item(icons::ICON_SELECT_ALL, "Select all", "Ctrl+A")).clicked() {
                    self.sel = (0..self.doc.sub.events.len()).collect();
                }
            });

            ui.menu_button("Timing", |ui| {
                if ui.add(menu_item(icons::ICON_SYNC, "Sync to speech (auto)…", "")).on_hover_text("Fix offset / frame-rate drift by matching lines to the audio").clicked() {
                    self.dialogs.speech.mode = crate::speech_dlg::Mode::Sync;
                    self.dialogs.speech.open = true;
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_SWAP_HORIZ, "Shift times…", "")).clicked() {
                    self.dialogs.shift.open = true;
                }
                if ui.add(menu_item(icons::ICON_SPEED, "Change frame rate…", "")).clicked() {
                    self.dialogs.fps.open = true;
                }
                if ui.add(menu_item(icons::ICON_STRAIGHTEN, "Synchronize (2 points)…", "")).clicked() {
                    self.dialogs.sync.prefill(&self.doc.sub, &self.selected());
                    self.dialogs.sync.open = true;
                }
                if ui.add(menu_item(icons::ICON_TIMER, "Adjust durations…", "")).clicked() {
                    self.dialogs.tools.durations_open = true;
                }
                if ui.add(menu_item(icons::ICON_LINK, "Bridge gaps…", "")).clicked() {
                    self.dialogs.tools.bridge_open = true;
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_FIRST_PAGE, "Set start to video", "F7")).clicked() {
                    self.set_cur_time(true);
                }
                if ui.add(menu_item(icons::ICON_LAST_PAGE, "Set end to video", "F8")).clicked() {
                    self.set_cur_time(false);
                }
                ui.separator();
                let has_kf = self.keyframes().is_some();
                if ui.add_enabled(has_kf, menu_item(icons::ICON_ANCHOR, "Snap selected to keyframes", "F6")).clicked() {
                    self.snap_selected_to_keyframes();
                }
                if ui.add(menu_item(icons::ICON_UPLOAD_FILE, "Load keyframe file…", "")).clicked() {
                    self.load_keyframe_file();
                }
                if ui.add_enabled(has_kf, menu_item(icons::ICON_DOWNLOAD, "Export keyframes…", "")).clicked() {
                    self.export_keyframes();
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_LINK, "Snap to neighbours", "")).clicked() {
                    self.snap_to_neighbors();
                }
                if ui.add(menu_item(icons::ICON_MOVIE, "Snap all to video frames", "")).clicked() {
                    if let Some(fps) = self.player.as_ref().map(|p| p.info.fps) {
                        self.modify(|s| subforge_core::timing::snap_to_frames(s, fps, &subforge_core::timing::Scope::All));
                    } else {
                        self.flash_err("Open a video first (frame rate needed)");
                    }
                }
                if ui.add(menu_item(icons::ICON_SORT, "Sort by start time", "")).clicked() {
                    self.modify(|s| s.sort_by_time());
                }
            });

            ui.menu_button("Typeset", |ui| {
                if ui.add(menu_item(icons::ICON_ANIMATION, "Position & Motion panel", "")).clicked() {
                    self.show_pane(Pane::Motion);
                }
                if ui.add(menu_item(icons::ICON_OPEN_WITH, "Move tool (drag on video)", "M")).clicked() {
                    self.vtool = crate::motion::VideoTool::Move;
                    self.show_pane(Pane::Video);
                }
                ui.separator();
                ui.menu_button((ic_s(icons::ICON_MOVING), "Quick animation"), |ui| {
                    for p in subforge_core::anim::Preset::ALL {
                        if ui.button(p.name()).clicked() {
                            self.apply_preset_to_selection(p);
                            ui.close();
                        }
                    }
                });
                if ui.add(menu_item(icons::ICON_DELETE, "Remove animation", "")).clicked() {
                    let v = self.selected();
                    self.modify(|s| {
                        for &i in v.iter().rev() {
                            subforge_core::anim::remove_tween(s, i);
                        }
                    });
                    self.motion.loaded_for = None;
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_FONT_DOWNLOAD, "Fonts…", "")).on_hover_text("Which fonts the script needs, where they are, collect them").clicked() {
                    self.dialogs.fonts.open = true;
                    self.dialogs.fonts.reports.clear();
                    self.refresh_font_report(true);
                }
                if ui.add(menu_item(icons::ICON_MOVIE, "Mux into MKV…", "")).clicked() {
                    self.open_mux_dialog();
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_STYLE, "Styles manager…", "")).clicked() {
                    self.dialogs.styles.open = true;
                }
                if ui.add(menu_item(icons::ICON_ASPECT_RATIO, "Resample resolution…", "")).on_hover_text("Change PlayRes and rescale positions, sizes, clips and drawings").clicked() {
                    self.dialogs.tools.resample_open = true;
                    self.dialogs.tools.resample_to = None;
                }
                if ui.add(menu_item(icons::ICON_DESCRIPTION, "Script properties…", "")).clicked() {
                    self.dialogs.props.open = true;
                }
            });

            ui.menu_button("Tools", |ui| {
                if ui.add(menu_item(icons::ICON_FACT_CHECK, "Quality check panel", "F9")).clicked() {
                    self.show_pane(Pane::Issues);
                }
                if ui.add(menu_item(icons::ICON_AUTO_FIX_HIGH, "QC settings / Auto-fix…", "")).clicked() {
                    self.dialogs.qc_open = true;
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_SPELLCHECK, "Spell check…", "Ctrl+F7")).clicked() {
                    self.dialogs.spell.open = true;
                }
                if ui.add(menu_item(icons::ICON_AUTO_FIX_NORMAL, "Fix common errors…", "")).clicked() {
                    self.dialogs.fixes.open = true;
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_RECORD_VOICE_OVER, "Speech: transcribe / sync…", "")).on_hover_text("whisper.cpp transcription, re-timing and sync to speech").clicked() {
                    self.dialogs.speech.open = true;
                }
                if ui.add(menu_item(icons::ICON_DOCUMENT_SCANNER, "OCR image subtitles (PGS / VobSub)…", "")).clicked() {
                    self.ocr_from_file();
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_HEARING_DISABLED, "Remove text for hearing impaired…", "")).clicked() {
                    self.dialogs.tools.hi_open = true;
                }
                if ui.add(menu_item(icons::ICON_TEXT_FORMAT, "Change casing…", "")).clicked() {
                    self.dialogs.tools.case_open = true;
                }
                if ui.add(menu_item(icons::ICON_MERGE, "Merge lines with same text", "")).clicked() {
                    let n = self.modify(|s| subforge_core::tools::merge_same_text(s, 120));
                    self.flash(format!("Merged {n} duplicate line(s)"));
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_FORMAT_ALIGN_CENTER, "Balance line breaks (selected)", "")).clicked() {
                    let v = self.selected();
                    self.modify(|s| {
                        for i in v {
                            let e = &mut s.events[i];
                            if !e.text.contains("\\N") {
                                e.text = subforge_core::text::balance_two_lines(&e.text);
                            }
                        }
                    });
                }
                if ui.add(menu_item(icons::ICON_SHORT_TEXT, "Unbreak lines (selected)", "")).clicked() {
                    let v = self.selected();
                    self.modify(|s| {
                        for i in v {
                            s.events[i].text = s.events[i].text.replace("\\N", " ");
                        }
                    });
                }
                if ui.add(menu_item(icons::ICON_FORMAT_CLEAR, "Strip all tags", "")).clicked() {
                    let n = self.modify(edit::strip_all_tags);
                    self.flash(format!("Stripped tags from {n} lines"));
                }
                if ui.add(menu_item(icons::ICON_DELETE_SWEEP, "Remove empty lines", "")).clicked() {
                    let n = self.modify(|s| {
                        let b = s.events.len();
                        s.events.retain(|e| e.comment || !e.plain().trim().is_empty());
                        b - s.events.len()
                    });
                    self.flash(format!("Removed {n} empty lines"));
                }
                ui.separator();
                if ui.add(menu_item(icons::ICON_SWAP_HORIZ, "Batch convert…", "")).clicked() {
                    self.dialogs.batch.open = true;
                }
            });

            ui.menu_button("Translate", |ui| {
                if ui.add(menu_item(icons::ICON_UPLOAD_FILE, "Load reference (original) subtitle…", "")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Subtitles", &["srt", "ass", "ssa", "vtt"]).set_directory(self.last_dir()).pick_file() {
                        match subforge_core::load(&p, None) {
                            Ok(l) => {
                                self.reference = Some(Reference { path: p.clone(), sub: l.sub, by_index: false });
                                self.ref_cache = None;
                                self.settings.show_original_column = true;
                                self.flash(format!("Reference loaded: {}", p.display()));
                            }
                            Err(e) => self.flash_err(format!("Cannot load reference: {e}")),
                        }
                    }
                }
                if ui.add(menu_item(icons::ICON_TRANSLATE, "Use current file as reference (start translating)", "")).clicked() {
                    self.reference = Some(Reference { path: self.doc.path.clone().unwrap_or_default(), sub: self.doc.sub.clone(), by_index: false });
                    self.ref_cache = None;
                    self.settings.show_original_column = true;
                }
                if let Some(r) = &self.reference {
                    ui.label(RichText::new(format!("Reference: {}", r.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "(current file)".into()))).weak());
                }
                ui.add_enabled_ui(self.reference.is_some(), |ui| {
                    if let Some(r) = &mut self.reference {
                        if ui.checkbox(&mut r.by_index, "Match lines by number (instead of time)").changed() {
                            self.ref_cache = None;
                        }
                    }
                    ui.checkbox(&mut self.settings.show_original_column, "Show original column");
                    if ui.add(menu_item(icons::ICON_CONTENT_COPY, "Copy original to selected lines", "")).clicked() {
                        let v = self.selected();
                        let originals: Vec<(usize, Option<String>)> = v.iter().map(|&i| (i, self.original_text(i))).collect();
                        self.modify(|s| {
                            for (i, o) in originals {
                                if let Some(o) = o {
                                    s.events[i].text = o;
                                }
                            }
                        });
                        self.buf_for = None;
                    }
                    if ui.add(menu_item(icons::ICON_CLOSE, "Close reference", "")).clicked() {
                        self.reference = None;
                        self.ref_cache = None;
                    }
                });
            });

            ui.menu_button("View", |ui| {
                ui.label(RichText::new("Panels").small().weak());
                if let Some(tree) = &mut self.tree {
                    for p in Pane::ALL {
                        let mut on = layout::is_visible(tree, p);
                        if ui.checkbox(&mut on, (ic_s(p.icon()), p.title())).changed() {
                            layout::set_visible(tree, p, on);
                        }
                    }
                }
                if ui.add(menu_item(icons::ICON_DASHBOARD, "Reset layout", "")).on_hover_text("Restore the default arrangement of panels").clicked() {
                    self.tree = Some(layout::default_tree());
                }
                ui.label(RichText::new("Tip: drag a panel's tab to move it; drag the gaps between panels to resize").small().weak());
                ui.separator();
                if ui.checkbox(&mut self.settings.dark, "Dark theme").changed() {
                    apply_theme(ui.ctx(), &self.settings);
                }
                ui.horizontal(|ui| {
                    ui.label("UI scale");
                    if ui.add(egui::Slider::new(&mut self.settings.ui_scale, 0.75..=2.0).step_by(0.05)).changed() {
                        apply_theme(ui.ctx(), &self.settings);
                    }
                });
            });

            ui.menu_button("Help", |ui| {
                if ui.add(menu_item(icons::ICON_KEYBOARD, "Keyboard shortcuts", "")).clicked() {
                    self.dialogs.help_open = true;
                }
                if ui.add(menu_item(icons::ICON_INFO, "About", "")).clicked() {
                    self.dialogs.about_open = true;
                }
            });
        });
    }

    pub fn show_pane(&mut self, p: Pane) {
        if let Some(t) = &mut self.tree {
            layout::set_visible(t, p, true);
        }
    }

    pub fn apply_preset_to_selection(&mut self, p: subforge_core::anim::Preset) {
        let mut v = self.selected();
        v.sort_unstable();
        let fps = self.fps_or_default();
        let res = self.doc.sub.play_res();
        let mut errors = Vec::new();
        self.modify(|s| {
            for &i in v.iter().rev() {
                let i = subforge_core::anim::find_source(s, i).unwrap_or(i);
                let here = subforge_core::anim::State::of_event(s, &s.events[i]);
                let tw = p.build(here, res, s.events[i].duration());
                if let Err(e) = subforge_core::anim::apply_tween(s, i, &tw, fps) {
                    errors.push(format!("line {}: {e}", i + 1));
                }
            }
        });
        self.motion.loaded_for = None;
        if errors.is_empty() {
            self.flash(format!("{} applied to {} line(s)", p.name(), v.len()));
        } else {
            self.flash_err(errors.join("; "));
        }
    }

    pub fn status_bar(&mut self, ui: &mut egui::Ui) {
        let (n_err, n_warn) = {
            let c = self.issues();
            (c.issues.iter().filter(|i| i.severity == Severity::Error).count(), c.issues.iter().filter(|i| i.severity == Severity::Warning).count())
        };
        ui.horizontal(|ui| {
            ui.label(ic_s(icons::ICON_DESCRIPTION));
            let mut name = RichText::new(self.doc.name());
            if self.doc.dirty {
                name = name.italics();
            }
            ui.label(name);
            if self.doc.dirty {
                ui.label(RichText::new("modified").small().color(Color32::from_rgb(225, 160, 50)));
            }
            ui.separator();
            ui.label(format!("{} · {} · {} lines", self.doc.format.ext().to_uppercase(), self.doc.encoding, self.doc.sub.events.len()))
                .on_hover_text(format!("Undo history: {} steps, ~{:.1} KB", self.history.len(), self.history.approx_bytes() as f64 / 1024.0));
            if self.sel.len() > 1 {
                ui.label(format!("({} selected)", self.sel.len()));
            }
            ui.separator();
            let err_col = if n_err > 0 { Color32::from_rgb(230, 80, 80) } else { ui.visuals().weak_text_color() };
            let warn_col = if n_warn > 0 { Color32::from_rgb(225, 160, 50) } else { ui.visuals().weak_text_color() };
            let r = ui
                .add(egui::Button::new((ic_s(icons::ICON_ERROR).color(err_col), RichText::new(n_err.to_string()).color(err_col), ic_s(icons::ICON_WARNING).color(warn_col), RichText::new(n_warn.to_string()).color(warn_col))).frame(false))
                .on_hover_text("Open the quality check panel");
            if r.clicked() {
                self.show_pane(Pane::Issues);
            }
            if let Some(fams) = self.missing_fonts().filter(|f| !f.is_empty()) {
                let label = if fams.len() == 1 { format!("font missing: {}", fams[0]) } else { format!("{} font families missing", fams.len()) };
                ui.separator();
                let c = Color32::from_rgb(230, 110, 90);
                if ui
                    .add(egui::Button::new((ic_s(icons::ICON_FONT_DOWNLOAD).color(c), RichText::new(label).color(c))).frame(false))
                    .on_hover_text("Open the Fonts window")
                    .clicked()
                {
                    self.dialogs.fonts.open = true;
                }
            }
            if let Some((m, t, err)) = &self.status {
                if t.elapsed() < Duration::from_secs(8) {
                    ui.separator();
                    let (icon, c) = if *err { (icons::ICON_ERROR, Color32::from_rgb(230, 110, 90)) } else { (icons::ICON_INFO, ui.visuals().text_color()) };
                    ui.label(ic_s(icon).color(c));
                    ui.label(RichText::new(m).color(c));
                }
            }
            if let Some(p) = &self.player {
                let s = p.status.lock().unwrap().clone();
                if !s.is_empty() {
                    ui.separator();
                    ui.label(ic(icons::ICON_WARNING).color(Color32::from_rgb(230, 160, 60)));
                    ui.label(RichText::new(s).color(Color32::from_rgb(230, 160, 60)));
                }
            }
        });
    }
}
