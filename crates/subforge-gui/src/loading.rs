//! Background importing with a progress screen.
//!
//! Everything slow happens off the UI thread: reading/decoding/parsing subtitles (plus the first
//! quality check), probing video, extracting subtitle tracks from containers, and warming up the
//! player (first frame, keyframes, audio waveform, embedded fonts). A modal shows each step with
//! its progress and blocks editing until the project is ready, so the editor is smooth from the
//! first click.

use crate::app::{App, Doc};
use crate::icons::{self, ic, ic_s};
use crate::media::{self, MediaInfo, Player};
use eframe::egui::{self, Color32, RichText};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use subforge_core::extract::{self, Probe, Track};
use subforge_core::qc;

pub struct LoadedSub {
    pub loaded: subforge_core::Loaded,
    pub issues: Vec<qc::Issue>,
    pub cfg: qc::QcConfig,
    pub ms: u128,
}

pub enum Task {
    Subtitle {
        path: PathBuf,
        stage: Arc<AtomicU8>,
        rx: Receiver<Result<LoadedSub, String>>,
        /// video to open afterwards (sibling or the container it was extracted from)
        then_video: Option<PathBuf>,
    },
    VideoProbe {
        path: PathBuf,
        rx: Receiver<Result<MediaInfo, String>>,
    },
    VideoWarmup {
        started: Instant,
    },
    Tracks {
        video: PathBuf,
        rx: Receiver<Result<Probe, String>>,
    },
    PickTrack {
        video: PathBuf,
        probe: Probe,
        selected: usize,
        open_video: bool,
    },
    Track(crate::tracking::TrackJob),
    Mux {
        output: PathBuf,
        progress: Arc<AtomicU32>,
        cancel: Arc<AtomicBool>,
        rx: Receiver<Result<(), String>>,
    },
    Job(Job),
    Extract {
        video: PathBuf,
        track: Track,
        progress: Arc<AtomicU32>,
        cancel: Arc<AtomicBool>,
        rx: Receiver<Result<PathBuf, String>>,
        open_video: bool,
    },
}

/// Result of a generic background job.
pub enum JobOut {
    /// a new document (transcription, OCR) — inserted into the current one if that has unsaved work
    NewDoc { sub: subforge_core::Subtitle, suggested: Option<PathBuf>, msg: String },
    /// replace the current document's events (re-timing), as one undo step
    Replace { sub: subforge_core::Subtitle, msg: String },
    /// OCR finished: open the review window
    OcrReview(Box<crate::ocr_dlg::OcrReview>),
}

/// Shared progress of a job: (step label, progress 0..1 or <0 for indeterminate).
#[derive(Clone)]
pub struct JobCtx {
    status: Arc<std::sync::Mutex<(String, f32)>>,
    cancel: Arc<AtomicBool>,
}

impl JobCtx {
    pub fn set(&self, label: &str, progress: f32) {
        *self.status.lock().unwrap() = (label.to_string(), progress);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub struct Job {
    pub title: String,
    pub file: String,
    ctx: JobCtx,
    rx: Receiver<Result<JobOut, String>>,
}

pub fn spawn_job(title: impl Into<String>, file: impl Into<String>, f: impl FnOnce(&JobCtx) -> Result<JobOut, String> + Send + 'static) -> Task {
    let (tx, rx) = mpsc::channel();
    let ctx = JobCtx { status: Arc::new(std::sync::Mutex::new(("Starting".into(), -1.0))), cancel: Arc::new(AtomicBool::new(false)) };
    let c2 = ctx.clone();
    std::thread::spawn(move || {
        let r = f(&c2);
        let _ = tx.send(r);
    });
    Task::Job(Job { title: title.into(), file: file.into(), ctx, rx })
}

#[derive(Default)]
pub struct Loading {
    pub current: Option<Task>,
    pub queue: VecDeque<Task>,
}


#[derive(PartialEq, Clone, Copy)]
enum StepState {
    Waiting,
    Running,
    Done,
}

struct Step {
    label: String,
    state: StepState,
    progress: Option<f32>,
    detail: String,
}

fn step(label: impl Into<String>, state: StepState) -> Step {
    Step { label: label.into(), state, progress: None, detail: String::new() }
}

fn spawn_subtitle(path: PathBuf, cfg: qc::QcConfig, then_video: Option<PathBuf>) -> Task {
    let (tx, rx) = mpsc::channel();
    let stage = Arc::new(AtomicU8::new(0));
    let st = stage.clone();
    let p2 = path.clone();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let r = (|| {
            st.store(0, Ordering::Relaxed);
            let bytes = std::fs::read(&p2).map_err(|e| format!("cannot read {}: {e}", p2.display()))?;
            st.store(1, Ordering::Relaxed);
            let dec = subforge_core::encoding::decode(&bytes, None);
            st.store(2, Ordering::Relaxed);
            let (sub, format) = subforge_core::parse_str(&dec.text, subforge_core::Format::from_path(&p2)).map_err(|e| e.to_string())?;
            st.store(3, Ordering::Relaxed);
            let issues = qc::check(&sub, &cfg);
            Ok(LoadedSub { loaded: subforge_core::Loaded { sub, format, encoding: dec.encoding, had_bom: dec.had_bom }, issues, cfg, ms: t0.elapsed().as_millis() })
        })();
        let _ = tx.send(r);
    });
    Task::Subtitle { path, stage, rx, then_video }
}

