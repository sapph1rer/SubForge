//! Background import pipeline + loading UI.
//!
//! Everything slow that happens when a file is brought into the program (parsing a subtitle,
//! probing a video, extracting an embedded subtitle track, waiting for the first video frame)
//! runs off the UI thread and is shown in a modal "Loading" window with per-step status, so the
//! program never looks frozen and is only usable once the file is really ready. Slow *optional*
//! work (waveform, keyframes) continues in the background with a progress chip in the status bar.

use crate::app::{App, Pending};
use crate::icons;
use crate::media::{self, MediaInfo, Player};
use crate::subextract::{self, SubTrack};
use eframe::egui::{self, Color32, RichText};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Show the loading window only if a job takes longer than this (avoids flicker for tiny files).
const SHOW_AFTER: Duration = Duration::from_millis(150);
/// Give up waiting for the first video frame after this long.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum StepState {
    Pending,
    Running(Option<f32>),
    Done,
    Failed(String),
}

pub struct Step {
    pub label: String,
    pub state: StepState,
}

pub enum Outcome {
    Subtitle { path: Option<PathBuf>, loaded: subforge_core::Loaded, hint: Option<String> },
    Video { path: PathBuf, info: MediaInfo, tracks: Vec<SubTrack> },
    Tracks { video: PathBuf, duration_ms: i64, tracks: Vec<SubTrack> },
    Failed(String),
    Cancelled,
}

enum Msg {
    Step(usize, StepState),
    Done(Outcome),
}

/// Handle given to the worker thread.
pub struct Reporter {
    tx: Sender<Msg>,
    ctx: egui::Context,
    pub cancel: Arc<AtomicBool>,
}

impl Reporter {
    fn step(&self, i: usize, st: StepState) {
        let _ = self.tx.send(Msg::Step(i, st));
        self.ctx.request_repaint();
    }
    pub fn run(&self, i: usize) {
        self.step(i, StepState::Running(None));
    }
    pub fn progress(&self, i: usize, p: f32) {
        self.step(i, StepState::Running(Some(p)));
    }
    pub fn done(&self, i: usize) {
        self.step(i, StepState::Done);
    }
    #[allow(dead_code)]
    pub fn fail(&self, i: usize, e: String) {
        self.step(i, StepState::Failed(e));
    }
}

pub struct Job {
    pub title: String,
    pub steps: Vec<Step>,
    started: Instant,
    cancel: Arc<AtomicBool>,
    rx: Option<Receiver<Msg>>,
    cancellable: bool,
    /// Set once the player exists and we are waiting for its first frame.
    wait_frame_since: Option<Instant>,
    /// Embedded tracks found in the video being opened (offered once the video is ready).
    video_tracks: Option<(PathBuf, i64, Vec<SubTrack>)>,
}

impl Job {
    fn visible(&self) -> bool {
        self.wait_frame_since.is_some() || self.started.elapsed() >= SHOW_AFTER
    }
    fn fraction(&self) -> f32 {
        let n = self.steps.len().max(1) as f32;
        let mut done = 0.0;
        for s in &self.steps {
            done += match &s.state {
                StepState::Done => 1.0,
                StepState::Running(Some(p)) => *p,
                StepState::Running(None) => 0.35,
                _ => 0.0,
            };
        }
        (done / n).clamp(0.0, 1.0)
    }
}

