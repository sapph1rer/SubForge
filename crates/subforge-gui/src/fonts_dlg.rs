//! Fonts window (which fonts the script needs and where they are) and the MKV mux window.

use crate::app::App;
use crate::icons::{self, btn, ic_s};
use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use subforge_core::fonts::{self, FontIndex, FontReport, Source, Status};

const INDEX_MAGIC: &[u8; 4] = b"SFF1";

/// System font index, built once in the background (cached on disk between runs).
pub struct FontDb {
    pub index: Arc<Mutex<Option<Arc<FontIndex>>>>,
    pub progress: Arc<AtomicU32>,
    pub version: Arc<AtomicU32>,
    pub busy: Arc<AtomicBool>,
}

impl FontDb {
    pub fn start(ctx: &egui::Context) -> FontDb {
        let db = FontDb { index: Arc::new(Mutex::new(None)), progress: Arc::new(AtomicU32::new(0)), version: Arc::new(AtomicU32::new(0)), busy: Arc::new(AtomicBool::new(false)) };
        db.rescan(ctx);
        db
    }
    pub fn rescan(&self, ctx: &egui::Context) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (idx, prog, ver, busy, ctx) = (self.index.clone(), self.progress.clone(), self.version.clone(), self.busy.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("font-index".into())
            .spawn(move || {
                let prev: Option<FontIndex> = crate::cache::load("system", "fontidx", INDEX_MAGIC).and_then(|b| serde_json::from_slice(&b).ok());
                if let Some(p) = &prev {
                    // usable immediately while the rescan checks for changes
                    *idx.lock().unwrap() = Some(Arc::new(p.clone()));
                    ver.fetch_add(1, Ordering::SeqCst);
                }
                let fresh = FontIndex::scan(&FontIndex::system_dirs(), prev.as_ref(), |d, t| {
                    prog.store(if t == 0 { 1000 } else { (d * 1000 / t) as u32 }, Ordering::Relaxed);
                });
                if let Ok(b) = serde_json::to_vec(&fresh) {
                    crate::cache::save("system", "fontidx", INDEX_MAGIC, &b);
                }
                *idx.lock().unwrap() = Some(Arc::new(fresh));
                ver.fetch_add(1, Ordering::SeqCst);
                busy.store(false, Ordering::SeqCst);
                ctx.request_repaint();
            })
            .ok();
    }
    pub fn get(&self) -> Option<Arc<FontIndex>> {
        self.index.lock().unwrap().clone()
    }
}

pub struct FontsDlg {
    pub open: bool,
    /// (doc gen, index version, video path, extra dir) the report was computed for
    key: Option<(u64, u32, Option<PathBuf>, String)>,
    pub reports: Vec<FontReport>,
    attached: Option<(PathBuf, Arc<FontIndex>)>,
    folder: Option<(String, Arc<FontIndex>)>,
    embedded: Option<(usize, Arc<FontIndex>)>,
    last_calc: Option<std::time::Instant>,
    pub mux: MuxDlg,
}

impl Default for FontsDlg {
    fn default() -> Self {
        FontsDlg { open: false, key: None, reports: Vec::new(), attached: None, folder: None, embedded: None, last_calc: None, mux: MuxDlg::default() }
    }
}

pub struct MuxDlg {
    pub open: bool,
    pub video: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub language: String,
    pub title: String,
    pub default: bool,
    pub replace_subs: bool,
    pub attach_fonts: bool,
}

impl Default for MuxDlg {
    fn default() -> Self {
        MuxDlg { open: false, video: None, output: None, language: "tha".into(), title: String::new(), default: true, replace_subs: false, attach_fonts: true }
    }
}

const LANGS: &[(&str, &str)] = &[("tha", "Thai"), ("eng", "English"), ("jpn", "Japanese"), ("chi", "Chinese"), ("kor", "Korean"), ("ind", "Indonesian"), ("vie", "Vietnamese"), ("und", "Undetermined")];