fn spawn_probe(tools: &media::Tools, path: PathBuf) -> Task {
    let (tx, rx) = mpsc::channel();
    let (t, p) = (tools.clone(), path.clone());
    std::thread::spawn(move || {
        let _ = tx.send(media::probe(&t, &p).map_err(|e| e.to_string()));
    });
    Task::VideoProbe { path, rx }
}

fn spawn_tracks(tools: &media::Tools, video: PathBuf) -> Task {
    let (tx, rx) = mpsc::channel();
    let (t, v) = (tools.ffprobe.clone(), video.clone());
    std::thread::spawn(move || {
        let _ = tx.send(extract::probe_tracks(&t, &v));
    });
    Task::Tracks { video, rx }
}

pub fn spawn_mux(tools: &media::Tools, opts: subforge_core::extract::MuxOpts) -> Task {
    let (tx, rx) = mpsc::channel();
    let progress = Arc::new(AtomicU32::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p2, c2) = (progress.clone(), cancel.clone());
    let (ff, fp) = (tools.ffmpeg.clone(), tools.ffprobe.clone());
    let output = opts.output.clone();
    std::thread::spawn(move || {
        let r = extract::mux(&ff, &fp, &opts, |f| {
            p2.store((f * 1000.0) as u32, Ordering::Relaxed);
            !c2.load(Ordering::Relaxed)
        });
        let _ = tx.send(r);
    });
    Task::Mux { output, progress, cancel, rx }
}

fn spawn_extract(tools: &media::Tools, video: PathBuf, track: Track, duration_ms: i64, open_video: bool) -> Task {
    let (tx, rx) = mpsc::channel();
    let progress = Arc::new(AtomicU32::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p2, c2) = (progress.clone(), cancel.clone());
    let (ff, v, tr) = (tools.ffmpeg.clone(), video.clone(), track.clone());
    std::thread::spawn(move || {
        // extract into the cache work dir; the user saves wherever they like afterwards
        let dir = crate::cache::dir().join("extracted");
        let _ = std::fs::create_dir_all(&dir);
        let name = extract::default_output(&v, &tr).file_name().map(|n| n.to_os_string()).unwrap_or_default();
        let out = dir.join(name);
        let r = extract::extract_track(&ff, &v, &tr, duration_ms, &out, |f| {
            p2.store((f * 1000.0) as u32, Ordering::Relaxed);
            !c2.load(Ordering::Relaxed)
        });
        let _ = tx.send(r);
    });
    Task::Extract { video, track, progress, cancel, rx, open_video }
}

impl App {
    // ------------------------------------------------------------ entry points

    /// Open a subtitle file in the background.
    pub fn start_import(&mut self, path: &Path) {
        if media::is_media_path(path) {
            self.start_import_from_video(path, true);
            return;
        }
        let video = if self.settings.autoload_video && self.ffmpeg_ok {
            crate::app::find_sibling_video(path).filter(|v| self.player.as_ref().map(|pl| pl.path != std::path::absolute(v).unwrap_or_else(|_| v.clone())).unwrap_or(true))
        } else {
            None
        };
        self.loading.queue.push_back(spawn_subtitle(path.to_path_buf(), self.settings.qc.clone(), video));
    }