fn spawn_job(ctx: &egui::Context, title: &str, steps: &[&str], cancellable: bool, f: impl FnOnce(&Reporter) -> Outcome + Send + 'static) -> Job {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let rep = Reporter { tx, ctx: ctx.clone(), cancel: cancel.clone() };
    std::thread::Builder::new()
        .name("import".into())
        .spawn(move || {
            let o = f(&rep);
            let _ = rep.tx.send(Msg::Done(o));
            rep.ctx.request_repaint();
        })
        .expect("spawn import thread");
    Job {
        title: title.to_string(),
        steps: steps.iter().map(|s| Step { label: s.to_string(), state: StepState::Pending }).collect(),
        started: Instant::now(),
        cancel,
        rx: Some(rx),
        cancellable,
        wait_frame_since: None,
        video_tracks: None,
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

pub fn language_name(code: &str) -> String {
    let c = code.trim().to_ascii_lowercase();
    let name = match c.as_str() {
        "" | "und" => "Unknown",
        "tha" | "th" => "Thai",
        "eng" | "en" => "English",
        "jpn" | "ja" => "Japanese",
        "kor" | "ko" => "Korean",
        "chi" | "zho" | "zh" => "Chinese",
        "fre" | "fra" | "fr" => "French",
        "ger" | "deu" | "de" => "German",
        "spa" | "es" => "Spanish",
        "ita" | "it" => "Italian",
        "rus" | "ru" => "Russian",
        "por" | "pt" => "Portuguese",
        "ind" | "id" => "Indonesian",
        "vie" | "vi" => "Vietnamese",
        "ara" | "ar" => "Arabic",
        "hin" | "hi" => "Hindi",
        "may" | "msa" | "ms" => "Malay",
        _ => return code.to_string(),
    };
    name.to_string()
}

// ------------------------------------------------------------------ import dialog state

#[derive(Default)]
pub struct ImportDlg {
    pub open: bool,
    pub video: PathBuf,
    pub duration_ms: i64,
    pub tracks: Vec<SubTrack>,
    pub sel: usize,
    pub with_video: bool,
}

impl App {
    // ------------------------------------------------------------ starting jobs
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn start_open_subtitle(&mut self, p: PathBuf, ctx: &egui::Context) {
        if self.busy() {
            self.flash_err("Still loading another file…");
            return;
        }
        let title = format!("Opening {}", file_name(&p));
        self.job = Some(spawn_job(ctx, &title, &["Reading & parsing subtitle file"], false, move |r| {
            r.run(0);
            match subforge_core::load(&p, None) {
                Ok(loaded) => {
                    r.done(0);
                    Outcome::Subtitle { path: Some(p), loaded, hint: None }
                }
                Err(e) => Outcome::Failed(format!("Open failed: {e}")),
            }
        }));
    }

    pub fn start_open_video(&mut self, p: PathBuf, ctx: &egui::Context) {
        if self.busy() {
            self.queued_video = Some(p);
            return;
        }
        if !self.ffmpeg_ok {
            self.tools = media::Tools::locate(&self.settings.ffmpeg_dir);
            self.ffmpeg_ok = self.tools.available();
            self.has_libass = self.ffmpeg_ok && self.tools.has_libass();
            if !self.ffmpeg_ok {
                self.flash_err("ffmpeg/ffprobe not found. Install ffmpeg or set its folder in Settings.");
                return;
            }
        }
        let tools = self.tools.clone();
        let title = format!("Opening {}", file_name(&p));
        self.job = Some(spawn_job(ctx, &title, &["Reading media info", "Looking for embedded subtitles", "Starting video & audio"], true, move |r| {
            r.run(0);
            let info = match media::probe(&tools, &p) {
                Ok(i) => i,
                Err(e) => return Outcome::Failed(format!("Cannot open video: {e}")),
            };
            r.done(0);
            if r.cancel.load(Ordering::Relaxed) {
                return Outcome::Cancelled;
            }
            r.run(1);
            let tracks = subextract::list_tracks(&tools, &p).unwrap_or_default();
            r.done(1);
            Outcome::Video { path: p, info, tracks }
        }));
    }

    /// List embedded subtitle tracks of `p` and show the import dialog.
    pub fn start_probe_tracks(&mut self, p: PathBuf, ctx: &egui::Context) {
        if self.busy() {
            self.flash_err("Still loading another file…");
            return;
        }
        if !self.ffmpeg_ok {
            self.tools = media::Tools::locate(&self.settings.ffmpeg_dir);
            self.ffmpeg_ok = self.tools.available();
            if !self.ffmpeg_ok {
                self.flash_err("ffmpeg/ffprobe not found — it is needed to read subtitles from video files. Set its folder in Settings.");
                return;
            }
        }
        let tools = self.tools.clone();
        let title = format!("Reading {}", file_name(&p));
        self.job = Some(spawn_job(ctx, &title, &["Reading container & subtitle tracks"], true, move |r| {
            r.run(0);
            let duration_ms = media::probe(&tools, &p).map(|i| i.duration_ms).unwrap_or(0);
            match subextract::list_tracks(&tools, &p) {
                Ok(tracks) => {
                    r.done(0);
                    Outcome::Tracks { video: p, duration_ms, tracks }
                }
                Err(e) => Outcome::Failed(format!("Cannot read file: {e}")),
            }
        }));
    }

    pub fn start_extract(&mut self, video: PathBuf, track: SubTrack, duration_ms: i64, with_video: bool, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let tools = self.tools.clone();
        let lang = language_name(&track.lang);
        let title = format!("Importing {} subtitles from {}", lang, file_name(&video));
        let hint = {
            let stem = video.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "subtitle".into());
            if track.lang.is_empty() { stem } else { format!("{stem}.{}", track.lang) }
        };
        if with_video && self.player.as_ref().map(|p| p.path != std::path::absolute(&video).unwrap_or_else(|_| video.clone())).unwrap_or(true) {
            self.queued_video = Some(video.clone());
        }
        self.job = Some(spawn_job(ctx, &title, &["Extracting subtitle track", "Parsing subtitles"], true, move |r| {
            r.run(0);
            let out = match subextract::extract(&tools, &video, &track, duration_ms, &r.cancel, &|p| r.progress(0, p)) {
                Ok(o) => o,
                Err(e) if e.to_string() == "cancelled" => return Outcome::Cancelled,
                Err(e) => return Outcome::Failed(format!("Import failed: {e}")),
            };
            r.done(0);
            r.run(1);
            let res = subforge_core::load(&out, None);
            let _ = std::fs::remove_file(&out);
            match res {
                Ok(loaded) => {
                    r.done(1);
                    Outcome::Subtitle { path: None, loaded, hint: Some(hint) }
                }
                Err(e) => Outcome::Failed(format!("Extracted track could not be parsed: {e}")),
            }
        }));
    }

    // ------------------------------------------------------------ per-frame polling
    pub fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(mut job) = self.job.take() else {
            if let Some(v) = self.queued_video.take() {
                self.start_open_video(v, ctx);
            }
            return;
        };
        let mut finished: Option<Outcome> = None;
        if let Some(rx) = &job.rx {
            loop {
                match rx.try_recv() {
                    Ok(Msg::Step(i, st)) => {
                        if let Some(s) = job.steps.get_mut(i) {
                            s.state = st;
                        }
                    }
                    Ok(Msg::Done(o)) => finished = Some(o),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if finished.is_none() {
                            finished = Some(Outcome::Failed("Import stopped unexpectedly".into()));
                        }
                        break;
                    }
                }
            }
        }
        if let Some(o) = finished {
            job.rx = None;
            self.finish_job(job, o, ctx);
            return;
        }
        if let Some(since) = job.wait_frame_since {
            let ready = self.player.as_ref().map(|p| p.plain.is_some() || !p.info.has_video).unwrap_or(true);
            if ready || since.elapsed() > FIRST_FRAME_TIMEOUT || job.cancel.load(Ordering::Relaxed) {
                self.video_ready(job.video_tracks.take());
                if let Some(v) = self.queued_video.take() {
                    self.start_open_video(v, ctx);
                }
                return;
            }
            ctx.request_repaint_after(Duration::from_millis(30));
        } else {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        self.job = Some(job);
    }

    fn finish_job(&mut self, mut job: Job, o: Outcome, ctx: &egui::Context) {
        match o {
            Outcome::Failed(e) => self.flash_err(e),
            Outcome::Cancelled => self.flash("Cancelled"),
            Outcome::Subtitle { path, loaded, hint } => {
                let sibling = path.clone();
                self.apply_loaded(path, loaded, hint);
                if let (Some(p), true) = (sibling, self.settings.autoload_video && self.ffmpeg_ok) {
                    if let Some(v) = crate::app::find_sibling_video(&p) {
                        let abs = std::path::absolute(&v).unwrap_or_else(|_| v.clone());
                        if self.player.as_ref().map(|pl| pl.path != abs).unwrap_or(true) {
                            self.start_open_video(v, ctx);
                        }
                    }
                }
                if self.job.is_none() {
                    if let Some(v) = self.queued_video.take() {
                        self.start_open_video(v, ctx);
                    }
                }
            }
            Outcome::Tracks { video, duration_ms, tracks } => {
                if tracks.is_empty() {
                    self.flash_err(format!("No subtitle tracks found in {}", file_name(&video)));
                } else {
                    self.show_import_dialog(video, duration_ms, tracks);
                }
            }
            Outcome::Video { path, info, tracks } => {
                self.player = None;
                self.video_tex = None;
                self.video_tex_ptr = 0;
                self.preview = Default::default();
                let duration = info.duration_ms;
                match Player::open_probed(&self.tools, &path, ctx, self.settings.video_max_width, info) {
                    Ok(mut pl) => {
                        pl.set_volume(self.settings.volume);
                        let i = &pl.info;
                        self.flash(format!("Video: {} — {}x{} @ {:.3} fps, {}", path.display(), i.width, i.height, i.fps, subforge_core::time::fmt_display(i.duration_ms)));
                        self.tl.reset_for(i.duration_ms);
                        self.player = Some(pl);
                        job.steps[2].state = StepState::Running(None);
                        job.wait_frame_since = Some(Instant::now());
                        job.cancellable = true;
                        job.video_tracks = Some((path, duration, tracks));
                        self.job = Some(job);
                    }
                    Err(e) => self.flash_err(format!("Cannot open video: {e}")),
                }
            }
        }
    }

    /// Video is up (first frame decoded). Offer embedded subtitles if there is nothing loaded yet.
    fn video_ready(&mut self, tracks: Option<(PathBuf, i64, Vec<SubTrack>)>) {
        let Some((video, duration, tracks)) = tracks else { return };
        let usable = tracks.iter().any(|t| t.importable());
        self.video_tracks = Some((video.clone(), duration, tracks.clone()));
        let empty_doc = self.doc.sub.events.is_empty() && !self.doc.dirty && self.doc.path.is_none();
        if usable && empty_doc && self.settings.offer_embedded_subs {
            self.show_import_dialog(video, duration, tracks);
            self.import.with_video = false; // the video is already open
        }
    }

    fn show_import_dialog(&mut self, video: PathBuf, duration_ms: i64, tracks: Vec<SubTrack>) {
        let abs = std::path::absolute(&video).unwrap_or_else(|_| video.clone());
        let same_video = self.player.as_ref().is_some_and(|p| p.path == abs);
        let sel = tracks
            .iter()
            .position(|t| t.importable() && t.lang.eq_ignore_ascii_case("tha"))
            .or_else(|| tracks.iter().position(|t| t.importable() && t.default))
            .or_else(|| tracks.iter().position(|t| t.importable()))
            .unwrap_or(0);
        self.import = ImportDlg { open: true, video, duration_ms, tracks, sel, with_video: !same_video };
    }

    // ------------------------------------------------------------ UI
    pub fn loading_overlay(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else { return };
        if !job.visible() {
            return;
        }
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("loading_modal")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(20.0));
                ui.add(egui::Label::new(RichText::new(&job.title).strong().size(16.0)).truncate());
            });
            ui.add_space(8.0);
            for s in &job.steps {
                ui.horizontal(|ui| {
                    match &s.state {
                        StepState::Done => {
                            ui.label(RichText::new(icons::CHECK_CIRCLE).color(Color32::from_rgb(90, 190, 110)));
                        }
                        StepState::Running(_) => {
                            ui.add(egui::Spinner::new().size(14.0));
                        }
                        StepState::Failed(_) => {
                            ui.label(RichText::new(icons::X_CIRCLE).color(Color32::from_rgb(230, 90, 80)));
                        }
                        StepState::Pending => {
                            ui.label(RichText::new(icons::CIRCLE).weak());
                        }
                    }
                    let t = RichText::new(&s.label);
                    ui.label(if s.state == StepState::Pending { t.weak() } else { t });
                    if let StepState::Running(Some(p)) = &s.state {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format!("{:.0}%", p * 100.0)).monospace().weak());
                        });
                    }
                });
            }
            ui.add_space(8.0);
            ui.add(egui::ProgressBar::new(job.fraction()).animate(true).desired_width(ui.available_width()));
            if job.cancellable {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let label = if job.wait_frame_since.is_some() { "Skip waiting" } else { "Cancel" };
                    if ui.button(label).clicked() {
                        cancel = true;
                    }
                });
            }
        });
        if cancel {
            if let Some(j) = &self.job {
                j.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Small progress chips for slow optional work that keeps running after the file is usable.
    pub fn background_chips(&self, ui: &mut egui::Ui) {
        let Some(p) = &self.player else { return };
        let wf = p.waveform_progress().filter(|f| *f < 1.0);
        let kf = p.keyframes_loading();
        if wf.is_none() && !kf {
            return;
        }
        ui.separator();
        ui.add(egui::Spinner::new().size(12.0));
        if let Some(f) = wf {
            ui.add(egui::ProgressBar::new(f).desired_width(90.0).text(RichText::new(format!("waveform {:.0}%", f * 100.0)).small()));
        }
        if kf {
            ui.label(RichText::new("keyframes…").small().weak());
        }
    }

    pub fn import_dialog(&mut self, ctx: &egui::Context) {
        if !self.import.open {
            return;
        }
        let mut open = true;
        let mut go = false;
        let mut cancel = false;
        egui::Window::new("Import subtitles from video")
            .id(egui::Id::new("import_dlg"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([620.0, 320.0])
            .min_size([380.0, 200.0])
            .show(ctx, |ui| {
                let d = &mut self.import;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::FILM_STRIP).size(16.0));
                    ui.add(egui::Label::new(RichText::new(file_name(&d.video)).strong()).truncate());
                });
                ui.label(RichText::new("Pick the subtitle track to open. Image-based tracks (PGS / VobSub / DVB) need OCR and can't be imported.").weak().small());
                ui.add_space(4.0);
                egui::ScrollArea::both().id_salt("imp_tracks").auto_shrink([false, false]).max_height((ui.available_height() - 70.0).max(80.0)).show(ui, |ui| {
                    egui::Grid::new("imp_grid").num_columns(6).striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
                        for h in ["", "Language", "Format", "Title", "Lines", "Flags"] {
                            ui.strong(h);
                        }
                        ui.end_row();
                        for (i, t) in d.tracks.iter().enumerate() {
                            let ok = t.importable();
                            ui.add_enabled_ui(ok, |ui| {
                                if ui.radio(d.sel == i, format!("#{}", i + 1)).clicked() {
                                    d.sel = i;
                                }
                            });
                            let weak = |s: String| if ok { RichText::new(s) } else { RichText::new(s).weak() };
                            ui.label(weak(language_name(&t.lang)));
                            ui.label(weak(t.codec_label().to_string()));
                            ui.add(egui::Label::new(weak(t.title.clone())).truncate());
                            ui.label(weak(t.frames.map(|f| f.to_string()).unwrap_or_else(|| "-".into())));
                            let mut flags = Vec::new();
                            if t.default {
                                flags.push("default");
                            }
                            if t.forced {
                                flags.push("forced");
                            }
                            if !ok {
                                flags.push("image");
                            }
                            ui.label(weak(flags.join(", ")));
                            ui.end_row();
                        }
                    });
                });
                ui.add_space(4.0);
                ui.checkbox(&mut d.with_video, "Also open this video for preview").on_hover_text("Needs ffmpeg");
                ui.horizontal(|ui| {
                    let ok = d.tracks.get(d.sel).is_some_and(|t| t.importable());
                    if ui.add_enabled(ok, egui::Button::new(format!("{} Import", icons::DOWNLOAD_SIMPLE))).clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if !open || cancel {
            self.import.open = false;
        }
        if go {
            let d = &self.import;
            if let Some(t) = d.tracks.get(d.sel).cloned() {
                let action = Pending::Import { video: d.video.clone(), track: Box::new(t), duration_ms: d.duration_ms, with_video: d.with_video };
                self.import.open = false;
                self.guard(action, ctx);
            }
        }
    }
}
