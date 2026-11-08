//! Dialogs for batch editing tools.

use crate::app::App;
use crate::icons::{self, btn};
use eframe::egui::{self, RichText};
use subforge_core::time::parse_offset;
use subforge_core::timing::Scope;
use subforge_core::tools::{self, Case, DurationMode, HiOpts};

#[derive(PartialEq, Clone, Copy)]
enum DurKind {
    Add,
    Percent,
    Fixed,
    Cps,
}

pub struct ToolsDlg {
    pub durations_open: bool,
    dur_kind: DurKind,
    dur_add: String,
    dur_pct: f64,
    dur_fixed: String,
    dur_cps: f64,
    dur_min: i64,
    dur_max: i64,
    dur_gap: i64,
    dur_selected_only: bool,

    pub bridge_open: bool,
    bridge_max: i64,
    bridge_min: i64,
    bridge_selected_only: bool,

    pub hi_open: bool,
    hi: HiOpts,
    hi_selected_only: bool,

    pub case_open: bool,
    case: Case,
    case_selected_only: bool,

    pub resample_open: bool,
    pub resample_to: Option<(i32, i32)>,
}

impl Default for ToolsDlg {
    fn default() -> Self {
        ToolsDlg {
            durations_open: false,
            dur_kind: DurKind::Cps,
            dur_add: "+300ms".into(),
            dur_pct: 110.0,
            dur_fixed: "2s".into(),
            dur_cps: 17.0,
            dur_min: 833,
            dur_max: 7000,
            dur_gap: 83,
            dur_selected_only: false,
            bridge_open: false,
            bridge_max: 500,
            bridge_min: 83,
            bridge_selected_only: false,
            hi_open: false,
            hi: HiOpts::default(),
            hi_selected_only: false,
            case_open: false,
            case: Case::Sentence,
            case_selected_only: true,
            resample_open: false,
            resample_to: None,
        }
    }
}

fn scope(app: &App, selected_only: bool) -> Scope {
    if selected_only {
        Scope::Indices(app.selected())
    } else {
        Scope::All
    }
}

impl App {
    pub fn tools_dialogs(&mut self, ctx: &egui::Context) {
        self.durations_dialog(ctx);
        self.bridge_dialog(ctx);
        self.hi_dialog(ctx);
        self.case_dialog(ctx);
        self.resample_dialog(ctx);
    }

