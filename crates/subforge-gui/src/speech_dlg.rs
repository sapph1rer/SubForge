//! Speech window: transcribe with whisper.cpp, re-time matching text, or sync any-language
//! subtitles to detected speech. Heavy work runs as a background job with the progress modal.

use crate::app::App;
use crate::icons::{self, btn, ic_s};
use crate::loading::{spawn_job, JobOut};
use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use subforge_core::{audio, speech};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Sync,
    Transcribe,
    Align,
}

pub struct Download {
    pub name: String,
    pub done: Arc<AtomicU64>,
    pub total: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
    pub rx: std::sync::mpsc::Receiver<Result<PathBuf, String>>,
}

pub struct SpeechDlg {
    pub open: bool,
    pub mode: Mode,
    pub max_chars: usize,
    pub max_dur_s: f32,
    pub translate: bool,
    pub sync_whisper: bool,
    pub drift: bool,
    pub snap: bool,
    pub snap_ms: i64,
    pub max_offset_s: i64,
    pub min_ratio: f32,
    pub model_pick: usize,
    pub download: Option<Download>,
}

impl Default for SpeechDlg {
    fn default() -> Self {
        SpeechDlg { open: false, mode: Mode::Sync, max_chars: 42, max_dur_s: 7.0, translate: false, sync_whisper: false, drift: true, snap: true, snap_ms: 250, max_offset_s: 120, min_ratio: 0.3, model_pick: 4, download: None }
    }
}

const LANGS: &[(&str, &str)] = &[("auto", "Auto-detect"), ("ja", "Japanese"), ("th", "Thai"), ("en", "English"), ("ko", "Korean"), ("zh", "Chinese"), ("es", "Spanish"), ("fr", "French"), ("de", "German"), ("vi", "Vietnamese"), ("id", "Indonesian")];

pub fn models_dir() -> PathBuf {
    crate::cache::dir().parent().map(|p| p.join("models")).unwrap_or_else(|| crate::cache::dir().join("models"))
}

fn start_download(name: &str) -> Download {
    let (tx, rx) = std::sync::mpsc::channel();
    let done = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let (d, t, c) = (done.clone(), total.clone(), cancel.clone());
    let url = speech::model_url(name);
    let dest = models_dir().join(format!("ggml-{name}.bin"));
    std::thread::spawn(move || {
        let r = (|| -> Result<PathBuf, String> {
            use std::io::{Read, Write};
            std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
            let mut resp = ureq::get(&url).call().map_err(|e| format!("download failed: {e}"))?;
            if let Some(len) = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()) {
                t.store(len, Ordering::Relaxed);
            }
            let part = dest.with_extension("bin.part");
            let mut f = std::io::BufWriter::new(std::fs::File::create(&part).map_err(|e| e.to_string())?);
            let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                if c.load(Ordering::Relaxed) {
                    drop(f);
                    let _ = std::fs::remove_file(&part);
                    return Err("cancelled".into());
                }
                let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                d.fetch_add(n as u64, Ordering::Relaxed);
            }
            f.flush().map_err(|e| e.to_string())?;
            drop(f);
            std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
            Ok(dest)
        })();
        let _ = tx.send(r);
    });
    Download { name: name.to_string(), done, total, cancel, rx }
}

fn path_field(ui: &mut egui::Ui, s: &mut String, hint: &str, pick: impl FnOnce() -> Option<PathBuf>) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(s).hint_text(hint).desired_width(300.0));
        if ui.add(crate::icons::ibtn(icons::ICON_FOLDER_OPEN)).on_hover_text("Browse…").clicked() {
            if let Some(p) = pick() {
                *s = p.display().to_string();
            }
        }
    });
}