    /// Open a video/audio file for preview in the background.
    pub fn start_open_video(&mut self, path: &Path) {
        if !self.ensure_ffmpeg() {
            return;
        }
        self.loading.queue.push_back(spawn_probe(&self.tools, path.to_path_buf()));
    }

    /// List the subtitle tracks of a container and let the user pick one to import.
    pub fn start_import_from_video(&mut self, video: &Path, open_video: bool) {
        if !self.ensure_ffmpeg() {
            return;
        }
        let _ = open_video;
        self.loading.queue.push_back(spawn_tracks(&self.tools, video.to_path_buf()));
    }

    pub fn ensure_ffmpeg(&mut self) -> bool {
        if !self.ffmpeg_ok {
            self.tools = media::Tools::locate(&self.settings.ffmpeg_dir);
            self.ffmpeg_ok = self.tools.available();
            self.has_libass = self.ffmpeg_ok && self.tools.has_libass();
        }
        if !self.ffmpeg_ok {
            self.flash_err("ffmpeg/ffprobe not found. Install ffmpeg or set its folder in Settings.");
        }
        self.ffmpeg_ok
    }

    fn apply_loaded_sub(&mut self, path: &Path, ls: LoadedSub, display_path: Option<&Path>) {
        let l = ls.loaded;
        let n = l.sub.events.len();
        let fmt = l.format;
        let enc = l.encoding.clone();
        let gen = self.doc.gen + 1;
        self.doc = Doc { sub: l.sub, path: display_path.map(Path::to_path_buf), format: fmt, encoding: l.encoding, dirty: display_path.is_none(), gen };
        self.history.clear();
        self.buf_for = None;
        self.reset_cache_for_new_doc(ls.issues, ls.cfg);
        self.select_only(Some(0));
        if let Some(p) = display_path {
            self.push_recent(p);
        }
        self.flash(format!("Opened {} — {} lines, {}, {} ({} ms)", path.display(), n, fmt.ext().to_uppercase(), enc, ls.ms));
    }

    /// Put a generated subtitle on screen: a new document, or — when the current document has
    /// unsaved work — appended to it as one undo step.
    pub fn open_generated(&mut self, mut sub: subforge_core::Subtitle, suggested: Option<PathBuf>, msg: String) {
        let n = sub.events.len();
        if self.doc.dirty && !self.doc.sub.events.is_empty() {
            self.modify(|s| {
                s.ensure_defaults();
                s.events.append(&mut sub.events);
                s.sort_by_time();
            });
            self.buf_for = None;
            self.flash(format!("{msg} — {n} lines added to the current document (Ctrl+Z to undo)"));
            return;
        }
        let fmt = suggested.as_deref().and_then(subforge_core::Format::from_path).unwrap_or(subforge_core::Format::Srt);
        let cfg = self.settings.qc.clone();
        let issues = qc::check(&sub, &cfg);
        let ls = LoadedSub { loaded: subforge_core::Loaded { sub, format: fmt, encoding: "utf-8".into(), had_bom: false }, issues, cfg, ms: 0 };
        self.apply_loaded_sub(Path::new("(generated)"), ls, None);
        self.doc.path = suggested;
        self.doc.dirty = true;
        self.flash(format!("{msg} — {n} lines (unsaved)"));
    }

    fn finish_job(&mut self, out: JobOut) {
        match out {
            JobOut::NewDoc { sub, suggested, msg } => self.open_generated(sub, suggested, msg),
            JobOut::Replace { sub, msg } => {
                self.modify(|s| *s = sub);
                self.buf_for = None;
                self.flash(format!("{msg} (Ctrl+Z to undo)"));
            }
            JobOut::OcrReview(r) => {
                self.flash(format!("OCR done — {} lines, {} to check", r.lines.len(), r.lines.iter().filter(|l| l.confidence < 70.0 || l.text.is_empty()).count()));
                self.ocr_review = Some(*r);
            }
        }
    }