    fn durations_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.tools.durations_open;
        let mut apply = false;
        egui::Window::new("Adjust durations").open(&mut open).resizable(true).collapsible(false).default_width(380.0).show(ctx, |ui| {
            let d = &mut self.dialogs.tools;
            ui.radio_value(&mut d.dur_kind, DurKind::Cps, "Extend to reach reading speed (CPS)");
            ui.indent("cps", |ui| {
                ui.add_enabled(d.dur_kind == DurKind::Cps, egui::DragValue::new(&mut d.dur_cps).range(5.0..=40.0).speed(0.1).suffix(" chars/s"));
            });
            ui.radio_value(&mut d.dur_kind, DurKind::Add, "Add / subtract time");
            ui.indent("add", |ui| {
                ui.add_enabled(d.dur_kind == DurKind::Add, egui::TextEdit::singleline(&mut d.dur_add).desired_width(100.0).hint_text("+300ms / -0.2s"));
            });
            ui.radio_value(&mut d.dur_kind, DurKind::Percent, "Scale by percentage");
            ui.indent("pct", |ui| {
                ui.add_enabled(d.dur_kind == DurKind::Percent, egui::DragValue::new(&mut d.dur_pct).range(10.0..=500.0).suffix(" %"));
            });
            ui.radio_value(&mut d.dur_kind, DurKind::Fixed, "Set a fixed duration");
            ui.indent("fixed", |ui| {
                ui.add_enabled(d.dur_kind == DurKind::Fixed, egui::TextEdit::singleline(&mut d.dur_fixed).desired_width(100.0));
            });
            ui.separator();
            egui::Grid::new("dur_limits").num_columns(2).show(ui, |ui| {
                ui.label("Minimum");
                ui.add(egui::DragValue::new(&mut d.dur_min).range(0..=10_000).suffix(" ms"));
                ui.end_row();
                ui.label("Maximum");
                ui.add(egui::DragValue::new(&mut d.dur_max).range(0..=60_000).suffix(" ms"));
                ui.end_row();
                ui.label("Keep gap to next");
                ui.add(egui::DragValue::new(&mut d.dur_gap).range(0..=2000).suffix(" ms"));
                ui.end_row();
            });
            ui.checkbox(&mut d.dur_selected_only, "Selected lines only");
            if ui.add(btn(icons::ICON_DONE, "Apply")).clicked() {
                apply = true;
            }
        });
        self.dialogs.tools.durations_open = open;
        if apply {
            let d = &self.dialogs.tools;
            let mode = match d.dur_kind {
                DurKind::Cps => Some(DurationMode::TargetCps(d.dur_cps)),
                DurKind::Percent => Some(DurationMode::Percent(d.dur_pct)),
                DurKind::Add => parse_offset(&d.dur_add).map(DurationMode::AddMs),
                DurKind::Fixed => parse_offset(&d.dur_fixed).map(DurationMode::Fixed),
            };
            let Some(mode) = mode else {
                self.flash_err("Invalid time value");
                return;
            };
            let (gap, mn, mx) = (d.dur_gap, d.dur_min, d.dur_max);
            let sc = scope(self, d.dur_selected_only);
            let n = self.modify(|s| tools::adjust_durations(s, mode, &sc, Some(gap), mn, mx));
            self.buf_for = None;
            self.flash(format!("Durations: {n} line(s) changed"));
        }
    }

    fn bridge_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.tools.bridge_open;
        let mut apply = false;
        egui::Window::new("Bridge gaps").open(&mut open).resizable(true).collapsible(false).show(ctx, |ui| {
            let d = &mut self.dialogs.tools;
            ui.label("Extend lines so that small gaps to the next line are closed (chaining).");
            egui::Grid::new("bridge").num_columns(2).show(ui, |ui| {
                ui.label("Bridge gaps shorter than");
                ui.add(egui::DragValue::new(&mut d.bridge_max).range(1..=5000).suffix(" ms"));
                ui.end_row();
                ui.label("Leave a gap of");
                ui.add(egui::DragValue::new(&mut d.bridge_min).range(0..=1000).suffix(" ms"));
                ui.end_row();
            });
            ui.checkbox(&mut d.bridge_selected_only, "Selected lines only");
            if ui.add(btn(icons::ICON_LINK, "Bridge")).clicked() {
                apply = true;
            }
        });
        self.dialogs.tools.bridge_open = open;
        if apply {
            let d = &self.dialogs.tools;
            let (mx, mn) = (d.bridge_max, d.bridge_min);
            let sc = scope(self, d.bridge_selected_only);
            let n = self.modify(|s| tools::bridge_gaps(s, mx, mn, &sc));
            self.buf_for = None;
            self.flash(format!("Bridged {n} gap(s)"));
        }
    }

    fn hi_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.tools.hi_open;
        let mut apply = false;
        egui::Window::new("Remove text for hearing impaired").open(&mut open).resizable(true).collapsible(false).show(ctx, |ui| {
            let d = &mut self.dialogs.tools;
            ui.checkbox(&mut d.hi.brackets, "Text in [brackets]");
            ui.checkbox(&mut d.hi.parens, "Text in (parentheses)");
            ui.checkbox(&mut d.hi.speakers, "Speaker labels (\"JOHN:\")");
            ui.indent("spk", |ui| {
                ui.add_enabled(d.hi.speakers, egui::Checkbox::new(&mut d.hi.speakers_uppercase_only, "only UPPERCASE labels"));
            });
            ui.checkbox(&mut d.hi.music, "Lines with only ♪ / music symbols");
            ui.checkbox(&mut d.hi_selected_only, "Selected lines only");
            ui.label(RichText::new("Override tags are kept; lines that become empty are removed.").small().weak());
            if ui.add(btn(icons::ICON_HEARING_DISABLED, "Remove")).clicked() {
                apply = true;
            }
        });
        self.dialogs.tools.hi_open = open;
        if apply {
            let d = &self.dialogs.tools;
            let opts = d.hi;
            let sc = scope(self, d.hi_selected_only);
            let (c, r) = self.modify(|s| tools::remove_hi(s, opts, &sc));
            self.buf_for = None;
            self.flash(format!("HI removal: {c} line(s) changed, {r} removed"));
        }
    }

    fn case_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.tools.case_open;
        let mut apply = false;
        egui::Window::new("Change casing").open(&mut open).resizable(true).collapsible(false).show(ctx, |ui| {
            let d = &mut self.dialogs.tools;
            ui.radio_value(&mut d.case, Case::Sentence, "Sentence case");
            ui.radio_value(&mut d.case, Case::Title, "Title Case");
            ui.radio_value(&mut d.case, Case::Upper, "UPPER CASE");
            ui.radio_value(&mut d.case, Case::Lower, "lower case");
            ui.checkbox(&mut d.case_selected_only, "Selected lines only");
            if ui.add(btn(icons::ICON_TEXT_FORMAT, "Apply")).clicked() {
                apply = true;
            }
        });
        self.dialogs.tools.case_open = open;
        if apply {
            let d = &self.dialogs.tools;
            let c = d.case;
            let sc = scope(self, d.case_selected_only);
            let n = self.modify(|s| tools::change_case(s, c, &sc));
            self.buf_for = None;
            self.flash(format!("Casing changed on {n} line(s)"));
        }
    }

    fn resample_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.tools.resample_open;
        let mut apply = None;
        let from = self.doc.sub.play_res();
        let video = self.player.as_ref().map(|p| (p.info.width as i32, p.info.height as i32));
        egui::Window::new("Resample resolution").open(&mut open).resizable(true).collapsible(false).show(ctx, |ui| {
            let to = self.dialogs.tools.resample_to.get_or_insert(video.unwrap_or((1920, 1080)));
            ui.label(format!("Current script resolution: {} × {}", from.0, from.1));
            ui.horizontal(|ui| {
                ui.label("New");
                ui.add(egui::DragValue::new(&mut to.0).range(16..=16384));
                ui.label("×");
                ui.add(egui::DragValue::new(&mut to.1).range(16..=16384));
            });
            ui.horizontal_wrapped(|ui| {
                for (w, h) in [(640, 360), (1280, 720), (1920, 1080), (3840, 2160)] {
                    if ui.small_button(format!("{w}×{h}")).clicked() {
                        *to = (w, h);
                    }
                }
                if let Some(v) = video {
                    if ui.small_button(format!("video {}×{}", v.0, v.1)).clicked() {
                        *to = v;
                    }
                }
            });
            ui.label(RichText::new("Scales styles (font size, outline, shadow, margins), \\pos/\\move/\\org, clips (rect & vector), \\fs/\\bord/\\shad/\\blur inside \\t, and drawings — so typesetting looks the same at the new PlayRes.").small().weak());
            if ui.add(btn(icons::ICON_ASPECT_RATIO, "Resample")).clicked() {
                apply = Some(*to);
            }
        });
        self.dialogs.tools.resample_open = open;
        if let Some(to) = apply {
            self.modify(|s| tools::resample(s, to));
            self.buf_for = None;
            self.flash(format!("Resampled {}×{} → {}×{}", from.0, from.1, to.0, to.1));
        }
    }
}