impl App {
    pub fn whisper_opts(&self) -> speech::WhisperOpts {
        let exe = if self.settings.whisper_exe.trim().is_empty() {
            let mut hints = vec![PathBuf::from(&self.settings.ffmpeg_dir)];
            if let Ok(e) = std::env::current_exe() {
                if let Some(d) = e.parent() {
                    hints.push(d.to_path_buf());
                    hints.push(d.join("whisper"));
                }
            }
            speech::locate_whisper(&hints).unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "whisper-cli.exe" } else { "whisper-cli" }))
        } else {
            PathBuf::from(self.settings.whisper_exe.trim())
        };
        speech::WhisperOpts {
            exe,
            model: PathBuf::from(self.settings.whisper_model.trim()),
            language: self.settings.whisper_lang.clone(),
            translate: self.dialogs.speech.translate,
            threads: self.settings.whisper_threads,
            prompt: self.settings.whisper_prompt.clone(),
            extra: self.settings.whisper_extra.split_whitespace().map(String::from).collect(),
        }
    }

    pub fn speech_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.dialogs.speech.open;
        if !open {
            return;
        }
        // download progress
        if let Some(d) = &self.dialogs.speech.download {
            match d.rx.try_recv() {
                Ok(Ok(p)) => {
                    self.settings.whisper_model = p.display().to_string();
                    self.flash(format!("Downloaded model {}", p.display()));
                    self.dialogs.speech.download = None;
                }
                Ok(Err(e)) => {
                    if e != "cancelled" {
                        self.flash_err(e);
                    }
                    self.dialogs.speech.download = None;
                }
                Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
            }
        }
        let mut run = false;
        let has_media = self.player.as_ref().is_some_and(|p| p.info.has_audio);
        egui::Window::new("Speech — transcribe / sync").open(&mut open).resizable(true).default_width(520.0).show(ctx, |ui| {
            let d = &mut self.dialogs.speech;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut d.mode, Mode::Sync, (ic_s(icons::ICON_SYNC), "Sync to speech"));
                ui.selectable_value(&mut d.mode, Mode::Transcribe, (ic_s(icons::ICON_RECORD_VOICE_OVER), "Transcribe"));
                ui.selectable_value(&mut d.mode, Mode::Align, (ic_s(icons::ICON_PLAYLIST_ADD_CHECK), "Re-time text"));
            });
            ui.separator();
            let needs_whisper = match d.mode {
                Mode::Sync => d.sync_whisper,
                _ => true,
            };
            match d.mode {
                Mode::Sync => {
                    ui.label(RichText::new("Fixes offset and frame-rate drift of subtitles in any language (e.g. Thai subs over Japanese audio) by matching lines to detected speech.").weak());
                    ui.add_space(4.0);
                    egui::Grid::new("sync_g").num_columns(2).show(ui, |ui| {
                        ui.label("Speech detection");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut d.sync_whisper, false, "Built-in (fast)");
                            ui.radio_value(&mut d.sync_whisper, true, "Whisper (better with music)");
                        });
                        ui.end_row();
                        ui.label("Search range");
                        ui.add(egui::DragValue::new(&mut d.max_offset_s).range(5..=600).suffix(" s ±"));
                        ui.end_row();
                        ui.label("Frame-rate drift");
                        ui.checkbox(&mut d.drift, "detect (23.976 ↔ 25 fps, …)");
                        ui.end_row();
                        ui.label("Snap edges");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut d.snap, "to speech within");
                            ui.add_enabled(d.snap, egui::DragValue::new(&mut d.snap_ms).range(50..=1000).suffix(" ms"));
                        });
                        ui.end_row();
                    });
                }
                Mode::Transcribe => {
                    ui.label(RichText::new("Creates subtitles from the video's audio with whisper.cpp (runs on this computer).").weak());
                    ui.add_space(4.0);
                    egui::Grid::new("tr_g").num_columns(2).show(ui, |ui| {
                        ui.label("Line length");
                        ui.add(egui::DragValue::new(&mut d.max_chars).range(16..=80).suffix(" chars"));
                        ui.end_row();
                        ui.label("Max duration");
                        ui.add(egui::DragValue::new(&mut d.max_dur_s).range(2.0..=15.0).speed(0.1).suffix(" s"));
                        ui.end_row();
                        ui.label("Output");
                        ui.checkbox(&mut d.translate, "translate to English");
                        ui.end_row();
                    });
                }
                Mode::Align => {
                    ui.label(RichText::new("Re-times lines whose text is in the spoken language (e.g. a Japanese script over Japanese audio): each line is matched to the transcript character by character.").weak());
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Minimum match");
                        ui.add(egui::Slider::new(&mut d.min_ratio, 0.1..=0.9).max_decimals(2));
                    });
                }
            }
            if needs_whisper {
                ui.add_space(6.0);
                egui::CollapsingHeader::new(RichText::new("whisper.cpp").strong()).default_open(self.settings.whisper_model.is_empty()).show(ui, |ui| {
                    egui::Grid::new("wh_g").num_columns(2).show(ui, |ui| {
                        ui.label("Language");
                        egui::ComboBox::from_id_salt("wh_lang").selected_text(LANGS.iter().find(|l| l.0 == self.settings.whisper_lang).map(|l| l.1).unwrap_or(&self.settings.whisper_lang)).show_ui(ui, |ui| {
                            for (code, name) in LANGS {
                                ui.selectable_value(&mut self.settings.whisper_lang, code.to_string(), format!("{name} ({code})"));
                            }
                        });
                        ui.end_row();
                        ui.label("whisper-cli");
                        path_field(ui, &mut self.settings.whisper_exe, "auto: next to SubForge / ffmpeg or on PATH", || rfd::FileDialog::new().pick_file());
                        ui.end_row();
                        ui.label("Model");
                        path_field(ui, &mut self.settings.whisper_model, "ggml-*.bin", || rfd::FileDialog::new().add_filter("ggml model", &["bin"]).set_directory(models_dir()).pick_file());
                        ui.end_row();
                        ui.label("");
                        ui.horizontal(|ui| {
                            let d = &mut self.dialogs.speech;
                            egui::ComboBox::from_id_salt("wh_model").width(260.0).selected_text(format!("{} — {}", speech::MODELS[d.model_pick].0, speech::MODELS[d.model_pick].1)).show_ui(ui, |ui| {
                                for (i, (n, desc)) in speech::MODELS.iter().enumerate() {
                                    ui.selectable_value(&mut d.model_pick, i, format!("{n} — {desc}"));
                                }
                            });
                            match &d.download {
                                None => {
                                    if ui.add(btn(icons::ICON_CLOUD_DOWNLOAD, "Download")).on_hover_text(format!("from {}\ninto {}", speech::model_url(speech::MODELS[d.model_pick].0), models_dir().display())).clicked() {
                                        d.download = Some(start_download(speech::MODELS[d.model_pick].0));
                                    }
                                }
                                Some(dl) => {
                                    let (a, b) = (dl.done.load(Ordering::Relaxed), dl.total.load(Ordering::Relaxed));
                                    let f = if b > 0 { a as f32 / b as f32 } else { 0.0 };
                                    ui.add(egui::ProgressBar::new(f).desired_width(120.0).text(format!("{} {:.0} MB", dl.name, a as f64 / 1e6)));
                                    if ui.add(crate::icons::ibtn(icons::ICON_CLOSE)).on_hover_text("Cancel").clicked() {
                                        dl.cancel.store(true, Ordering::Relaxed);
                                    }
                                }
                            }
                        });
                        ui.end_row();
                        ui.label("Prompt");
                        ui.add(egui::TextEdit::singleline(&mut self.settings.whisper_prompt).hint_text("names / terms (optional)").desired_width(300.0));
                        ui.end_row();
                        ui.label("Threads");
                        ui.add(egui::DragValue::new(&mut self.settings.whisper_threads).range(0..=64)).on_hover_text("0 = whisper default");
                        ui.end_row();
                        ui.label("Extra args");
                        ui.add(egui::TextEdit::singleline(&mut self.settings.whisper_extra).hint_text("e.g. -bs 5 -mc 0").desired_width(300.0));
                        ui.end_row();
                    });
                    ui.label(RichText::new("Get whisper-cli from the whisper.cpp releases (CPU, CUDA or Vulkan builds) and put it next to SubForge or ffmpeg.").small().weak());
                });
            }
            ui.add_space(6.0);
            if !has_media {
                ui.label(RichText::new("Open the video (with audio) first.").color(Color32::from_rgb(225, 160, 50)));
            }
            let ready = has_media && (!needs_whisper || std::path::Path::new(self.settings.whisper_model.trim()).is_file());
            if needs_whisper && !std::path::Path::new(self.settings.whisper_model.trim()).is_file() {
                ui.label(RichText::new("Choose or download a whisper model.").color(Color32::from_rgb(225, 160, 50)));
            }
            let label = match self.dialogs.speech.mode {
                Mode::Sync => "Sync",
                Mode::Transcribe => "Transcribe",
                Mode::Align => "Re-time",
            };
            if ui.add_enabled(ready, btn(icons::ICON_PLAY_ARROW, label)).clicked() {
                run = true;
            }
        });
        self.dialogs.speech.open = open;
        if run {
            self.start_speech_job();
        }
    }

    fn start_speech_job(&mut self) {
        let Some(p) = &self.player else { return };
        let media = p.path.clone();
        let ffmpeg = p.ffmpeg();
        let duration = p.info.duration_ms;
        let spec = p.spectrum_data();
        let d = &self.dialogs.speech;
        let mode = d.mode;
        let wopts = self.whisper_opts();
        let use_whisper = mode != Mode::Sync || d.sync_whisper;
        let seg = speech::SegmentOpts { max_chars: d.max_chars, max_duration_ms: (d.max_dur_s * 1000.0) as i64, ..Default::default() };
        let sync = speech::SyncOpts { max_offset_ms: d.max_offset_s * 1000, drift: d.drift, snap_ms: if d.snap { d.snap_ms } else { 0 }, ..Default::default() };
        let min_ratio = d.min_ratio as f64;
        let sub = self.doc.sub.clone();
        let suggested = media.with_extension(if wopts.translate { "en.srt" } else { "srt" });
        let suggested = if suggested.exists() { media.with_extension("whisper.srt") } else { suggested };
        let name = media.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let title = match mode {
            Mode::Sync => "Sync to speech",
            Mode::Transcribe => "Transcribing",
            Mode::Align => "Re-timing to speech",
        };
        self.dialogs.speech.open = false;
        self.loading.queue.push_back(spawn_job(title, name, move |jc| {
            let transcript = |jc: &crate::loading::JobCtx| -> Result<speech::Transcript, String> {
                let work = std::env::temp_dir().join(format!("subforge-whisper-{}", std::process::id()));
                std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
                let wav = work.join("audio16k.wav");
                jc.set("Extracting audio (16 kHz)", 0.0);
                speech::extract_wav(&ffmpeg, &media, 0, &wav, duration, |f| {
                    jc.set("Extracting audio (16 kHz)", f);
                    !jc.cancelled()
                })?;
                jc.set("whisper: loading model", -1.0);
                let r = speech::run_whisper(
                    &wopts,
                    &wav,
                    &work,
                    |f| {
                        jc.set("whisper: recognising speech", f);
                        !jc.cancelled()
                    },
                    |_| {},
                );
                let _ = std::fs::remove_dir_all(&work);
                r
            };
            match mode {
                Mode::Transcribe => {
                    let tr = transcript(jc)?;
                    jc.set("Building lines", -1.0);
                    let out = speech::transcript_to_subtitle(&tr, seg);
                    Ok(JobOut::NewDoc { sub: out, suggested: Some(suggested), msg: format!("Transcribed ({})", tr.language) })
                }
                Mode::Align => {
                    let tr = transcript(jc)?;
                    let mut s = sub;
                    let r = speech::align_text(&mut s, &tr, min_ratio, 84);
                    Ok(JobOut::Replace { sub: s, msg: format!("Re-timed {} of {} lines from speech, {} interpolated", r.matched, r.total, r.interpolated.len()) })
                }
                Mode::Sync => {
                    let speech_iv = if use_whisper {
                        transcript(jc)?.speech()
                    } else {
                        let data = match spec {
                            Some(d) => d,
                            None => {
                                let mut b = audio::SpecBuilder::new();
                                audio::decode_16k(&ffmpeg, &media, 0, |s| {
                                    b.push(s);
                                    let ms = b.columns() as i64 * audio::COL_MS;
                                    jc.set("Analysing audio", if duration > 0 { (ms as f32 / duration as f32).min(1.0) } else { -1.0 });
                                    !jc.cancelled()
                                })?;
                                b.finish();
                                b.data
                            }
                        };
                        jc.set("Detecting speech", -1.0);
                        audio::vad(&data, audio::VadOpts::default())
                    };
                    jc.set("Matching lines to speech", -1.0);
                    let mut s = sub;
                    let mut rep = speech::estimate_sync(&s, &speech_iv, &sync);
                    speech::apply_sync(&mut s, &speech_iv, &mut rep, &sync);
                    let drift = if (rep.rate - 1.0).abs() > 1e-9 { format!(", rate ×{:.5}", rep.rate) } else { String::new() };
                    let warn = if rep.confidence < 0.15 { " — LOW confidence, check it" } else { "" };
                    Ok(JobOut::Replace {
                        sub: s,
                        msg: format!("Synced: offset {:+.3}s{drift}, confidence {:.2}, snapped {}/{} edges{warn}", rep.offset_ms as f64 / 1000.0, rep.confidence, rep.snapped_starts, rep.snapped_ends),
                    })
                }
            }
        }));
    }
}
