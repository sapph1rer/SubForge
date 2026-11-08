//! OCR of image subtitles: settings → background job → review window (picture next to the
//! recognised text, low-confidence lines highlighted, replacement list) → new document.

use crate::app::App;
use crate::icons::{self, btn, ibtn};
use crate::loading::{spawn_job, JobOut};
use eframe::egui::{self, Color32, RichText};
use std::collections::HashMap;
use std::path::PathBuf;
use subforge_core::extract::Track;
use subforge_core::ocr::{self, BitmapTrack, OcrLine};

#[derive(Default)]
pub struct OcrDlg {
    pub open: bool,
    pub video: PathBuf,
    pub track: Option<Track>,
    pub open_video: bool,
    pub langs: Vec<String>,
    pub langs_err: Option<String>,
}

pub struct OcrReview {
    pub video: PathBuf,
    pub open_video: bool,
    pub track: BitmapTrack,
    pub lines: Vec<OcrLine>,
    pub only_low: bool,
    pub textures: HashMap<usize, egui::TextureHandle>,
    pub new_from: String,
    pub new_to: String,
    pub threshold: f32,
}

impl App {
    fn tesseract_path(&self) -> PathBuf {
        if !self.settings.tesseract_exe.trim().is_empty() {
            return PathBuf::from(self.settings.tesseract_exe.trim());
        }
        let mut hints = vec![PathBuf::from(&self.settings.ffmpeg_dir)];
        if let Ok(e) = std::env::current_exe() {
            if let Some(d) = e.parent() {
                hints.push(d.to_path_buf());
            }
        }
        ocr::locate_tesseract(&hints).unwrap_or_else(|| PathBuf::from("tesseract"))
    }

