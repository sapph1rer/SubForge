//! Tool windows: shift, frame rate, sync, find/replace, styles, properties, batch, QC, settings.

use crate::app::{apply_theme, App};
use crate::editor::EditorState;
use crate::icons::{self, btn, ibtn};
use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;
use subforge_core::edit::{self, FindField, FindOpts};
use subforge_core::qc::{self, FixOpts, QcConfig};
use subforge_core::time::{fmt_display, parse_clock, parse_fps, parse_offset};
use subforge_core::timing::{self, Field, Scope};
use subforge_core::{Color, Format, Style, Subtitle, WriteOpts};

pub struct ShiftDlg {
    pub open: bool,
    offset: String,
    forward: bool,
    scope: u8,
    field: Field,
}
impl Default for ShiftDlg {
    fn default() -> Self {
        ShiftDlg { open: false, offset: "1.000s".into(), forward: true, scope: 0, field: Field::Both }
    }
}

pub struct FpsDlg {
    pub open: bool,
    from: String,
    to: String,
}
impl Default for FpsDlg {
    fn default() -> Self {
        FpsDlg { open: false, from: "23.976".into(), to: "25".into() }
    }
}

#[derive(Default)]
pub struct SyncDlg {
    pub open: bool,
    a_old: String,
    a_new: String,
    b_old: String,
    b_new: String,
}
impl SyncDlg {
    pub fn prefill(&mut self, sub: &Subtitle, sel: &[usize]) {
        let first = sel.first().copied().unwrap_or(0);
        let last = sel.last().copied().filter(|&l| l != first).unwrap_or(sub.events.len().saturating_sub(1));
        if let Some(e) = sub.events.get(first) {
            self.a_old = fmt_display(e.start);
            self.a_new = fmt_display(e.start);
        }
        if let Some(e) = sub.events.get(last) {
            self.b_old = fmt_display(e.start);
            self.b_new = fmt_display(e.start);
        }
    }
}

#[derive(Default)]
pub struct FindDlg {
    pub open: bool,
    pub replace_mode: bool,
    pub focus: bool,
    pattern: String,
    replacement: String,
    regex: bool,
    case: bool,
    whole: bool,
    field: FindField,
    msg: String,
}

#[derive(Default)]
pub struct StylesDlg {
    pub open: bool,
    sel: usize,
}

#[derive(Default)]
pub struct PropsDlg {
    pub open: bool,
}