    // ------------------------------------------------------------ driver

    /// Advance background tasks. Call once per frame.
    pub fn poll_loading(&mut self, ctx: &egui::Context) {
        if self.loading.current.is_none() {
            self.loading.current = self.loading.queue.pop_front();
        }
        let Some(task) = self.loading.current.take() else { return };
        ctx.request_repaint_after(Duration::from_millis(50));
        let next: Option<Task> = match task {
            Task::Subtitle { path, stage, rx, then_video } => match rx.try_recv() {
                Ok(Ok(ls)) => {
                    self.apply_loaded_sub(&path, ls, Some(&path));
                    if let Some(v) = then_video {
                        self.loading.queue.push_front(spawn_probe(&self.tools, v));
                    }
                    None
                }
                Ok(Err(e)) => {
                    self.flash_err(format!("Open failed: {e}"));
                    None
                }
                Err(TryRecvError::Empty) => Some(Task::Subtitle { path, stage, rx, then_video }),
                Err(TryRecvError::Disconnected) => None,
            },
            Task::VideoProbe { path, rx } => match rx.try_recv() {
                Ok(Ok(info)) => {
                    self.player = None;
                    self.video_tex = None;
                    self.video_tex_ptr = 0;
                    self.preview = Default::default();
                    match Player::open_probed(&self.tools, &path, info, ctx, self.settings.video_max_width) {
                        Ok(mut pl) => {
                            pl.set_volume(self.settings.volume);
                            self.tl.reset_for(pl.info.duration_ms);
                            let i = &pl.info;
                            self.flash(format!("Video: {} — {}x{} @ {:.3} fps, {}", path.display(), i.width, i.height, i.fps, subforge_core::time::fmt_display(i.duration_ms)));
                            self.player = Some(pl);
                            Some(Task::VideoWarmup { started: Instant::now() })
                        }
                        Err(e) => {
                            self.flash_err(format!("Cannot open video: {e}"));
                            None
                        }
                    }
                }
                Ok(Err(e)) => {
                    self.flash_err(format!("Cannot open video: {e}"));
                    None
                }
                Err(TryRecvError::Empty) => Some(Task::VideoProbe { path, rx }),
                Err(TryRecvError::Disconnected) => None,
            },
            Task::VideoWarmup { started } => {
                let ready = self.player.as_ref().map(|p| p.warm_status().all_done()).unwrap_or(true);
                // never block forever: after 60 s the rest continues in the background
                if ready || started.elapsed() > Duration::from_secs(60) {
                    None
                } else {
                    Some(Task::VideoWarmup { started })
                }
            }
            Task::Tracks { video, rx } => match rx.try_recv() {
                Ok(Ok(probe)) => {
                    if probe.tracks.is_empty() {
                        self.flash_err(format!("{} has no subtitle tracks — opened as video only", video.display()));
                        self.loading.queue.push_front(spawn_probe(&self.tools, video));
                        None
                    } else {
                        let selected = probe.tracks.iter().position(|t| t.text_based && t.default).or_else(|| probe.tracks.iter().position(|t| t.text_based)).unwrap_or(0);
                        Some(Task::PickTrack { video, probe, selected, open_video: true })
                    }
                }
                Ok(Err(e)) => {
                    self.flash_err(e);
                    None
                }
                Err(TryRecvError::Empty) => Some(Task::Tracks { video, rx }),
                Err(TryRecvError::Disconnected) => None,
            },
            t @ Task::PickTrack { .. } => Some(t), // answered in the modal
            Task::Track(job) => match job.rx.try_recv() {
                Ok(Ok(track)) => {
                    let weak = track.weak_frames(self.tracker.params.min_score);
                    self.flash(format!("Tracked {} frames{}", track.a.len(), if weak > 0 { format!(", {weak} weak") } else { String::new() }));
                    self.tracker.result = Some(track);
                    self.show_pane(crate::layout::Pane::Motion);
                    None
                }
                Ok(Err(e)) => {
                    if e != "cancelled" {
                        self.flash_err(format!("Tracking failed: {e}"));
                    }
                    None
                }
                Err(TryRecvError::Empty) => Some(Task::Track(job)),
                Err(TryRecvError::Disconnected) => None,
            },
            Task::Job(job) => match job.rx.try_recv() {
                Ok(Ok(out)) => {
                    self.finish_job(out);
                    None
                }
                Ok(Err(e)) => {
                    if e != "cancelled" {
                        self.flash_err(format!("{} failed: {e}", job.title));
                    }
                    None
                }
                Err(TryRecvError::Empty) => Some(Task::Job(job)),
                Err(TryRecvError::Disconnected) => None,
            },
            Task::Mux { output, progress, cancel, rx } => match rx.try_recv() {
                Ok(Ok(())) => {
                    self.flash(format!("Muxed into {}", output.display()));
                    None
                }
                Ok(Err(e)) => {
                    if e != "cancelled" {
                        self.flash_err(format!("Mux failed: {e}"));
                    }
                    None
                }
                Err(TryRecvError::Empty) => Some(Task::Mux { output, progress, cancel, rx }),
                Err(TryRecvError::Disconnected) => None,
            },
            Task::Extract { video, track, progress, cancel, rx, open_video } => match rx.try_recv() {
                Ok(Ok(file)) => {
                    // load the extracted file; it becomes an unsaved document named after the video
                    let (tx2, rx2) = mpsc::channel();
                    let cfg = self.settings.qc.clone();
                    let f2 = file.clone();
                    std::thread::spawn(move || {
                        let t0 = Instant::now();
                        let r = subforge_core::load(&f2, None).map_err(|e| e.to_string()).map(|loaded| {
                            let issues = qc::check(&loaded.sub, &cfg);
                            LoadedSub { loaded, issues, cfg, ms: t0.elapsed().as_millis() }
                        });
                        let _ = tx2.send(r);
                    });
                    // reuse the subtitle task, but the document gets the "save next to the video" path
                    let target = extract::default_output(&video, &track);
                    self.extract_target = Some(target);
                    Some(Task::Subtitle { path: file, stage: Arc::new(AtomicU8::new(2)), rx: rx2, then_video: open_video.then_some(video) })
                }
                Ok(Err(e)) => {
                    if e != "cancelled" {
                        self.flash_err(format!("Extraction failed: {e}"));
                    }
                    None
                }
                Err(TryRecvError::Empty) => Some(Task::Extract { video, track, progress, cancel, rx, open_video }),
                Err(TryRecvError::Disconnected) => None,
            },
        };
        // the extracted document: point its path at the suggested save location, keep it dirty
        if next.is_none() {
            if let Some(target) = self.extract_target.take() {
                if self.doc.path.as_ref().is_some_and(|p| p.starts_with(crate::cache::dir())) {
                    self.doc.path = Some(target.clone());
                    self.doc.dirty = true;
                    self.flash(format!("Imported track — will be saved as {}", target.display()));
                } else {
                    self.extract_target = Some(target);
                }
            }
        }
        self.loading.current = next;
        if self.loading.current.is_some() || !self.loading.queue.is_empty() {
            // keep polling (and start the next queued job) without waiting for input
            ctx.request_repaint();
        }
    }