    pub fn open_ocr_dialog(&mut self, video: PathBuf, track: Track, open_video: bool) {
        let tess = self.tesseract_path();
        let td = (!self.settings.tessdata_dir.trim().is_empty()).then(|| PathBuf::from(self.settings.tessdata_dir.trim()));
        let (langs, err) = match ocr::tesseract_languages(&tess, td.as_deref()) {
            Ok(l) => (l, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        self.dialogs.ocr = OcrDlg { open: true, video, track: Some(track), open_video, langs, langs_err: err };
    }

    /// Tools ▸ OCR: pick a file, then the usual track picker (image tracks lead here).
    pub fn ocr_from_file(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Video / image subtitles", &["mkv", "mp4", "m2ts", "ts", "sup", "idx", "vob", "avi"]).set_directory(self.last_dir()).pick_file() {
            self.start_import_from_video(&p, false);
        }
    }

    pub fn ocr_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.ocr.open;
        if open {
            let mut run = false;
            egui::Window::new("OCR image subtitles").open(&mut open).resizable(true).default_width(480.0).show(ctx, |ui| {
                let d = &mut self.dialogs.ocr;
                if let Some(t) = &d.track {
                    ui.label(RichText::new(format!("{} — {}", d.video.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), t.label())).strong());
                }
                ui.add_space(4.0);
                egui::Grid::new("ocr_g").num_columns(2).show(ui, |ui| {
                    ui.label("Languages");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.settings.ocr_lang).desired_width(140.0)).on_hover_text("Tesseract language codes joined with +, e.g. tha+eng, jpn, eng");
                        if !d.langs.is_empty() {
                            egui::ComboBox::from_id_salt("ocr_lang_add").selected_text("add…").show_ui(ui, |ui| {
                                for l in &d.langs {
                                    if ui.selectable_label(false, l).clicked() && !self.settings.ocr_lang.split('+').any(|x| x == l) {
                                        if !self.settings.ocr_lang.is_empty() {
                                            self.settings.ocr_lang.push('+');
                                        }
                                        self.settings.ocr_lang.push_str(l);
                                    }
                                }
                            });
                        }
                    });
                    ui.end_row();
                    ui.label("Tesseract");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.settings.tesseract_exe).hint_text("auto (Program Files\\Tesseract-OCR or PATH)").desired_width(260.0));
                        if ui.add(ibtn(icons::ICON_FOLDER_OPEN)).clicked() {
                            if let Some(p) = rfd::FileDialog::new().pick_file() {
                                self.settings.tesseract_exe = p.display().to_string();
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("tessdata");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.tessdata_dir).hint_text("optional folder with *.traineddata").desired_width(260.0));
                    ui.end_row();
                    ui.label("Thai repair");
                    ui.checkbox(&mut self.settings.ocr_dict_fix, "repair lost tone marks / สระอำ with the dictionary");
                    ui.end_row();
                    ui.label("Replace list");
                    ui.label(format!("{} pair(s) — edit in the review window", self.settings.ocr_replace.len()));
                    ui.end_row();
                });
                if let Some(e) = &d.langs_err {
                    ui.colored_label(Color32::from_rgb(225, 120, 80), format!("Tesseract not found: {e}"));
                    ui.label(RichText::new("Install it from github.com/UB-Mannheim/tesseract (tick Thai under additional languages) or set its path above.").small().weak());
                } else if !self.settings.ocr_lang.split('+').all(|l| d.langs.iter().any(|x| x == l)) {
                    ui.colored_label(Color32::from_rgb(225, 160, 50), format!("Installed languages: {}", d.langs.join(", ")));
                }
                ui.add_space(6.0);
                if ui.add(btn(icons::ICON_DOCUMENT_SCANNER, "Start OCR")).clicked() {
                    run = true;
                }
            });
            self.dialogs.ocr.open = open && !run;
            if run {
                self.start_ocr_job();
            }
        }
        self.ocr_review_window(ctx);
    }

    fn start_ocr_job(&mut self) {
        let d = &self.dialogs.ocr;
        let Some(track) = d.track.clone() else { return };
        let video = d.video.clone();
        let open_video = d.open_video;
        let ffprobe = self.tools.ffprobe.clone();
        let opts = ocr::OcrOpts {
            tesseract: self.tesseract_path(),
            lang: self.settings.ocr_lang.clone(),
            tessdata: (!self.settings.tessdata_dir.trim().is_empty()).then(|| PathBuf::from(self.settings.tessdata_dir.trim())),
            replace: self.settings.ocr_replace.clone(),
            ..Default::default()
        };
        let dict_fix = self.settings.ocr_dict_fix && opts.lang.contains("tha");
        let name = video.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.loading.queue.push_back(spawn_job("OCR", format!("{name} — {}", track.label()), move |jc| {
            jc.set("Reading subtitle pictures", -1.0);
            let bt = ocr::read_track(&ffprobe, &video, track.sub_index)?;
            if bt.items.is_empty() {
                return Err("the track has no pictures".into());
            }
            let speller = if dict_fix { subforge_core::spell::SpellChecker::builtin(false, true).ok() } else { None };
            let work = std::env::temp_dir().join(format!("subforge-ocr-{}", std::process::id()));
            let n = bt.items.len();
            jc.set(&format!("Recognising text (0/{n})"), 0.0);
            let lines = ocr::ocr(&bt, &opts, &work, speller.as_ref(), &|a, b| {
                jc.set(&format!("Recognising text ({a}/{b})"), a as f32 / b.max(1) as f32);
                !jc.cancelled()
            });
            let _ = std::fs::remove_dir_all(&work);
            let lines = lines?;
            Ok(JobOut::OcrReview(Box::new(OcrReview { video, open_video, track: bt, lines, only_low: false, textures: HashMap::new(), new_from: String::new(), new_to: String::new(), threshold: 70.0 })))
        }));
    }

    fn ocr_review_window(&mut self, ctx: &egui::Context) {
        let Some(mut r) = self.ocr_review.take() else { return };
        let mut open = true;
        let mut accept = false;
        let mut apply_replace = false;
        egui::Window::new("OCR review").open(&mut open).resizable(true).default_size([900.0, 620.0]).show(ctx, |ui| {
            let low = r.lines.iter().filter(|l| l.confidence < r.threshold || l.text.is_empty()).count();
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{} lines", r.lines.len()));
                ui.label(RichText::new(format!("{low} to check")).color(if low > 0 { Color32::from_rgb(225, 160, 50) } else { Color32::from_rgb(90, 190, 110) }));
                ui.separator();
                ui.checkbox(&mut r.only_low, "show only lines to check");
                ui.label("below");
                ui.add(egui::DragValue::new(&mut r.threshold).range(0.0..=100.0).suffix(" %"));
                ui.separator();
                if ui.add(btn(icons::ICON_DONE_ALL, "Accept → new document")).clicked() {
                    accept = true;
                }
            });
            egui::CollapsingHeader::new("Replace list (applied to every OCR run)").show(ui, |ui| {
                let mut del = None;
                for (i, (a, b)) in self.settings.ocr_replace.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.monospace(format!("{a}  →  {b}"));
                        if ui.add(ibtn(icons::ICON_DELETE)).clicked() {
                            del = Some(i);
                        }
                    });
                }
                if let Some(i) = del {
                    self.settings.ocr_replace.remove(i);
                }
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut r.new_from).hint_text("wrong").desired_width(120.0));
                    ui.label("→");
                    ui.add(egui::TextEdit::singleline(&mut r.new_to).hint_text("right").desired_width(120.0));
                    if ui.add_enabled(!r.new_from.is_empty(), btn(icons::ICON_ADD, "Add")).clicked() {
                        self.settings.ocr_replace.push((std::mem::take(&mut r.new_from), std::mem::take(&mut r.new_to)));
                    }
                    if ui.add(btn(icons::ICON_FIND_REPLACE, "Apply to these lines")).clicked() {
                        apply_replace = true;
                    }
                });
            });
            ui.separator();
            let rows: Vec<usize> = (0..r.lines.len()).filter(|&i| !r.only_low || r.lines[i].confidence < r.threshold || r.lines[i].text.is_empty()).collect();
            let row_h = 92.0;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, rows.len(), |ui, range| {
                for &i in &rows[range] {
                    let line = &mut r.lines[i];
                    let b = &r.track.items[line.image];
                    let tex = r.textures.entry(line.image).or_insert_with(|| {
                        // composite over a neutral grey so white text with black outline is visible
                        let rgba = b.rgba();
                        let mut px = Vec::with_capacity(b.w * b.h);
                        for c in rgba.chunks_exact(4) {
                            let a = c[3] as u32;
                            let mix = |v: u8| ((v as u32 * a + 70 * (255 - a)) / 255) as u8;
                            px.push(Color32::from_rgb(mix(c[0]), mix(c[1]), mix(c[2])));
                        }
                        ctx.load_texture(format!("ocr{}", line.image), egui::ColorImage::new([b.w.max(1), b.h.max(1)], px), egui::TextureOptions::LINEAR)
                    });
                    ui.horizontal(|ui| {
                        ui.set_min_height(row_h - 6.0);
                        let bad = line.confidence < r.threshold || line.text.is_empty();
                        ui.vertical(|ui| {
                            ui.set_width(118.0);
                            ui.label(RichText::new(format!("#{}", i + 1)).strong().color(if bad { Color32::from_rgb(225, 160, 50) } else { ui.visuals().text_color() }));
                            ui.label(RichText::new(subforge_core::time::fmt_display(line.start)).monospace().small());
                            ui.label(RichText::new(format!("{:.0}%{}", line.confidence, if line.top { " · top" } else { "" })).small().weak());
                        });
                        let max_w = (ui.available_width() * 0.5).max(160.0);
                        let scale = (80.0 / b.h as f32).min(max_w / b.w as f32).min(1.0);
                        ui.image((tex.id(), egui::vec2(b.w as f32 * scale, b.h as f32 * scale)));
                        let mut t = line.text.replace("\\N", "\n");
                        if ui.add(egui::TextEdit::multiline(&mut t).desired_rows(2).desired_width(f32::INFINITY).font(egui::FontId::proportional(17.0))).changed() {
                            line.text = t.replace('\n', "\\N");
                            line.confidence = 100.0;
                        }
                    });
                    ui.separator();
                }
            });
        });
        if apply_replace {
            for l in r.lines.iter_mut() {
                l.text = ocr::cleanup(&l.text, &self.settings.ocr_replace);
            }
        }
        if accept {
            let sub = ocr::to_subtitle(&r.lines);
            let suggested = r.video.with_extension(if r.video.extension().is_some_and(|e| e == "sup" || e == "idx") { "srt" } else { "ocr.srt" });
            self.open_generated(sub, Some(suggested), "OCR".into());
            if r.open_video && self.player.as_ref().is_none_or(|p| p.path != r.video) && subforge_core::Format::from_path(&r.video).is_none() && crate::media::is_media_path(&r.video) {
                self.start_open_video(&r.video.clone());
            }
            return;
        }
        if open {
            self.ocr_review = Some(r);
        }
    }
}