impl App {
    /// Recompute the font report if the script, fonts or video changed.
    pub fn refresh_font_report(&mut self, force: bool) {
        let ver = self.font_db.version.load(Ordering::Relaxed);
        let video = self.player.as_ref().and_then(|p| p.work_dir.as_ref().map(|w| w.join("fonts")));
        let extra = self.settings.fonts_dir.clone();
        let key = (self.doc.gen, ver, video.clone(), extra.clone());
        if !force && self.dialogs.fonts.key.as_ref() == Some(&key) {
            return;
        }
        // while typing, don't recheck on every keystroke
        if !force && self.dialogs.fonts.key.as_ref().is_some_and(|k| k.1 == key.1 && k.2 == key.2 && k.3 == key.3) && self.dialogs.fonts.last_calc.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(800)) {
            return;
        }
        let system = self.font_db.get().unwrap_or_default();
        let d = &mut self.dialogs.fonts;
        // attached fonts (extracted from the MKV into the cache when the video was opened)
        let attached = match &video {
            Some(dir) => match &d.attached {
                Some((p, idx)) if p == dir && !force => idx.clone(),
                _ => {
                    let idx = Arc::new(FontIndex::scan(std::slice::from_ref(dir), None, |_, _| {}));
                    d.attached = Some((dir.clone(), idx.clone()));
                    idx
                }
            },
            None => Arc::new(FontIndex::default()),
        };
        let folder = if extra.trim().is_empty() {
            Arc::new(FontIndex::default())
        } else {
            match &d.folder {
                Some((p, idx)) if *p == extra && !force => idx.clone(),
                _ => {
                    let idx = Arc::new(FontIndex::scan(&[PathBuf::from(extra.trim())], None, |_, _| {}));
                    d.folder = Some((extra.clone(), idx.clone()));
                    idx
                }
            }
        };
        let fonts_len: usize = self.doc.sub.extra_sections.iter().filter(|(n, _)| n.eq_ignore_ascii_case("fonts")).map(|(_, l)| l.iter().map(|x| x.len()).sum::<usize>()).sum();
        let embedded = match &d.embedded {
            Some((n, idx)) if *n == fonts_len => idx.clone(),
            _ => {
                let idx = Arc::new(fonts::index_embedded(&self.doc.sub, &crate::cache::dir().join("embedded-fonts")));
                d.embedded = Some((fonts_len, idx.clone()));
                idx
            }
        };
        d.reports = fonts::check(&self.doc.sub, &[(Source::Embedded, &embedded), (Source::Attached, &attached), (Source::Folder, &folder), (Source::System, &system)]);
        d.key = Some(key);
        d.last_calc = Some(std::time::Instant::now());
    }

    /// Distinct missing font families (for the status bar).
    /// Missing font families (deduplicated, original spelling).
    pub fn missing_fonts(&mut self) -> Option<Vec<String>> {
        if !matches!(self.doc.format, subforge_core::Format::Ass | subforge_core::Format::Ssa) || self.font_db.get().is_none() {
            return None;
        }
        self.refresh_font_report(false);
        let mut fams: Vec<String> = Vec::new();
        for r in self.dialogs.fonts.reports.iter().filter(|r| r.status == Status::Missing) {
            if !fams.iter().any(|f| f.eq_ignore_ascii_case(&r.usage.family)) {
                fams.push(r.usage.family.clone());
            }
        }
        Some(fams)
    }

    pub fn fonts_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.fonts.open;
        if !open {
            self.dialogs.fonts.open = false;
            self.mux_dialog(ctx);
            return;
        }
        self.refresh_font_report(false);
        let mut select_line: Option<usize> = None;
        let mut collect = false;
        let mut rescan = false;
        let mut open_mux = false;
        let scanning = self.font_db.busy.load(Ordering::Relaxed);
        let prog = self.font_db.progress.load(Ordering::Relaxed) as f32 / 1000.0;
        egui::Window::new("Fonts").open(&mut open).resizable(true).default_size([640.0, 380.0]).show(ctx, |ui| {
            let reports = &self.dialogs.fonts.reports;
            let (ok, fake, miss) = reports.iter().fold((0, 0, 0), |(a, b, c), r| match r.status {
                Status::Ok => (a + 1, b, c),
                Status::Synthesized { .. } => (a, b + 1, c),
                Status::Missing => (a, b, c + 1),
            });
            ui.horizontal(|ui| {
                ui.label(ic_s(icons::ICON_CHECK_CIRCLE).color(Color32::from_rgb(90, 190, 110)));
                ui.label(format!("{ok} found"));
                ui.label(ic_s(icons::ICON_WARNING).color(Color32::from_rgb(225, 160, 50)));
                ui.label(format!("{fake} faked bold/italic"));
                ui.label(ic_s(icons::ICON_ERROR).color(Color32::from_rgb(230, 80, 80)));
                ui.label(format!("{miss} missing"));
                if scanning {
                    ui.spinner();
                    ui.label(RichText::new(format!("indexing system fonts {:.0}%", prog * 100.0)).small().weak());
                }
            });
            ui.label(RichText::new("Searched in order: fonts embedded in the script → fonts attached to the video → your fonts folder → installed fonts.").small().weak());
            ui.separator();
            let avail = ui.available_height() - 70.0;
            egui::ScrollArea::vertical().max_height(avail.max(120.0)).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("font_grid").striped(true).num_columns(5).spacing([10.0, 4.0]).show(ui, |ui| {
                    ui.label("");
                    ui.label(RichText::new("Font").strong());
                    ui.label(RichText::new("Lines").strong());
                    ui.label(RichText::new("Found in").strong());
                    ui.label(RichText::new("File").strong());
                    ui.end_row();
                    for r in reports {
                        let (icon, col, tip) = match &r.status {
                            Status::Ok => (icons::ICON_CHECK_CIRCLE, Color32::from_rgb(90, 190, 110), "Exact face available".to_string()),
                            Status::Synthesized { bold, italic } => (
                                icons::ICON_WARNING,
                                Color32::from_rgb(225, 160, 50),
                                format!("No {} face: the renderer fakes it, so it may look different from the original", match (bold, italic) {
                                    (true, true) => "bold italic",
                                    (true, false) => "bold",
                                    _ => "italic",
                                }),
                            ),
                            Status::Missing => (icons::ICON_ERROR, Color32::from_rgb(230, 80, 80), "Not found anywhere: players will substitute another font".to_string()),
                        };
                        ui.label(ic_s(icon).color(col)).on_hover_text(tip);
                        let mut name = r.usage.family.clone();
                        if r.usage.bold {
                            name += " Bold";
                        }
                        if r.usage.italic {
                            name += " Italic";
                        }
                        ui.label(RichText::new(name).strong()).on_hover_text(format!("styles: {}", r.usage.styles.join(", ")));
                        if ui.link(format!("{}", r.usage.lines.len())).on_hover_text("Go to the first line using it").clicked() {
                            select_line = r.usage.lines.first().copied();
                        }
                        ui.label(r.source.map(|s| s.label()).unwrap_or("—"));
                        let file = r.face.as_ref().map(|f| f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).unwrap_or_default();
                        ui.label(RichText::new(file).small().weak()).on_hover_text(r.face.as_ref().map(|f| format!("{}\n{} {}", f.path.display(), f.family, f.subfamily)).unwrap_or_default());
                        ui.end_row();
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Fonts folder");
                let r = ui.add(egui::TextEdit::singleline(&mut self.settings.fonts_dir).hint_text("optional extra folder").desired_width(220.0));
                if r.lost_focus() {
                    self.dialogs.fonts.key = None;
                }
                if ui.add(btn(icons::ICON_FOLDER_OPEN, "…")).clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.settings.fonts_dir = d.display().to_string();
                        self.dialogs.fonts.key = None;
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.add(btn(icons::ICON_REFRESH, "Rescan")).on_hover_text("Re-index installed fonts and folders").clicked() {
                    rescan = true;
                }
                if ui.add(btn(icons::ICON_DOWNLOAD, "Collect fonts…")).on_hover_text("Copy every font file this script needs into a folder").clicked() {
                    collect = true;
                }
                if ui.add(btn(icons::ICON_MOVIE, "Mux into MKV…")).on_hover_text("Add this subtitle + its fonts to a video file").clicked() {
                    open_mux = true;
                }
            });
        });
        self.dialogs.fonts.open = open;
        if let Some(i) = select_line {
            self.select_only(Some(i));
            self.scroll_to = Some(i);
        }
        if rescan {
            self.font_db.rescan(ctx);
            self.refresh_font_report(true);
        }
        if collect {
            if let Some(dir) = rfd::FileDialog::new().set_directory(self.last_dir()).pick_folder() {
                match fonts::collect(&self.dialogs.fonts.reports, &dir) {
                    Ok(v) => self.flash(format!("Collected {} font file(s) into {}", v.len(), dir.display())),
                    Err(e) => self.flash_err(format!("Collect failed: {e}")),
                }
            }
        }
        if open_mux {
            self.open_mux_dialog();
        }
        self.mux_dialog(ctx);
    }

    pub fn open_mux_dialog(&mut self) {
        let m = &mut self.dialogs.fonts.mux;
        m.open = true;
        let video = self.player.as_ref().map(|p| p.path.clone());
        if m.video.is_none() || video.is_some() {
            m.video = video.or(m.video.take());
        }
        if let Some(v) = &m.video {
            let stem = v.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "video".into());
            m.output = Some(v.with_file_name(format!("{stem}.subbed.mkv")));
        }
        if m.title.is_empty() {
            m.title = self.doc.sub.info_get("Title").unwrap_or("").to_string();
        }
    }

    fn mux_dialog(&mut self, ctx: &egui::Context) {
        if !self.dialogs.fonts.mux.open {
            return;
        }
        let mut open = true;
        let mut go = false;
        egui::Window::new("Mux into MKV").open(&mut open).resizable(true).default_width(520.0).show(ctx, |ui| {
            let m = &mut self.dialogs.fonts.mux;
            egui::Grid::new("mux_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Video");
                ui.horizontal(|ui| {
                    ui.label(m.video.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(choose a video)".into()));
                    if ui.add(btn(icons::ICON_FOLDER_OPEN, "…")).clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("Video", crate::media::VIDEO_EXTS).pick_file() {
                            let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                            m.output = Some(p.with_file_name(format!("{stem}.subbed.mkv")));
                            m.video = Some(p);
                        }
                    }
                });
                ui.end_row();
                ui.label("Output");
                ui.horizontal(|ui| {
                    ui.label(m.output.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
                    if ui.add(btn(icons::ICON_SAVE_AS, "…")).clicked() {
                        let mut d = rfd::FileDialog::new().add_filter("Matroska", &["mkv"]);
                        if let Some(o) = &m.output {
                            if let Some(n) = o.file_name() {
                                d = d.set_file_name(n.to_string_lossy());
                            }
                        }
                        if let Some(p) = d.save_file() {
                            m.output = Some(p.with_extension("mkv"));
                        }
                    }
                });
                ui.end_row();
                ui.label("Language");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("mux_lang").selected_text(LANGS.iter().find(|l| l.0 == m.language).map(|l| l.1).unwrap_or(&m.language)).show_ui(ui, |ui| {
                        for (code, name) in LANGS {
                            ui.selectable_value(&mut m.language, code.to_string(), format!("{name} ({code})"));
                        }
                    });
                    ui.add(egui::TextEdit::singleline(&mut m.language).desired_width(50.0)).on_hover_text("ISO 639-2 code");
                });
                ui.end_row();
                ui.label("Track title");
                ui.add(egui::TextEdit::singleline(&mut m.title).desired_width(260.0).hint_text("e.g. Thai (signs & songs)"));
                ui.end_row();
                ui.label("Options");
                ui.vertical(|ui| {
                    ui.checkbox(&mut m.default, "Make it the default subtitle track");
                    ui.checkbox(&mut m.replace_subs, "Remove the video's existing subtitle tracks");
                    ui.checkbox(&mut m.attach_fonts, "Attach the fonts this script uses");
                });
                ui.end_row();
            });
            let miss = self.dialogs.fonts.reports.iter().filter(|r| r.status == Status::Missing).count();
            if m.attach_fonts && miss > 0 {
                ui.label(RichText::new(format!("{miss} font(s) are missing and can't be attached — see the Fonts window")).small().color(Color32::from_rgb(225, 160, 50)));
            }
            ui.label(RichText::new("Streams are copied (no re-encoding) — takes seconds even for long episodes.").small().weak());
            ui.add_space(4.0);
            let ready = m.video.is_some() && m.output.is_some();
            if ui.add_enabled(ready, btn(icons::ICON_MOVIE, RichText::new("Mux").strong())).clicked() {
                go = true;
            }
        });
        self.dialogs.fonts.mux.open = open;
        if go {
            self.start_mux();
        }
    }

    fn start_mux(&mut self) {
        if !self.ensure_ffmpeg() {
            return;
        }
        self.refresh_font_report(false);
        let m = &self.dialogs.fonts.mux;
        let (Some(video), Some(output)) = (m.video.clone(), m.output.clone()) else { return };
        // current document -> temp file in its own format (ASS keeps everything)
        let fmt = self.doc.format;
        let tmp = crate::cache::dir().join(format!("mux-input.{}", fmt.ext()));
        let _ = std::fs::create_dir_all(crate::cache::dir());
        let mut sub = self.doc.sub.clone();
        subforge_core::prepare_for(&mut sub, fmt);
        if let Err(e) = subforge_core::save(&tmp, &sub, Some(fmt), &Default::default()) {
            self.flash_err(format!("Cannot write subtitles: {e}"));
            return;
        }
        let fonts: Vec<PathBuf> = if m.attach_fonts && matches!(fmt, subforge_core::Format::Ass | subforge_core::Format::Ssa) {
            let mut v: Vec<PathBuf> = self.dialogs.fonts.reports.iter().filter(|r| r.source != Some(Source::Attached)).filter_map(|r| r.face.as_ref().map(|f| f.path.clone())).collect();
            v.sort();
            v.dedup();
            v
        } else {
            Vec::new()
        };
        let opts = subforge_core::extract::MuxOpts {
            video,
            tracks: vec![subforge_core::extract::NewTrack { file: tmp, language: m.language.trim().to_string(), title: m.title.trim().to_string(), default: m.default, forced: false }],
            keep_existing_subs: !m.replace_subs,
            fonts,
            output,
        };
        self.dialogs.fonts.mux.open = false;
        self.loading.queue.push_back(crate::loading::spawn_mux(&self.tools, opts));
    }
}