    // ------------------------------------------------------------ modal

    pub fn loading_modal(&mut self, ctx: &egui::Context) {
        if self.loading.current.is_none() {
            return;
        }
        let mut action: Option<&'static str> = None;
        let mut pick: Option<(usize, bool)> = None;
        let (title, file, steps) = self.describe_task();
        egui::Modal::new(egui::Id::new("loading_modal")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.label(ic(icons::ICON_HOURGLASS_EMPTY).size(22.0));
                ui.vertical(|ui| {
                    ui.heading(title);
                    ui.label(RichText::new(file).weak().small());
                });
            });
            ui.add_space(6.0);
            if let Some(Task::PickTrack { probe, selected, open_video, .. }) = &mut self.loading.current {
                ui.label("This file contains subtitle tracks. Choose one to import:");
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for (i, t) in probe.tracks.iter().enumerate() {
                        let ocr_able = subforge_core::ocr::is_bitmap_codec(&t.codec);
                        ui.add_enabled_ui(t.text_based || ocr_able, |ui| {
                            let icon = if ocr_able { icons::ICON_DOCUMENT_SCANNER } else { icons::ICON_SUBTITLES };
                            let label = if ocr_able { format!("{}  · image, OCR", t.label()) } else { t.label() };
                            let resp = ui.add(egui::Button::selectable(*selected == i, (ic_s(icon), label)).min_size(egui::vec2(430.0, 26.0)));
                            if resp.clicked() {
                                *selected = i;
                            }
                            if resp.double_clicked() {
                                pick = Some((i, *open_video));
                            }
                        })
                        .response
                        .on_disabled_hover_text("This subtitle codec is not supported");
                    }
                });
                ui.add_space(6.0);
                ui.checkbox(open_video, "Also open this file as the preview video");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ok = probe.tracks.get(*selected).is_some_and(|t| t.text_based);
                    let ocr_able = probe.tracks.get(*selected).is_some_and(|t| subforge_core::ocr::is_bitmap_codec(&t.codec));
                    if ocr_able {
                        if ui.add(crate::icons::btn(icons::ICON_DOCUMENT_SCANNER, "OCR track…")).on_hover_text("Recognise the text with Tesseract, then review it").clicked() {
                            pick = Some((*selected, *open_video));
                        }
                    } else if ui.add_enabled(ok, crate::icons::btn(icons::ICON_DOWNLOAD, "Import track")).clicked() {
                        pick = Some((*selected, *open_video));
                    }
                    if ui.add(crate::icons::btn(icons::ICON_MOVIE, "Video only")).clicked() {
                        action = Some("video_only");
                    }
                    if ui.add(crate::icons::btn(icons::ICON_CLOSE, "Cancel")).clicked() {
                        action = Some("cancel");
                    }
                });
                return;
            }
            for s in &steps {
                ui.horizontal(|ui| {
                    match s.state {
                        StepState::Done => {
                            ui.label(ic(icons::ICON_CHECK_CIRCLE).color(Color32::from_rgb(90, 190, 110)));
                        }
                        StepState::Running => {
                            ui.add(egui::Spinner::new().size(16.0));
                        }
                        StepState::Waiting => {
                            ui.label(ic(icons::ICON_RADIO_BUTTON_UNCHECKED).color(ui.visuals().weak_text_color()));
                        }
                    }
                    let mut t = RichText::new(&s.label);
                    if s.state == StepState::Waiting {
                        t = t.weak();
                    }
                    ui.label(t);
                    if !s.detail.is_empty() {
                        ui.label(RichText::new(&s.detail).weak().small());
                    }
                });
                if let (Some(p), StepState::Running) = (s.progress, s.state) {
                    ui.add(egui::ProgressBar::new(p).desired_height(6.0).desired_width(430.0));
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| match &self.loading.current {
                Some(Task::VideoWarmup { .. }) => {
                    if ui.add(crate::icons::btn(icons::ICON_SKIP_NEXT, "Continue now")).on_hover_text("Start editing; the remaining steps finish in the background").clicked() {
                        action = Some("skip");
                    }
                }
                Some(Task::Extract { .. }) | Some(Task::Mux { .. }) | Some(Task::Track(_)) | Some(Task::Job(_)) => {
                    if ui.add(crate::icons::btn(icons::ICON_CANCEL, "Cancel")).clicked() {
                        action = Some("cancel_extract");
                    }
                }
                _ => {}
            });
        });
        if let Some((i, open)) = pick {
            if let Some(Task::PickTrack { video, probe, .. }) = self.loading.current.take() {
                let t = probe.tracks[i].clone();
                if subforge_core::ocr::is_bitmap_codec(&t.codec) {
                    self.open_ocr_dialog(video, t, open);
                } else {
                    self.loading.current = Some(spawn_extract(&self.tools, video, t, probe.duration_ms, open));
                }
            }
        }
        match action {
            Some("cancel") => self.loading.current = None,
            Some("video_only") => {
                if let Some(Task::PickTrack { video, .. }) = self.loading.current.take() {
                    self.loading.current = Some(spawn_probe(&self.tools, video));
                }
            }
            Some("skip") => self.loading.current = None,
            Some("cancel_extract") => {
                match &self.loading.current {
                    Some(Task::Extract { cancel, .. }) | Some(Task::Mux { cancel, .. }) => cancel.store(true, Ordering::Relaxed),
                    Some(Task::Track(j)) => j.cancel.store(true, Ordering::Relaxed),
                    Some(Task::Job(j)) => j.ctx.cancel.store(true, Ordering::Relaxed),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn describe_task(&self) -> (String, String, Vec<Step>) {
        use StepState::*;
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match &self.loading.current {
            Some(Task::Subtitle { path, stage, .. }) => {
                let s = stage.load(Ordering::Relaxed);
                let st = |i: u8| if s > i { Done } else if s == i { Running } else { Waiting };
                (
                    "Opening subtitles".into(),
                    name(path),
                    vec![step("Reading file", st(0)), step("Detecting encoding & format", st(1)), step("Parsing styles and events", st(2)), step("Running quality check", st(3))],
                )
            }
            Some(Task::VideoProbe { path, .. }) => ("Opening video".into(), name(path), vec![step("Probing streams", Running), step("Preparing decoder", Waiting)]),
            Some(Task::VideoWarmup { .. }) => {
                let Some(p) = &self.player else { return ("Opening video".into(), String::new(), vec![]) };
                let w = p.warm_status();
                let mk = |label: &str, done: bool, prog: Option<f32>, detail: String| Step { label: label.into(), state: if done { Done } else { Running }, progress: if done { None } else { prog }, detail };
                let mut v = vec![mk("Decoding first frame", w.first_frame, None, String::new())];
                if p.info.has_video {
                    v.push(mk("Reading keyframes", w.keyframes, Some(w.keyframe_progress), w.keyframe_count.map(|n| format!("{n} found")).unwrap_or_default()));
                    v.push(mk("Extracting embedded fonts", w.fonts, None, String::new()));
                }
                if p.info.has_audio {
                    v.push(mk(
                        "Building audio waveform",
                        w.waveform,
                        Some(w.waveform_progress),
                        if w.waveform_cached { "from cache".into() } else { format!("{:.0}%", w.waveform_progress * 100.0) },
                    ));
                }
                ("Preparing video".into(), name(&p.path), v)
            }
            Some(Task::Tracks { video, .. }) => ("Reading container".into(), name(video), vec![step("Listing subtitle tracks", Running)]),
            Some(Task::PickTrack { video, .. }) => ("Import subtitles from video".into(), name(video), vec![]),
            Some(Task::Extract { video, track, progress, .. }) => {
                let p = progress.load(Ordering::Relaxed) as f32 / 1000.0;
                (
                    "Extracting subtitle track".into(),
                    name(video),
                    vec![
                        step("Listing subtitle tracks", Done),
                        Step { label: format!("Extracting {}", track.label()), state: Running, progress: Some(p), detail: format!("{:.0}%", p * 100.0) },
                        step("Parsing", Waiting),
                    ],
                )
            }
            Some(Task::Track(j)) => {
                let p = j.progress.load(Ordering::Relaxed) as f32 / 1000.0;
                (
                    "Motion tracking".into(),
                    format!("{} frames", j.frames),
                    vec![Step { label: "Decoding frames and matching points".into(), state: Running, progress: Some(p), detail: format!("{:.0}%", p * 100.0) }],
                )
            }
            Some(Task::Job(j)) => {
                let (label, p) = j.ctx.status.lock().unwrap().clone();
                (
                    j.title.clone(),
                    j.file.clone(),
                    vec![Step { label, state: Running, progress: (p >= 0.0).then_some(p), detail: if p >= 0.0 { format!("{:.0}%", p * 100.0) } else { String::new() } }],
                )
            }
            Some(Task::Mux { output, progress, .. }) => {
                let p = progress.load(Ordering::Relaxed) as f32 / 1000.0;
                (
                    "Muxing into MKV".into(),
                    name(output),
                    vec![step("Writing subtitles", Done), Step { label: "Copying streams + attaching fonts".into(), state: Running, progress: Some(p), detail: format!("{:.0}%", p * 100.0) }],
                )
            }
            None => (String::new(), String::new(), vec![]),
        }
    }
}