pub struct BatchDlg {
    pub open: bool,
    pub files: Vec<PathBuf>,
    out_dir: Option<PathBuf>,
    target: Format,
    suffix: String,
    encoding: String,
    shift: String,
    fps_from: String,
    fps_to: String,
    fix: bool,
    strip: bool,
    overwrite: bool,
    log: Vec<(bool, String)>,
}
impl Default for BatchDlg {
    fn default() -> Self {
        BatchDlg {
            open: false,
            files: Vec::new(),
            out_dir: None,
            target: Format::Srt,
            suffix: String::new(),
            encoding: "utf-8".into(),
            shift: String::new(),
            fps_from: String::new(),
            fps_to: String::new(),
            fix: false,
            strip: false,
            overwrite: false,
            log: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct Dialogs {
    pub shift: ShiftDlg,
    pub fps: FpsDlg,
    pub sync: SyncDlg,
    pub find: FindDlg,
    pub styles: StylesDlg,
    pub props: PropsDlg,
    pub batch: BatchDlg,
    pub qc_open: bool,
    pub fix_opts: FixOpts,
    pub settings_open: bool,
    pub help_open: bool,
    pub about_open: bool,
    pub focus_text: bool,
    pub editor: EditorState,
    pub tools: crate::tools_dlg::ToolsDlg,
    pub fonts: crate::fonts_dlg::FontsDlg,
    pub speech: crate::speech_dlg::SpeechDlg,
    pub ocr: crate::ocr_dlg::OcrDlg,
    pub spell: crate::spell_dlg::SpellDlg,
    pub fixes: crate::spell_dlg::FixDlg,
}

const ENCODINGS: &[&str] = &["utf-8", "utf-8-bom", "utf-16le", "windows-874", "shift_jis", "gbk", "big5", "euc-kr", "windows-1252"];
const FPS_PRESETS: &[&str] = &["23.976", "24", "25", "29.97", "30", "50", "59.94", "60"];

fn color_edit(ui: &mut egui::Ui, c: &mut Color) -> bool {
    let mut c32 = Color32::from_rgba_unmultiplied(c.r, c.g, c.b, 255 - c.a);
    let r = egui::color_picker::color_edit_button_srgba(ui, &mut c32, egui::color_picker::Alpha::BlendOrAdditive);
    if r.changed() {
        let [r, g, b, a] = c32.to_srgba_unmultiplied();
        *c = Color { r, g, b, a: 255 - a };
        true
    } else {
        false
    }
}

fn time_field(ui: &mut egui::Ui, s: &mut String, vt: Option<i64>) {
    ui.add(egui::TextEdit::singleline(s).desired_width(100.0).font(egui::TextStyle::Monospace));
    if ui.add_enabled(vt.is_some(), btn(icons::ICON_TIMER, "video")).on_hover_text("Use current video time").clicked() {
        *s = fmt_display(vt.unwrap_or(0));
    }
}

impl App {
    pub fn show_dialogs(&mut self, ctx: &egui::Context) {
        self.shift_dialog(ctx);
        self.fps_dialog(ctx);
        self.sync_dialog(ctx);
        self.find_dialog(ctx);
        self.styles_dialog(ctx);
        self.props_dialog(ctx);
        self.batch_dialog(ctx);
        self.qc_dialog(ctx);
        self.settings_dialog(ctx);
        self.help_dialogs(ctx);
    }

    fn shift_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.shift.open;
        let mut apply = false;
        let vt = self.video_time();
        egui::Window::new("Shift times").open(&mut open).resizable(true).collapsible(false).show(ctx, |ui| {
            let d = &mut self.dialogs.shift;
            ui.horizontal(|ui| {
                ui.label("Offset");
                ui.add(egui::TextEdit::singleline(&mut d.offset).desired_width(110.0).font(egui::TextStyle::Monospace));
                ui.label(RichText::new("e.g. 1.5s, 250ms, 0:00:02.300").weak().small());
            });
            if let (Some(t), Some(e)) = (vt, self.cur.and_then(|c| self.doc.sub.events.get(c))) {
                if ui.button("Use (video time - current line start)").clicked() {
                    let diff = t - e.start;
                    d.forward = diff >= 0;
                    d.offset = format!("{:.3}s", diff.abs() as f64 / 1000.0);
                }
            }
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.forward, true, "Forward (later)");
                ui.radio_value(&mut d.forward, false, "Backward (earlier)");
            });
            ui.horizontal(|ui| {
                ui.label("Apply to");
                ui.radio_value(&mut d.scope, 0, "All lines");
                ui.radio_value(&mut d.scope, 1, "Selected");
                ui.radio_value(&mut d.scope, 2, "Current line onward");
            });
            ui.horizontal(|ui| {
                ui.label("Times");
                ui.radio_value(&mut d.field, Field::Both, "Start & end");
                ui.radio_value(&mut d.field, Field::Start, "Start only");
                ui.radio_value(&mut d.field, Field::End, "End only");
            });
            ui.separator();
            if ui.button("Apply").clicked() {
                apply = true;
            }
        });
        self.dialogs.shift.open = open;
        if apply {
            let d = &self.dialogs.shift;
            match parse_offset(&d.offset) {
                Some(ms) => {
                    let ms = if d.forward { ms.abs() } else { -ms.abs() };
                    let scope = match d.scope {
                        1 => Scope::Indices(self.selected()),
                        2 => Scope::From(self.cur.and_then(|c| self.doc.sub.events.get(c)).map(|e| e.start).unwrap_or(0)),
                        _ => Scope::All,
                    };
                    let field = d.field;
                    self.modify(|s| timing::shift(s, ms, &scope, field));
                    self.buf_for = None;
                    self.flash(format!("Shifted by {} ms", ms));
                }
                None => self.flash_err("Invalid offset"),
            }
        }
    }

    fn fps_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.fps.open;
        let mut apply = false;
        let video_fps = self.player.as_ref().map(|p| p.info.fps);
        egui::Window::new("Change frame rate").open(&mut open).resizable(true).collapsible(false).show(ctx, |ui| {
            let d = &mut self.dialogs.fps;
            for (label, v) in [("Subtitles were timed for", &mut d.from), ("Target video frame rate", &mut d.to)] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    ui.add(egui::TextEdit::singleline(v).desired_width(70.0));
                    egui::ComboBox::from_id_salt(label).selected_text("presets").width(70.0).show_ui(ui, |ui| {
                        for p in FPS_PRESETS {
                            if ui.selectable_label(false, *p).clicked() {
                                *v = p.to_string();
                            }
                        }
                    });
                    if let Some(f) = video_fps {
                        if ui.small_button("video").clicked() {
                            *v = format!("{:.3}", f);
                        }
                    }
                });
            }
            ui.label(RichText::new("Times are multiplied by from / to (e.g. 23.976 to 25 speeds up PAL conversions).").weak().small());
            if ui.button("Apply").clicked() {
                apply = true;
            }
        });
        self.dialogs.fps.open = open;
        if apply {
            match (parse_fps(&self.dialogs.fps.from), parse_fps(&self.dialogs.fps.to)) {
                (Some(a), Some(b)) => {
                    self.modify(|s| timing::change_fps(s, a, b, &Scope::All));
                    self.buf_for = None;
                    self.flash(format!("Converted {a:.3} -> {b:.3} fps"));
                }
                _ => self.flash_err("Invalid frame rate"),
            }
        }
    }

    fn sync_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.sync.open;
        let mut apply = false;
        let vt = self.video_time();
        egui::Window::new("Synchronize (two points)").open(&mut open).resizable(true).collapsible(false).show(ctx, |ui| {
            ui.label("Pick two lines far apart. Enter where they are now and where they should be.\nAll times are stretched/shifted linearly. (Prefilled from the first/last selected line.)");
            let d = &mut self.dialogs.sync;
            egui::Grid::new("syncgrid").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("");
                ui.label(RichText::new("current time").strong());
                ui.label(RichText::new("correct time").strong());
                ui.end_row();
                ui.label("Point A");
                ui.add(egui::TextEdit::singleline(&mut d.a_old).desired_width(100.0).font(egui::TextStyle::Monospace));
                ui.horizontal(|ui| time_field(ui, &mut d.a_new, vt));
                ui.end_row();
                ui.label("Point B");
                ui.add(egui::TextEdit::singleline(&mut d.b_old).desired_width(100.0).font(egui::TextStyle::Monospace));
                ui.horizontal(|ui| time_field(ui, &mut d.b_new, vt));
                ui.end_row();
            });
            if ui.button("Apply").clicked() {
                apply = true;
            }
        });
        self.dialogs.sync.open = open;
        if apply {
            let d = &self.dialogs.sync;
            let p = [parse_clock(&d.a_old), parse_clock(&d.a_new), parse_clock(&d.b_old), parse_clock(&d.b_new)];
            if let [Some(ao), Some(an), Some(bo), Some(bn)] = p {
                let r = self.modify(|s| timing::sync_two_points(s, ao, an, bo, bn, &Scope::All));
                match r {
                    Ok((f, o)) => self.flash(format!("Synchronized: ×{:.5} {:+.0} ms", f, o)),
                    Err(e) => {
                        self.undo();
                        self.flash_err(e);
                    }
                }
                self.buf_for = None;
            } else {
                self.flash_err("Invalid time in sync points");
            }
        }
    }

    fn find_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.find.open;
        let mut act: Option<&str> = None;
        let title = if self.dialogs.find.replace_mode { "Find & replace" } else { "Find" };
        egui::Window::new(title).id(egui::Id::new("find_dlg")).open(&mut open).resizable(true).collapsible(false).show(ctx, |ui| {
            let d = &mut self.dialogs.find;
            egui::Grid::new("findgrid").num_columns(2).show(ui, |ui| {
                ui.label("Find");
                let r = ui.add(egui::TextEdit::singleline(&mut d.pattern).desired_width(260.0));
                if d.focus {
                    r.request_focus();
                    d.focus = false;
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    act = Some("next");
                }
                ui.end_row();
                if d.replace_mode {
                    ui.label("Replace");
                    ui.add(egui::TextEdit::singleline(&mut d.replacement).desired_width(260.0));
                    ui.end_row();
                }
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut d.case, "Match case");
                ui.checkbox(&mut d.whole, "Whole word");
                ui.checkbox(&mut d.regex, "Regex");
                egui::ComboBox::from_id_salt("findfield").selected_text(format!("{:?}", d.field)).width(70.0).show_ui(ui, |ui| {
                    for f in [FindField::Text, FindField::Actor, FindField::Style, FindField::Effect] {
                        ui.selectable_value(&mut d.field, f, format!("{f:?}"));
                    }
                });
            });
            ui.horizontal(|ui| {
                if ui.add(btn(icons::ICON_ARROW_BACK, "Prev")).clicked() {
                    act = Some("prev");
                }
                if ui.add(btn(icons::ICON_ARROW_FORWARD, "Next")).clicked() {
                    act = Some("next");
                }
                ui.checkbox(&mut d.replace_mode, "Replace mode");
                if d.replace_mode {
                    if ui.button("Replace").clicked() {
                        act = Some("replace");
                    }
                    if ui.button("Replace all").clicked() {
                        act = Some("all");
                    }
                }
            });
            if !d.msg.is_empty() {
                ui.label(RichText::new(&d.msg).weak());
            }
        });
        self.dialogs.find.open = open;
        let Some(a) = act else { return };
        let d = &self.dialogs.find;
        let opts = FindOpts { pattern: d.pattern.clone(), regex: d.regex, case_sensitive: d.case, whole_word: d.whole, field: d.field };
        let repl = d.replacement.clone();
        let res: Result<String, String> = match a {
            "next" | "prev" => edit::find_next(&self.doc.sub, &opts, self.cur, a == "prev").map(|r| match r {
                Some(i) => {
                    self.select_only(Some(i));
                    format!("Found at line {}", i + 1)
                }
                None => "Not found".into(),
            }),
            "replace" => {
                if let Some(c) = self.cur {
                    let r = self.modify(|s| edit::replace_in(s, c, &opts, &repl));
                    self.buf_for = None;
                    let next = edit::find_next(&self.doc.sub, &opts, self.cur, false).ok().flatten();
                    if let Some(n) = next {
                        self.select_only(Some(n));
                    }
                    r.map(|n| format!("{n} replaced in line {}", c + 1))
                } else {
                    Ok("No line selected".into())
                }
            }
            _ => {
                let r = self.modify(|s| edit::replace_all(s, &opts, &repl));
                self.buf_for = None;
                r.map(|n| format!("{n} replacement(s)"))
            }
        };
        self.dialogs.find.msg = match res {
            Ok(m) => m,
            Err(e) => format!("Error: {e}"),
        };
    }

    fn styles_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.styles.open;
        let mut changed: Option<Subtitle> = None;
        egui::Window::new("Styles manager").open(&mut open).default_width(560.0).show(ctx, |ui| {
            let mut sub = self.doc.sub.clone();
            let mut dirty = false;
            let sel = &mut self.dialogs.styles.sel;
            if sub.styles.is_empty() {
                sub.styles.push(Style::default());
                dirty = true;
            }
            *sel = (*sel).min(sub.styles.len() - 1);
            ui.horizontal_top(|ui| {
                let left = ui.vertical(|ui| {
                    ui.set_width(150.0);
                    egui::ScrollArea::vertical().id_salt("stylelist").max_height(300.0).show(ui, |ui| {
                        for (i, s) in sub.styles.iter().enumerate() {
                            let used = sub.events.iter().filter(|e| e.style == s.name).count();
                            if ui.selectable_label(*sel == i, format!("{}  ({used})", s.name)).clicked() {
                                *sel = i;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("New").clicked() {
                            let mut s = Style::default();
                            let mut n = 1;
                            while sub.styles.iter().any(|x| x.name == format!("Style{n}")) {
                                n += 1;
                            }
                            s.name = format!("Style{n}");
                            sub.styles.push(s);
                            *sel = sub.styles.len() - 1;
                            dirty = true;
                        }
                        if ui.button("Copy").clicked() {
                            let mut s = sub.styles[*sel].clone();
                            s.name = format!("{} copy", s.name);
                            sub.styles.push(s);
                            *sel = sub.styles.len() - 1;
                            dirty = true;
                        }
                        if ui.add_enabled(sub.styles.len() > 1, egui::Button::new("Delete")).clicked() {
                            sub.styles.remove(*sel);
                            *sel = sel.saturating_sub(1);
                            dirty = true;
                        }
                    });
                });
                ui.add_sized([8.0, left.response.rect.height()], egui::Separator::default().vertical());
                ui.vertical(|ui| {
                    let s = &mut sub.styles[*sel];
                    let old_name = s.name.clone();
                    egui::Grid::new("stylegrid").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                        ui.label("Name");
                        dirty |= ui.text_edit_singleline(&mut s.name).changed();
                        ui.end_row();
                        ui.label("Font");
                        ui.horizontal(|ui| {
                            dirty |= ui.add(egui::TextEdit::singleline(&mut s.fontname).desired_width(150.0)).changed();
                            dirty |= ui.add(egui::DragValue::new(&mut s.fontsize).range(1.0..=500.0).speed(0.5)).changed();
                        });
                        ui.end_row();
                        ui.label("Format");
                        ui.horizontal(|ui| {
                            dirty |= ui.checkbox(&mut s.bold, "Bold").changed();
                            dirty |= ui.checkbox(&mut s.italic, "Italic").changed();
                            dirty |= ui.checkbox(&mut s.underline, "Underline").changed();
                            dirty |= ui.checkbox(&mut s.strikeout, "Strike").changed();
                        });
                        ui.end_row();
                        ui.label("Colours");
                        ui.horizontal(|ui| {
                            ui.label("Text");
                            dirty |= color_edit(ui, &mut s.primary);
                            ui.label("Karaoke");
                            dirty |= color_edit(ui, &mut s.secondary);
                            ui.label("Outline");
                            dirty |= color_edit(ui, &mut s.outline);
                            ui.label("Shadow");
                            dirty |= color_edit(ui, &mut s.back);
                        });
                        ui.end_row();
                        ui.label("Border");
                        ui.horizontal(|ui| {
                            let mut opaque = s.border_style == 3;
                            if ui.checkbox(&mut opaque, "Opaque box").changed() {
                                s.border_style = if opaque { 3 } else { 1 };
                                dirty = true;
                            }
                            ui.label("Outline");
                            dirty |= ui.add(egui::DragValue::new(&mut s.outline_w).range(0.0..=50.0).speed(0.1)).changed();
                            ui.label("Shadow");
                            dirty |= ui.add(egui::DragValue::new(&mut s.shadow).range(0.0..=50.0).speed(0.1)).changed();
                        });
                        ui.end_row();
                        ui.label("Alignment");
                        egui::Grid::new("aligngrid").spacing([2.0, 2.0]).show(ui, |ui| {
                            for row in [[7, 8, 9], [4, 5, 6], [1, 2, 3]] {
                                for a in row {
                                    if ui.selectable_label(s.alignment == a, a.to_string()).clicked() {
                                        s.alignment = a;
                                        dirty = true;
                                    }
                                }
                                ui.end_row();
                            }
                        });
                        ui.end_row();
                        ui.label("Margins L/R/V");
                        ui.horizontal(|ui| {
                            dirty |= ui.add(egui::DragValue::new(&mut s.margin_l).range(0..=5000)).changed();
                            dirty |= ui.add(egui::DragValue::new(&mut s.margin_r).range(0..=5000)).changed();
                            dirty |= ui.add(egui::DragValue::new(&mut s.margin_v).range(0..=5000)).changed();
                        });
                        ui.end_row();
                        ui.label("Scale X/Y %");
                        ui.horizontal(|ui| {
                            dirty |= ui.add(egui::DragValue::new(&mut s.scale_x).range(1.0..=1000.0)).changed();
                            dirty |= ui.add(egui::DragValue::new(&mut s.scale_y).range(1.0..=1000.0)).changed();
                            ui.label("Spacing");
                            dirty |= ui.add(egui::DragValue::new(&mut s.spacing).speed(0.1)).changed();
                            ui.label("Angle");
                            dirty |= ui.add(egui::DragValue::new(&mut s.angle).speed(0.5)).changed();
                        });
                        ui.end_row();
                    });
                    let new_name = s.name.clone();
                    if new_name != old_name && !new_name.is_empty() {
                        for e in sub.events.iter_mut() {
                            if e.style == old_name {
                                e.style = new_name.clone();
                            }
                        }
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new("Renaming a style updates the lines that use it.").weak().small());
                });
            });
            if dirty {
                changed = Some(sub);
            }
        });
        self.dialogs.styles.open = open;
        if let Some(sub) = changed {
            // collapse a burst of drags into one undo step
            if !self.text_session {
                self.checkpoint();
                self.text_session = true;
            }
            self.doc.sub = sub;
            self.touch();
        }
    }

    fn props_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.props.open;
        let video_res = self.player.as_ref().map(|p| (p.info.width, p.info.height));
        let mut new_info: Option<Vec<(String, String)>> = None;
        egui::Window::new("Script properties").open(&mut open).resizable(true).show(ctx, |ui| {
            let mut sub = self.doc.sub.clone();
            let mut dirty = false;
            let keys = ["Title", "Original Script", "Original Translation", "Original Timing", "PlayResX", "PlayResY", "WrapStyle", "ScaledBorderAndShadow", "YCbCr Matrix"];
            egui::Grid::new("props").num_columns(2).show(ui, |ui| {
                for k in keys {
                    ui.label(k);
                    let mut v = sub.info_get(k).unwrap_or("").to_string();
                    if ui.add(egui::TextEdit::singleline(&mut v).desired_width(220.0)).changed() {
                        sub.info_set(k, v);
                        dirty = true;
                    }
                    ui.end_row();
                }
            });
            if let Some((w, h)) = video_res {
                if ui.button(format!("Set PlayRes from video ({w}×{h})")).clicked() {
                    sub.info_set("PlayResX", w.to_string());
                    sub.info_set("PlayResY", h.to_string());
                    dirty = true;
                }
            }
            ui.label(RichText::new("WrapStyle: 0 smart, 1 end-of-line, 2 none, 3 smart (lower wider)").weak().small());
            if dirty {
                new_info = Some(sub.info);
            }
        });
        self.dialogs.props.open = open;
        if let Some(info) = new_info {
            if !self.text_session {
                self.checkpoint();
                self.text_session = true;
            }
            self.doc.sub.info = info;
            self.touch();
        }
    }

    fn batch_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.batch.open;
        let mut run = false;
        egui::Window::new("Batch convert").open(&mut open).default_width(560.0).show(ctx, |ui| {
            let d = &mut self.dialogs.batch;
            ui.horizontal(|ui| {
                if ui.button("Add files…").clicked() {
                    if let Some(v) = rfd::FileDialog::new().add_filter("Subtitles", &["srt", "ass", "ssa", "vtt"]).pick_files() {
                        d.files.extend(v);
                    }
                }
                if ui.button("Add folder…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        d.files.extend(subforge_core::collect_inputs(&[dir], true));
                    }
                }
                if ui.button("Clear").clicked() {
                    d.files.clear();
                    d.log.clear();
                }
                ui.label(RichText::new(format!("{} file(s) — you can also drop files here", d.files.len())).weak());
            });
            egui::ScrollArea::vertical().id_salt("batchfiles").max_height(140.0).show(ui, |ui| {
                let mut remove = None;
                for (i, f) in d.files.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.add(ibtn(icons::ICON_CLOSE)).clicked() {
                            remove = Some(i);
                        }
                        ui.label(f.display().to_string());
                    });
                }
                if let Some(i) = remove {
                    d.files.remove(i);
                }
            });
            ui.separator();
            egui::Grid::new("batchopts").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Target format");
                ui.horizontal(|ui| {
                    for f in Format::ALL {
                        ui.radio_value(&mut d.target, f, f.ext().to_uppercase());
                    }
                });
                ui.end_row();
                ui.label("Output folder");
                ui.horizontal(|ui| {
                    ui.label(d.out_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(same as input)".into()));
                    if ui.button("Choose…").clicked() {
                        d.out_dir = rfd::FileDialog::new().pick_folder();
                    }
                    if d.out_dir.is_some() && ui.small_button("reset").clicked() {
                        d.out_dir = None;
                    }
                });
                ui.end_row();
                ui.label("Name suffix");
                ui.add(egui::TextEdit::singleline(&mut d.suffix).hint_text("e.g. .th").desired_width(100.0));
                ui.end_row();
                ui.label("Encoding");
                egui::ComboBox::from_id_salt("batchenc").selected_text(&d.encoding).show_ui(ui, |ui| {
                    for e in ENCODINGS {
                        ui.selectable_value(&mut d.encoding, e.to_string(), *e);
                    }
                });
                ui.end_row();
                ui.label("Shift");
                ui.add(egui::TextEdit::singleline(&mut d.shift).hint_text("e.g. -1.2s (empty = none)").desired_width(140.0));
                ui.end_row();
                ui.label("Frame rate");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.fps_from).hint_text("from").desired_width(60.0));
                    ui.label("→");
                    ui.add(egui::TextEdit::singleline(&mut d.fps_to).hint_text("to").desired_width(60.0));
                });
                ui.end_row();
                ui.label("Options");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.fix, "Auto-fix (QC)");
                    ui.checkbox(&mut d.strip, "Strip tags");
                    ui.checkbox(&mut d.overwrite, "Overwrite existing");
                });
                ui.end_row();
            });
            ui.separator();
            if ui.add_enabled(!d.files.is_empty(), btn(icons::ICON_PLAY_ARROW, RichText::new("Convert").strong())).clicked() {
                run = true;
            }
            if !d.log.is_empty() {
                egui::ScrollArea::vertical().id_salt("batchlog").max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                    for (ok, l) in &d.log {
                        ui.label(RichText::new(l).color(if *ok { Color32::from_rgb(90, 190, 110) } else { Color32::from_rgb(230, 90, 80) }));
                    }
                });
            }
        });
        self.dialogs.batch.open = open;
        if run {
            self.run_batch();
        }
    }

    fn run_batch(&mut self) {
        let qc_cfg = self.settings.qc.clone();
        let d = &mut self.dialogs.batch;
        d.log.clear();
        let shift = if d.shift.trim().is_empty() { None } else { parse_offset(&d.shift) };
        if !d.shift.trim().is_empty() && shift.is_none() {
            d.log.push((false, format!("Invalid shift: {}", d.shift)));
            return;
        }
        let fps = match (d.fps_from.trim(), d.fps_to.trim()) {
            ("", "") => None,
            (a, b) => match (parse_fps(a), parse_fps(b)) {
                (Some(a), Some(b)) => Some((a, b)),
                _ => {
                    d.log.push((false, "Invalid frame rate".into()));
                    return;
                }
            },
        };
        if let Some(o) = &d.out_dir {
            let _ = std::fs::create_dir_all(o);
        }
        let (mut ok, mut bad) = (0, 0);
        for f in d.files.clone() {
            let r = (|| -> Result<PathBuf, String> {
                let l = subforge_core::load(&f, None).map_err(|e| e.to_string())?;
                let mut sub = l.sub;
                if let Some((a, b)) = fps {
                    timing::change_fps(&mut sub, a, b, &Scope::All);
                }
                if let Some(ms) = shift {
                    timing::shift(&mut sub, ms, &Scope::All, Field::Both);
                }
                if d.fix {
                    qc::fix(&mut sub, &qc_cfg, &FixOpts::default());
                }
                if d.strip {
                    edit::strip_all_tags(&mut sub);
                }
                subforge_core::prepare_for(&mut sub, d.target);
                let stem = f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "out".into());
                let dir = d.out_dir.clone().unwrap_or_else(|| f.parent().map(|p| p.to_path_buf()).unwrap_or_default());
                let dest = dir.join(format!("{}{}.{}", stem, d.suffix, d.target.ext()));
                if dest == f && !d.overwrite {
                    return Err("would overwrite the input (add a suffix or tick Overwrite)".into());
                }
                if dest.exists() && !d.overwrite {
                    return Err(format!("{} exists", dest.display()));
                }
                subforge_core::save(&dest, &sub, Some(d.target), &WriteOpts { encoding: d.encoding.clone(), ..Default::default() }).map_err(|e| e.to_string())?;
                Ok(dest)
            })();
            match r {
                Ok(dest) => {
                    ok += 1;
                    d.log.push((true, format!("OK  {} -> {}", f.display(), dest.display())));
                }
                Err(e) => {
                    bad += 1;
                    d.log.push((false, format!("FAILED  {}: {}", f.display(), e)));
                }
            }
        }
        d.log.push((bad == 0, format!("Done: {ok} converted, {bad} failed")));
    }

    fn qc_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.qc_open;
        let mut run_fix = false;
        egui::Window::new("Quality check settings & auto-fix").open(&mut open).resizable(true).show(ctx, |ui| {
            let q = &mut self.settings.qc;
            egui::Grid::new("qcgrid").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                ui.label("Max CPS");
                ui.add(egui::DragValue::new(&mut q.max_cps).range(0.0..=100.0).speed(0.1));
                ui.end_row();
                ui.label("Max line length");
                ui.add(egui::DragValue::new(&mut q.max_line_len).range(0..=200));
                ui.end_row();
                ui.label("Max lines");
                ui.add(egui::DragValue::new(&mut q.max_lines).range(0..=10));
                ui.end_row();
                ui.label("Min duration (ms)");
                ui.add(egui::DragValue::new(&mut q.min_duration_ms).range(0..=10_000).speed(10));
                ui.end_row();
                ui.label("Max duration (ms)");
                ui.add(egui::DragValue::new(&mut q.max_duration_ms).range(0..=60_000).speed(10));
                ui.end_row();
                ui.label("Min gap (ms)");
                ui.add(egui::DragValue::new(&mut q.min_gap_ms).range(0..=2000));
                ui.end_row();
                ui.label("Counting");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut q.count.ignore_spaces, "ignore spaces");
                    ui.checkbox(&mut q.count.ignore_punctuation, "ignore punctuation");
                });
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.label("Presets:");
                if ui.small_button("Netflix-ish (20 cps / 42)").clicked() {
                    *q = QcConfig::default();
                }
                if ui.small_button("Thai (lenient 25/45)").clicked() {
                    *q = QcConfig { max_cps: 25.0, max_line_len: 45, ..QcConfig::default() };
                }
                if ui.small_button("Japanese (13 cps / 16)").clicked() {
                    *q = QcConfig { max_cps: 13.0, max_line_len: 16, ..QcConfig::default() };
                }
                if ui.small_button("Kids (15/37)").clicked() {
                    *q = QcConfig { max_cps: 15.0, max_line_len: 37, ..QcConfig::default() };
                }
            });
            ui.label(RichText::new("Lengths count grapheme clusters, so Thai vowel/tone marks don't add characters.").weak().small());
            ui.separator();
            ui.heading("Auto-fix");
            let f = &mut self.dialogs.fix_opts;
            ui.checkbox(&mut f.trim_spaces, "Trim / collapse spaces");
            ui.checkbox(&mut f.remove_empty, "Remove empty lines");
            ui.checkbox(&mut f.fix_invalid, "Fix end ≤ start");
            ui.checkbox(&mut f.fix_overlaps, "Fix overlaps (same style)");
            ui.checkbox(&mut f.enforce_min_gap, "Enforce minimum gap");
            ui.checkbox(&mut f.extend_short, "Extend too-short lines");
            ui.checkbox(&mut f.extend_for_cps, "Extend lines to meet max CPS (when room)");
            ui.checkbox(&mut f.cap_long, "Cut lines longer than max duration");
            ui.checkbox(&mut f.sort, "Sort by start time");
            ui.checkbox(&mut f.rebalance_long, "Break over-long single lines in two (word-aware for Thai)");
            if ui.button(RichText::new("Run auto-fix").strong()).clicked() {
                run_fix = true;
            }
        });
        self.dialogs.qc_open = open;
        if run_fix {
            let (cfg, opts) = (self.settings.qc.clone(), self.dialogs.fix_opts.clone());
            let rep = self.modify(|s| qc::fix(s, &cfg, &opts));
            self.buf_for = None;
            self.flash(format!(
                "Auto-fix: {} changes ({} trimmed, {} empty removed, {} overlaps, {} extended, {} gaps, {} capped)",
                rep.total(),
                rep.trimmed,
                rep.removed_empty,
                rep.fixed_overlaps,
                rep.extended,
                rep.gaps_enforced,
                rep.capped
            ));
        }
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.settings_open;
        let mut relocate = false;
        let mut refont = false;
        egui::Window::new("Settings").open(&mut open).resizable(true).show(ctx, |ui| {
            let s = &mut self.settings;
            egui::Grid::new("settingsgrid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("ffmpeg folder");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut s.ffmpeg_dir).hint_text("empty = PATH / next to exe").desired_width(240.0));
                    if ui.button("…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            s.ffmpeg_dir = d.display().to_string();
                        }
                    }
                    if ui.button("Test").clicked() {
                        relocate = true;
                    }
                });
                ui.end_row();
                ui.label("");
                ui.label(RichText::new(if self.ffmpeg_ok { format!("using {}", self.tools.ffmpeg.display()) } else { "ffmpeg not found".into() }).small());
                ui.end_row();
                ui.label("Extra UI font");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut s.ui_font).hint_text("optional .ttf/.otf/.ttc").desired_width(240.0));
                    if ui.button("…").clicked() {
                        if let Some(f) = rfd::FileDialog::new().add_filter("Fonts", &["ttf", "otf", "ttc"]).pick_file() {
                            s.ui_font = f.display().to_string();
                            refont = true;
                        }
                    }
                });
                ui.end_row();
                ui.label("Save encoding");
                egui::ComboBox::from_id_salt("saveenc").selected_text(&s.save_encoding).show_ui(ui, |ui| {
                    for e in ENCODINGS {
                        ui.selectable_value(&mut s.save_encoding, e.to_string(), *e);
                    }
                });
                ui.end_row();
                ui.label("New line duration (ms)");
                ui.add(egui::DragValue::new(&mut s.default_duration_ms).range(100..=20_000).speed(10));
                ui.end_row();
                ui.label("Seek step (←/→, ms)");
                ui.add(egui::DragValue::new(&mut s.seek_step_ms).range(100..=60_000).speed(10));
                ui.end_row();
                ui.label("Video preview width");
                ui.add(egui::DragValue::new(&mut s.video_max_width).range(320..=3840).speed(10)).on_hover_text("Lower = less CPU; applies to newly opened videos");
                ui.end_row();
                ui.label("Auto-open video");
                ui.checkbox(&mut s.autoload_video, "open a video with the same name as the subtitle");
                ui.end_row();
                ui.label("Accurate preview");
                ui.add_enabled(self.has_libass, egui::Checkbox::new(&mut s.libass_preview, "render paused frames with libass")).on_disabled_hover_text("this ffmpeg build has no libass (subtitles filter)");
                ui.end_row();
                ui.label("Disk cache");
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::cache::dir().display().to_string()).small().weak());
                    if ui.small_button("Clear").on_hover_text("Waveforms, keyframes and extracted fonts").clicked() {
                        let _ = std::fs::remove_dir_all(crate::cache::dir());
                    }
                });
                ui.end_row();
                ui.label("Autosave (minutes)");
                ui.add(egui::DragValue::new(&mut s.autosave_minutes).range(0..=60)).on_hover_text(format!("Backup copy in {} (0 = off)", std::env::temp_dir().join("subforge-autosave").display()));
                ui.end_row();
                ui.label("Theme");
                ui.horizontal(|ui| {
                    if ui.radio_value(&mut s.dark, true, "Dark").changed() | ui.radio_value(&mut s.dark, false, "Light").changed() {
                        apply_theme(ui.ctx(), s);
                    }
                });
                ui.end_row();
            });
            if ui.button("Reload fonts").clicked() {
                refont = true;
            }
        });
        self.dialogs.settings_open = open;
        if relocate {
            self.tools = crate::media::Tools::locate(&self.settings.ffmpeg_dir);
            self.ffmpeg_ok = self.tools.available();
            self.has_libass = self.ffmpeg_ok && self.tools.has_libass();
        }
        if refont {
            crate::fonts::install(ctx, Some(&self.settings.ui_font));
        }
    }

    fn help_dialogs(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.help_open;
        egui::Window::new("Keyboard shortcuts").open(&mut open).resizable(true).show(ctx, |ui| {
            egui::Grid::new("keys").striped(true).show(ui, |ui| {
                for (k, v) in [
                    ("Space / Alt+Space", "Play / pause"),
                    ("← / →", "Seek − / + step (Shift = ×5)"),
                    ("Ctrl+← / Ctrl+→", "Previous / next frame"),
                    ("F5 / Ctrl+P", "Play current line"),
                    ("F7 / Ctrl+[", "Set start to video time"),
                    ("F8 / Ctrl+]", "Set end to video time"),
                    ("↑ / ↓  (Ctrl in text)", "Previous / next line"),
                    ("Ctrl+Enter", "Next line (creates one at the end)"),
                    ("Enter", "Focus text editor"),
                    ("Ctrl+I / Ctrl+D", "Insert after / duplicate"),
                    ("Ctrl+K / Ctrl+J", "Split / join"),
                    ("M", "Toggle the Move tool (drag lines on the video)"),
                ("Alt+← / Alt+→", "Previous / next keyframe"),
                ("F6", "Snap selected lines to keyframes"),
                    ("Delete", "Delete selected lines"),
                    ("Ctrl+F / Ctrl+H", "Find / replace"),
                    ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
                    ("Ctrl+S / Ctrl+Shift+S", "Save / save as"),
                    ("F9", "Quality check panel"),
                    ("Timeline", "drag edges/body · Alt = no snap · Ctrl+wheel zoom · right-click menu"),
                ] {
                    ui.label(RichText::new(k).monospace());
                    ui.label(v);
                    ui.end_row();
                }
            });
        });
        self.dialogs.help_open = open;
        let mut open = self.dialogs.about_open;
        egui::Window::new("About SubForge").open(&mut open).resizable(true).show(ctx, |ui| {
            ui.heading(format!("SubForge {}", env!("CARGO_PKG_VERSION")));
            ui.label("Subtitle editor & converter — SRT / ASS / SSA / VTT");
            ui.label("Rust + egui. Video preview via ffmpeg.");
            ui.label("CLI: subforge-cli --help");
        });
        self.dialogs.about_open = open;
    }
}
