//! Application state, document handling, menus, shortcuts and layout.

use crate::dialogs::Dialogs;
use crate::media::{self, Player, Tools};
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use subforge_core::qc::{self, Issue, QcConfig, Severity};
use subforge_core::{edit, Format, Subtitle, WriteOpts};

pub const APP_NAME: &str = "SubForge";

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub qc: QcConfig,
    pub ffmpeg_dir: String,
    pub ui_font: String,
    pub recent: Vec<String>,
    pub save_encoding: String,
    pub autoload_video: bool,
    pub default_duration_ms: i64,
    pub dark: bool,
    pub video_max_width: u32,
    pub volume: f32,
    pub show_original_column: bool,
    pub autosave_minutes: u32,
    pub ui_scale: f32,
    pub seek_step_ms: i64,
    /// Render paused frames with libass for an exact subtitle preview
    pub libass_preview: bool,
    /// Keyframe snapping window before / after a time (ms)
    pub kf_before_ms: i64,
    pub kf_after_ms: i64,
    /// Render with libass while playing too
    pub libass_playback: bool,
    pub show_safe_area: bool,
    // timeline
    pub tl_snap_keyframes: bool,
    pub tl_snap_lines: bool,
    pub tl_snap_playhead: bool,
    pub tl_snap_px: f32,
    pub tl_show_wave: bool,
    pub tl_show_keyframes: bool,
    pub tl_show_comments: bool,
    pub tl_show_text: bool,
    pub tl_wave_gain: f32,
    pub tl_lanes: usize,
    pub grid_hide_frames: bool,
    /// Extra folder searched for fonts
    pub fonts_dir: String,
    /// 0 = waveform, 1 = spectrum, 2 = both
    pub tl_audio_view: u8,
    pub tl_spec_gain: f32,
    // speech (whisper.cpp)
    pub whisper_exe: String,
    pub whisper_model: String,
    pub whisper_lang: String,
    pub whisper_threads: usize,
    pub whisper_prompt: String,
    pub whisper_extra: String,
    // OCR (Tesseract)
    pub tesseract_exe: String,
    pub tessdata_dir: String,
    pub ocr_lang: String,
    pub ocr_dict_fix: bool,
    pub ocr_replace: Vec<(String, String)>,
    // spelling
    pub spell_en: bool,
    pub spell_th: bool,
    pub spell_underline: bool,
    pub spell_user: Vec<String>,
    /// enabled "Fix common errors" rule ids (empty = defaults)
    pub fix_rules: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            qc: QcConfig::default(),
            ffmpeg_dir: String::new(),
            ui_font: String::new(),
            recent: Vec::new(),
            save_encoding: "utf-8".into(),
            autoload_video: true,
            default_duration_ms: 2000,
            dark: true,
            video_max_width: 1280,
            volume: 1.0,
            show_original_column: true,
            autosave_minutes: 2,
            ui_scale: 1.0,
            seek_step_ms: 2000,
            libass_preview: true,
            kf_before_ms: 300,
            kf_after_ms: 300,
            libass_playback: true,
            show_safe_area: false,
            tl_snap_keyframes: true,
            tl_snap_lines: true,
            tl_snap_playhead: true,
            tl_snap_px: 8.0,
            tl_show_wave: true,
            tl_show_keyframes: true,
            tl_show_comments: false,
            tl_show_text: true,
            tl_wave_gain: 1.0,
            tl_lanes: 3,
            grid_hide_frames: true,
            fonts_dir: String::new(),
            tl_audio_view: 0,
            tl_spec_gain: 1.4,
            whisper_exe: String::new(),
            whisper_model: String::new(),
            whisper_lang: "ja".into(),
            whisper_threads: 0,
            whisper_prompt: String::new(),
            whisper_extra: String::new(),
            tesseract_exe: String::new(),
            tessdata_dir: String::new(),
            ocr_lang: "tha+eng".into(),
            ocr_dict_fix: true,
            ocr_replace: Vec::new(),
            spell_en: true,
            spell_th: true,
            spell_underline: true,
            spell_user: Vec::new(),
            fix_rules: Vec::new(),
        }
    }
}

pub struct Doc {
    pub sub: Subtitle,
    pub path: Option<PathBuf>,
    pub format: Format,
    pub encoding: String,
    pub dirty: bool,
    /// Incremented on every change (cache key).
    pub gen: u64,
}

impl Doc {
    pub fn new() -> Doc {
        Doc { sub: Subtitle::new(), path: None, format: Format::Ass, encoding: "UTF-8".into(), dirty: false, gen: 1 }
    }
    pub fn name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled".into())
    }
}

#[derive(Clone, Debug)]
pub enum Pending {
    New,
    Open(Option<PathBuf>),
    ImportVideo(PathBuf),
    Quit,
}

pub struct IssueCache {
    pub gen: u64,
    pub cfg: QcConfig,
    pub issues: Vec<Issue>,
    pub per_event: Vec<Option<Severity>>,
}

pub struct RefCache {
    pub gen: u64,
    pub map: Vec<Option<usize>>,
}

/// Debounce state for libass preview renders.
pub struct PreviewState {
    pub last_key: (i64, u64),
    pub since: Instant,
    pub requested: Option<(i64, u64)>,
    pub written_gen: u64,
    // libass playback
    pub was_playing: bool,
    pub play_gen: u64,
    pub last_edit_gen: u64,
    pub last_edit: Instant,
}

impl Default for PreviewState {
    fn default() -> Self {
        PreviewState { last_key: (-1, 0), since: Instant::now(), requested: None, written_gen: 0, was_playing: false, play_gen: 0, last_edit_gen: 0, last_edit: Instant::now() }
    }
}

pub struct Reference {
    pub path: PathBuf,
    pub sub: Subtitle,
    /// Match by index instead of by time
    pub by_index: bool,
}

pub struct App {
    pub settings: Settings,
    pub doc: Doc,
    pub history: crate::undo::History,
    pub sel: BTreeSet<usize>,
    pub cur: Option<usize>,
    pub anchor: Option<usize>,
    pub scroll_to: Option<usize>,
    // editor buffers
    pub buf_for: Option<(usize, u64)>,
    pub start_buf: String,
    pub end_buf: String,
    pub dur_buf: String,
    pub text_buf: String,
    pub text_session: bool,
    // media
    pub tools: Tools,
    pub ffmpeg_ok: bool,
    pub player: Option<Player>,
    pub video_tex: Option<egui::TextureHandle>,
    pub video_tex_ptr: usize,
    pub has_libass: bool,
    pub preview: PreviewState,
    pub tl: crate::timeline::TimelineState,
    // caches
    pub issues: Option<IssueCache>,
    pub reference: Option<Reference>,
    pub ref_cache: Option<RefCache>,
    // ui
    pub dialogs: Dialogs,
    pub status: Option<(String, Instant, bool)>,
    pub pending: Option<Pending>,
    allow_close: bool,
    last_autosave: Instant,
    last_title: String,
    pub grid_filter: String,
    pub issue_filter_min: Severity,
    // layout / loading / typesetting
    pub tree: Option<egui_tiles::Tree<crate::layout::Pane>>,
    pub loading: crate::loading::Loading,
    pub extract_target: Option<PathBuf>,
    pub vtool: crate::motion::VideoTool,
    pub motion: crate::motion::MotionState,
    pub kf_override: Option<std::sync::Arc<Vec<i64>>>,
    pub drag_axis: Option<u8>,
    /// frame-by-frame source whose text changed (frames are regenerated when editing ends)
    pub regen_pending: Option<usize>,
    pub font_db: crate::fonts_dlg::FontDb,
    pub typeset: crate::typeset::TypesetState,
    pub tracker: crate::tracking::TrackerState,
    pub ocr_review: Option<crate::ocr_dlg::OcrReview>,
    pub speller: crate::spell_dlg::Speller,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, open_file: Option<PathBuf>, open_video: Option<PathBuf>) -> App {
        let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, "settings")).unwrap_or_default();
        let layout_json: Option<String> = cc.storage.and_then(|s| s.get_string("layout_v1"));
        crate::fonts::install(&cc.egui_ctx, Some(&settings.ui_font));
        apply_theme(&cc.egui_ctx, &settings);
        let tools = Tools::locate(&settings.ffmpeg_dir);
        let ffmpeg_ok = tools.available();
        let has_libass = ffmpeg_ok && tools.has_libass();
        crate::cache::prune(Duration::from_secs(60 * 60 * 24 * 45));
        let mut app = App {
            settings,
            doc: Doc::new(),
            history: Default::default(),
            sel: BTreeSet::new(),
            cur: None,
            anchor: None,
            scroll_to: None,
            buf_for: None,
            start_buf: String::new(),
            end_buf: String::new(),
            dur_buf: String::new(),
            text_buf: String::new(),
            text_session: false,
            tools,
            ffmpeg_ok,
            player: None,
            video_tex: None,
            video_tex_ptr: 0,
            has_libass: false,
            preview: PreviewState::default(),
            tl: Default::default(),
            issues: None,
            reference: None,
            ref_cache: None,
            dialogs: Dialogs::default(),
            status: None,
            pending: None,
            allow_close: false,
            last_autosave: Instant::now(),
            last_title: String::new(),
            grid_filter: String::new(),
            issue_filter_min: Severity::Info,
            tree: Some(crate::layout::restore(layout_json)),
            loading: Default::default(),
            extract_target: None,
            vtool: Default::default(),
            motion: Default::default(),
            kf_override: None,
            drag_axis: None,
            regen_pending: None,
            font_db: crate::fonts_dlg::FontDb::start(&cc.egui_ctx),
            typeset: Default::default(),
            tracker: Default::default(),
            ocr_review: None,
            speller: Default::default(),
        };
        app.has_libass = has_libass;
        if !app.ffmpeg_ok {
            app.flash_err("ffmpeg not found — video preview & waveform disabled (set its folder in Settings)");
        }
        app.select_only(None);
        if let Some(p) = open_file {
            app.start_import(&p);
        }
        if let Some(v) = open_video {
            if app.loading.queue.is_empty() {
                // only a video: offer its subtitle tracks (falls back to "video only")
                app.start_import_from_video(&v, true);
            } else {
                app.start_open_video(&v);
            }
        }
        app
    }

    // ------------------------------------------------------------ messages
    pub fn flash(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now(), false));
    }
    pub fn flash_err(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now(), true));
    }

    // ------------------------------------------------------------ undo / modify
    /// Open an undo step before a change (continuous edits share one step until committed).
    pub fn checkpoint(&mut self) {
        self.history.begin(&self.doc.sub, self.cur);
    }
    /// Mark the document as changed (after mutating `doc.sub`).
    pub fn touch(&mut self) {
        self.doc.gen += 1;
        self.doc.dirty = true;
    }
    /// Checkpoint + mutate + touch, recorded as one undo step.
    pub fn modify<R>(&mut self, f: impl FnOnce(&mut Subtitle) -> R) -> R {
        self.end_text_session();
        self.checkpoint();
        let r = f(&mut self.doc.sub);
        self.touch();
        self.history.commit(&self.doc.sub);
        self.fix_selection();
        r
    }
    /// Close a continuous edit (typing, dragging, style tweaks) into one undo step.
    pub fn end_text_session(&mut self) {
        self.text_session = false;
        self.history.commit(&self.doc.sub);
    }
    pub fn undo(&mut self) {
        self.text_session = false;
        if let Some(c) = self.history.undo(&mut self.doc.sub) {
            self.touch();
            self.select_only(c);
            self.buf_for = None;
        }
    }
    pub fn redo(&mut self) {
        self.text_session = false;
        if let Some(c) = self.history.redo(&mut self.doc.sub) {
            self.touch();
            self.select_only(c);
            self.buf_for = None;
        }
    }
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    // ------------------------------------------------------------ selection
    pub fn fix_selection(&mut self) {
        let n = self.doc.sub.events.len();
        self.sel.retain(|&i| i < n);
        if let Some(c) = self.cur {
            if c >= n {
                self.cur = if n > 0 { Some(n - 1) } else { None };
            }
        }
        if self.sel.is_empty() {
            if let Some(c) = self.cur {
                self.sel.insert(c);
            }
        }
    }
    pub fn select_only(&mut self, i: Option<usize>) {
        let i = i.filter(|&i| i < self.doc.sub.events.len()).or_else(|| (!self.doc.sub.events.is_empty()).then_some(0));
        if self.cur != i {
            self.end_text_session();
        }
        self.sel.clear();
        if let Some(i) = i {
            self.sel.insert(i);
        }
        self.cur = i;
        self.anchor = i;
        self.scroll_to = i;
    }
    pub fn click_row(&mut self, i: usize, mods: Modifiers) {
        if mods.shift {
            let a = self.anchor.unwrap_or(i);
            let (lo, hi) = (a.min(i), a.max(i));
            if !mods.command {
                self.sel.clear();
            }
            self.sel.extend(lo..=hi);
            if self.cur != Some(i) {
                self.end_text_session();
            }
            self.cur = Some(i);
        } else if mods.command {
            if !self.sel.insert(i) {
                self.sel.remove(&i);
            }
            self.end_text_session();
            self.cur = Some(i);
            self.anchor = Some(i);
        } else {
            self.select_only(Some(i));
            self.scroll_to = None;
        }
    }
    pub fn selected(&self) -> Vec<usize> {
        self.sel.iter().copied().collect()
    }
    pub fn go_next(&mut self, create: bool) {
        let n = self.doc.sub.events.len();
        match self.cur {
            Some(c) if c + 1 < n => self.select_only(Some(c + 1)),
            Some(c) if create => {
                let d = self.settings.default_duration_ms;
                let i = self.modify(|s| edit::insert_after(s, Some(c), d));
                self.select_only(Some(i));
            }
            None if n > 0 => self.select_only(Some(0)),
            _ => {}
        }
    }
    pub fn go_prev(&mut self) {
        if let Some(c) = self.cur {
            if c > 0 {
                self.select_only(Some(c - 1));
            }
        }
    }

    // ------------------------------------------------------------ caches
    pub fn issues(&mut self) -> &IssueCache {
        let stale = self.issues.as_ref().map(|c| c.gen != self.doc.gen || c.cfg != self.settings.qc).unwrap_or(true);
        if stale {
            let issues = qc::check(&self.doc.sub, &self.settings.qc);
            let mut per = vec![None; self.doc.sub.events.len()];
            for i in &issues {
                if i.index < per.len() {
                    let cur: &mut Option<Severity> = &mut per[i.index];
                    if cur.map(|c| i.severity > c).unwrap_or(true) {
                        *cur = Some(i.severity);
                    }
                }
            }
            self.issues = Some(IssueCache { gen: self.doc.gen, cfg: self.settings.qc.clone(), issues, per_event: per });
        }
        self.issues.as_ref().unwrap()
    }
    pub fn ref_map(&mut self) -> Option<&Vec<Option<usize>>> {
        let r = self.reference.as_ref()?;
        let stale = self.ref_cache.as_ref().map(|c| c.gen != self.doc.gen).unwrap_or(true);
        if stale {
            let map = if r.by_index {
                (0..self.doc.sub.events.len()).map(|i| (i < r.sub.events.len()).then_some(i)).collect()
            } else {
                edit::match_by_time(&self.doc.sub, &r.sub)
            };
            self.ref_cache = Some(RefCache { gen: self.doc.gen, map });
        }
        self.ref_cache.as_ref().map(|c| &c.map)
    }
    pub fn original_text(&mut self, i: usize) -> Option<String> {
        let j = (*self.ref_map()?.get(i)?)?;
        self.reference.as_ref()?.sub.events.get(j).map(|e| e.text.clone())
    }

    // ------------------------------------------------------------ files
    pub fn guard(&mut self, action: Pending, ctx: &egui::Context) {
        if self.doc.dirty {
            self.pending = Some(action);
        } else {
            self.run_pending(action, ctx);
        }
    }
    pub fn run_pending(&mut self, action: Pending, ctx: &egui::Context) {
        match action {
            Pending::New => {
                self.doc = Doc::new();
                self.history.clear();
                self.select_only(None);
                self.buf_for = None;
            }
            Pending::Open(Some(p)) => self.start_import(&p),
            Pending::Open(None) => {
                let mut exts: Vec<&str> = vec!["srt", "ass", "ssa", "vtt"];
                exts.extend(["mkv", "mp4", "m4v", "webm", "ts", "m2ts", "mov", "avi"]);
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Subtitles & videos with subtitles", &exts)
                    .add_filter("Subtitles", &["srt", "ass", "ssa", "vtt"])
                    .add_filter("Video (import a subtitle track)", media::VIDEO_EXTS)
                    .add_filter("All files", &["*"])
                    .set_directory(self.last_dir())
                    .pick_file()
                {
                    self.start_import(&p);
                }
            }
            Pending::ImportVideo(p) => self.start_import_from_video(&p, true),
            Pending::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
    pub fn last_dir(&self) -> PathBuf {
        self.doc
            .path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| self.settings.recent.first().and_then(|r| Path::new(r).parent().map(Path::to_path_buf)))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
    }
    /// Install a freshly computed QC result for a newly loaded document.
    pub fn reset_cache_for_new_doc(&mut self, issues: Vec<Issue>, cfg: QcConfig) {
        let mut per = vec![None; self.doc.sub.events.len()];
        for i in &issues {
            if i.index < per.len() {
                let cur: &mut Option<Severity> = &mut per[i.index];
                if cur.map(|c| i.severity > c).unwrap_or(true) {
                    *cur = Some(i.severity);
                }
            }
        }
        self.issues = Some(IssueCache { gen: self.doc.gen, cfg, issues, per_event: per });
        self.ref_cache = None;
        self.motion.loaded_for = None;
    }
    pub(crate) fn push_recent(&mut self, p: &Path) {
        if p.starts_with(crate::cache::dir()) {
            return;
        }
        let s = p.display().to_string();
        self.settings.recent.retain(|r| r != &s);
        self.settings.recent.insert(0, s);
        self.settings.recent.truncate(12);
    }
    pub fn write_opts(&self) -> WriteOpts {
        WriteOpts { encoding: self.settings.save_encoding.clone(), ..Default::default() }
    }
    pub fn save(&mut self) -> bool {
        self.end_text_session();
        match self.doc.path.clone() {
            Some(p) => self.save_to(&p, self.doc.format),
            None => self.save_as(None),
        }
    }
    pub fn save_as(&mut self, fmt: Option<Format>) -> bool {
        let fmt = fmt.unwrap_or(self.doc.format);
        let stem = self.doc.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "untitled".into());
        let mut dlg = rfd::FileDialog::new().set_directory(self.last_dir()).set_file_name(format!("{}.{}", stem, fmt.ext()));
        dlg = dlg.add_filter(fmt.name(), &[fmt.ext()]);
        for f in Format::ALL {
            if f != fmt {
                dlg = dlg.add_filter(f.name(), &[f.ext()]);
            }
        }
        let Some(p) = dlg.save_file() else { return false };
        let real = Format::from_path(&p).unwrap_or(fmt);
        self.save_to(&p, real)
    }
    pub fn save_to(&mut self, p: &Path, fmt: Format) -> bool {
        let mut sub = self.doc.sub.clone();
        subforge_core::prepare_for(&mut sub, fmt);
        match subforge_core::save(p, &sub, Some(fmt), &self.write_opts()) {
            Ok(_) => {
                if fmt != Format::Txt {
                    self.doc.path = Some(p.to_path_buf());
                    self.doc.format = fmt;
                    self.doc.dirty = false;
                    self.push_recent(p);
                }
                self.flash(format!("Saved {}", p.display()));
                true
            }
            Err(e) => {
                self.flash_err(format!("Save failed: {e}"));
                false
            }
        }
    }
    pub fn export(&mut self, fmt: Format) {
        let stem = self.doc.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "untitled".into());
        if let Some(p) = rfd::FileDialog::new().set_directory(self.last_dir()).set_file_name(format!("{}.{}", stem, fmt.ext())).add_filter(fmt.name(), &[fmt.ext()]).save_file() {
            let mut sub = self.doc.sub.clone();
            subforge_core::prepare_for(&mut sub, fmt);
            match subforge_core::save(&p, &sub, Some(fmt), &self.write_opts()) {
                Ok(_) => self.flash(format!("Exported {}", p.display())),
                Err(e) => self.flash_err(format!("Export failed: {e}")),
            }
        }
    }
    fn autosave(&mut self) {
        if self.settings.autosave_minutes == 0 || !self.doc.dirty {
            return;
        }
        if self.last_autosave.elapsed() < Duration::from_secs(self.settings.autosave_minutes as u64 * 60) {
            return;
        }
        self.last_autosave = Instant::now();
        let dir = std::env::temp_dir().join("subforge-autosave");
        let _ = std::fs::create_dir_all(&dir);
        let name = format!("{}.autosave.ass", self.doc.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "untitled".into()));
        let mut sub = self.doc.sub.clone();
        subforge_core::prepare_for(&mut sub, Format::Ass);
        let _ = subforge_core::save(dir.join(name), &sub, Some(Format::Ass), &WriteOpts::default());
    }

    pub fn open_video_dialog(&mut self, _ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Video / audio", media::VIDEO_EXTS).add_filter("All files", &["*"]).set_directory(self.last_dir()).pick_file() {
            self.start_open_video(&p);
        }
    }
    pub fn video_time(&self) -> Option<i64> {
        self.player.as_ref().map(|p| p.time())
    }
    pub fn seek(&mut self, t: i64) {
        if let Some(p) = &mut self.player {
            p.seek(t);
        }
        self.tl.playhead_hint = t;
    }
    /// Step exactly `delta` frames (by frame index, so repeated steps never drift).
    pub fn step_frame(&mut self, delta: i64) {
        if let Some(p) = &self.player {
            let idx = p.fc.idx_of(p.time()) + delta;
            let t = p.fc.time_of(idx.max(0));
            self.seek(t);
        }
    }
    /// Seek to the first video frame on which a line starting at `t` is visible.
    pub fn seek_line(&mut self, t: i64) {
        let t2 = match &self.player {
            Some(p) => {
                let idx = ((t as f64 - 1.0) / p.fc.frame_ms).ceil().max(0.0) as i64;
                p.fc.time_of(idx).max(t)
            }
            None => t,
        };
        self.seek(t2);
    }
    pub fn play_range(&mut self, a: i64, b: i64) {
        if let Some(p) = &mut self.player {
            p.play_from(a, Some(b));
        }
    }
    pub fn play_current(&mut self) {
        if let Some(e) = self.cur.and_then(|c| self.doc.sub.events.get(c)) {
            let (a, b) = (e.start, e.end);
            self.play_range(a, b);
        }
    }

    // ------------------------------------------------------------ editing helpers used by several panels
    pub fn set_cur_time(&mut self, start: bool) {
        let Some(t) = self.video_time() else { return };
        let Some(c) = self.cur else { return };
        self.modify(|s| {
            let e = &mut s.events[c];
            if start {
                e.start = t;
                if e.end <= e.start {
                    e.end = e.start + 1000;
                }
            } else {
                e.end = t.max(e.start + 1);
            }
        });
        self.buf_for = None;
    }
    pub fn delete_selected(&mut self) {
        let mut v = self.selected();
        if v.is_empty() {
            return;
        }
        v.sort_unstable();
        let first = v[0];
        self.modify(|s| {
            for &i in v.iter().rev() {
                if i < s.events.len() {
                    s.events.remove(i);
                }
            }
        });
        self.select_only(Some(first.min(self.doc.sub.events.len().saturating_sub(1))));
    }
    pub fn insert(&mut self, after: bool) {
        let d = self.settings.default_duration_ms;
        let cur = self.cur;
        let i = self.modify(|s| match (cur, after) {
            (Some(c), true) => edit::insert_after(s, Some(c), d),
            (Some(c), false) => edit::insert_before(s, c, d),
            (None, _) => edit::insert_after(s, None, d),
        });
        self.select_only(Some(i));
        self.dialogs.focus_text = true;
    }
    pub fn insert_at(&mut self, t: i64) {
        let d = self.settings.default_duration_ms;
        let style = self.cur.and_then(|c| self.doc.sub.events.get(c)).map(|e| e.style.clone()).unwrap_or_else(|| "Default".into());
        let i = self.modify(|s| {
            let pos = s.events.iter().position(|e| e.start > t).unwrap_or(s.events.len());
            let mut e = subforge_core::Event::new(t, t + d, "");
            e.style = style;
            s.events.insert(pos, e);
            pos
        });
        self.select_only(Some(i));
        self.dialogs.focus_text = true;
    }
    pub fn duplicate_selected(&mut self) {
        let v = self.selected();
        if v.is_empty() {
            return;
        }
        let last = *v.last().unwrap();
        self.modify(|s| {
            let copies: Vec<_> = v.iter().map(|&i| s.events[i].clone()).collect();
            for (k, e) in copies.into_iter().enumerate() {
                s.events.insert(last + 1 + k, e);
            }
        });
        self.select_only(Some(last + 1));
    }
    pub fn split_current(&mut self) {
        let Some(c) = self.cur else { return };
        let at = self.video_time().filter(|t| {
            let e = &self.doc.sub.events[c];
            *t > e.start && *t < e.end
        });
        if let Some(i) = self.modify(|s| edit::split(s, c, at)) {
            self.select_only(Some(i - 1));
        }
    }
    pub fn merge_selected(&mut self, newline: bool) {
        let mut v = self.selected();
        if v.len() == 1 {
            if let Some(c) = self.cur {
                if c + 1 < self.doc.sub.events.len() {
                    v.push(c + 1);
                }
            }
        }
        if let Some(i) = self.modify(|s| edit::merge(s, &v, newline)) {
            self.select_only(Some(i));
        }
    }
    pub fn toggle_comment(&mut self) {
        let v = self.selected();
        self.modify(|s| {
            for i in v {
                s.events[i].comment = !s.events[i].comment;
            }
        });
    }
    /// Re-apply the animation stored on line `i` (after its text or timing changed).
    pub fn regenerate_animation(&mut self, i: usize) {
        let Some(tw) = subforge_core::anim::read_tween(&self.doc.sub, i) else { return };
        let fps = self.fps_or_default();
        if !self.text_session {
            self.checkpoint();
        }
        if let Ok(r) = subforge_core::anim::apply_tween(&mut self.doc.sub, i, &tw, fps) {
            self.touch();
            let src = subforge_core::anim::find_source(&self.doc.sub, r.index).unwrap_or(r.index);
            self.cur = Some(src);
            self.sel.clear();
            self.sel.insert(src);
            self.buf_for = None;
        }
    }
    pub fn snap_selected_to_keyframes(&mut self) {
        let Some(kfs) = self.keyframes() else {
            self.flash_err("Keyframes not available (open a video or load a keyframe file)");
            return;
        };
        let v = self.selected();
        let (b, a) = (self.settings.kf_before_ms, self.settings.kf_after_ms);
        let n = self.modify(|s| subforge_core::timing::snap_to_keyframes(s, &kfs, b, a, &subforge_core::timing::Scope::Indices(v)));
        self.buf_for = None;
        self.flash(format!("Snapped {n} line(s) to keyframes"));
    }
    pub fn seek_keyframe(&mut self, forward: bool) {
        let Some(kfs) = self.keyframes() else { return };
        let t = self.video_time().unwrap_or(self.tl.playhead_hint);
        let k = if forward { subforge_core::timing::next_keyframe(&kfs, t) } else { subforge_core::timing::prev_keyframe(&kfs, t) };
        if let Some(k) = k {
            self.seek(k);
        }
    }
    pub fn snap_to_neighbors(&mut self) {
        // Snap end of current to start of next and start to end of previous (if gap < 1s)
        let Some(c) = self.cur else { return };
        self.modify(|s| {
            let (prev_end, next_start) = (c.checked_sub(1).map(|p| s.events[p].end), s.events.get(c + 1).map(|n| n.start));
            let e = &mut s.events[c];
            if let Some(pe) = prev_end {
                if (e.start - pe).abs() < 1000 {
                    e.start = pe;
                }
            }
            if let Some(ns) = next_start {
                if (ns - e.end).abs() < 1000 {
                    e.end = ns;
                }
            }
        });
    }

    // ------------------------------------------------------------ shortcuts
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let sc = |m: Modifiers, k: Key| KeyboardShortcut::new(m, k);
        let cmd = Modifiers::COMMAND;
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        let mut actions: Vec<&'static str> = Vec::new();
        ctx.input_mut(|i| {
            let mut take = |s: KeyboardShortcut, name: &'static str| {
                if i.consume_shortcut(&s) {
                    actions.push(name);
                }
            };
            take(sc(cs, Key::S), "save_as");
            take(sc(cmd, Key::S), "save");
            take(sc(cmd, Key::O), "open");
            take(sc(cmd, Key::N), "new");
            take(sc(cs, Key::Z), "redo");
            take(sc(cmd, Key::Z), "undo");
            take(sc(cmd, Key::Y), "redo");
            take(sc(cmd, Key::F), "find");
            take(sc(cmd, Key::H), "replace");
            take(sc(cmd, Key::Enter), "commit_next");
            take(sc(cmd, Key::ArrowDown), "next");
            take(sc(cmd, Key::ArrowUp), "prev");
            take(sc(cmd, Key::OpenBracket), "set_start");
            take(sc(cmd, Key::CloseBracket), "set_end");
            take(sc(Modifiers::CTRL, Key::F7), "spell");
            take(sc(Modifiers::NONE, Key::F7), "set_start");
            take(sc(Modifiers::NONE, Key::F8), "set_end");
            take(sc(Modifiers::NONE, Key::F5), "play_line");
            take(sc(cmd, Key::P), "play_line");
            take(sc(cmd, Key::I), "insert_after");
            take(sc(cmd, Key::D), "duplicate");
            take(sc(cmd, Key::J), "merge");
            take(sc(cmd, Key::K), "split");
            take(sc(Modifiers::NONE, Key::F9), "check");
            take(sc(Modifiers::ALT, Key::Space), "toggle_play");
            take(sc(Modifiers::ALT, Key::ArrowLeft), "kf_prev");
            take(sc(Modifiers::ALT, Key::ArrowRight), "kf_next");
            take(sc(Modifiers::NONE, Key::F6), "snap_kf");
        });
        // keys that only apply when not typing
        if !ctx.egui_wants_keyboard_input() {
            ctx.input_mut(|i| {
                let mut take = |s: KeyboardShortcut, name: &'static str| {
                    if i.consume_shortcut(&s) {
                        actions.push(name);
                    }
                };
                take(sc(Modifiers::NONE, Key::Space), "toggle_play");
                take(sc(Modifiers::NONE, Key::Delete), "delete");
                take(sc(Modifiers::NONE, Key::ArrowDown), "next");
                take(sc(Modifiers::NONE, Key::ArrowUp), "prev");
                take(sc(Modifiers::SHIFT, Key::ArrowLeft), "seek_back_big");
                take(sc(Modifiers::SHIFT, Key::ArrowRight), "seek_fwd_big");
                take(sc(cmd, Key::ArrowLeft), "frame_back");
                take(sc(cmd, Key::ArrowRight), "frame_fwd");
                take(sc(Modifiers::NONE, Key::ArrowLeft), "seek_back");
                take(sc(Modifiers::NONE, Key::ArrowRight), "seek_fwd");
                take(sc(cmd, Key::A), "select_all");
                take(sc(Modifiers::NONE, Key::Enter), "focus_text");
                take(sc(Modifiers::NONE, Key::M), "move_tool");
                take(sc(Modifiers::NONE, Key::R), "rotate_tool");
                take(sc(Modifiers::NONE, Key::S), "scale_tool");
                take(sc(Modifiers::NONE, Key::C), "clip_tool");
                take(sc(Modifiers::NONE, Key::T), "track_tool");
                take(sc(Modifiers::NONE, Key::Escape), "pointer_tool");
            });
        }
        for a in actions {
            self.action(a, ctx);
        }
    }

    pub fn action(&mut self, a: &str, ctx: &egui::Context) {
        let step = self.settings.seek_step_ms;
        match a {
            "spell" => self.dialogs.spell.open = true,
            "save" => {
                self.save();
            }
            "save_as" => {
                self.save_as(None);
            }
            "open" => self.guard(Pending::Open(None), ctx),
            "new" => self.guard(Pending::New, ctx),
            "undo" => self.undo(),
            "redo" => self.redo(),
            "find" => {
                self.dialogs.find.open = true;
                self.dialogs.find.replace_mode = false;
                self.dialogs.find.focus = true;
            }
            "replace" => {
                self.dialogs.find.open = true;
                self.dialogs.find.replace_mode = true;
                self.dialogs.find.focus = true;
            }
            "commit_next" => {
                self.end_text_session();
                self.go_next(true);
                self.dialogs.focus_text = true;
            }
            "next" => self.go_next(false),
            "prev" => self.go_prev(),
            "set_start" => self.set_cur_time(true),
            "set_end" => self.set_cur_time(false),
            "play_line" => self.play_current(),
            "insert_after" => self.insert(true),
            "duplicate" => self.duplicate_selected(),
            "merge" => self.merge_selected(true),
            "split" => self.split_current(),
            "check" => self.show_pane(crate::layout::Pane::Issues),
            "rotate_tool" | "scale_tool" | "clip_tool" | "track_tool" | "pointer_tool" => {
                use crate::motion::VideoTool::*;
                let want = match a {
                    "rotate_tool" => Rotate,
                    "scale_tool" => Scale,
                    "clip_tool" => Clip,
                    "track_tool" => Track,
                    _ => Pointer,
                };
                self.vtool = if self.vtool == want { Pointer } else { want };
            }
            "move_tool" => {
                self.vtool = if self.vtool == crate::motion::VideoTool::Move { crate::motion::VideoTool::Pointer } else { crate::motion::VideoTool::Move };
            }
            "kf_prev" => self.seek_keyframe(false),
            "kf_next" => self.seek_keyframe(true),
            "snap_kf" => self.snap_selected_to_keyframes(),
            "toggle_play" => {
                if let Some(p) = &mut self.player {
                    p.toggle();
                }
            }
            "delete" => self.delete_selected(),
            "frame_back" => self.step_frame(-1),
            "frame_fwd" => self.step_frame(1),
            "seek_back" | "seek_fwd" | "seek_back_big" | "seek_fwd_big" => {
                if let Some(p) = &self.player {
                    let d = match a {
                        "seek_back" => -step,
                        "seek_fwd" => step,
                        "seek_back_big" => -step * 5,
                        _ => step * 5,
                    };
                    let t = p.time() + d;
                    self.seek(t);
                }
            }
            "select_all" => {
                self.sel = (0..self.doc.sub.events.len()).collect();
            }
            "focus_text" => self.dialogs.focus_text = true,
            _ => {}
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let t = format!("{}{} — {}", if self.doc.dirty { "* " } else { "" }, self.doc.name(), APP_NAME);
        if t != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(t.clone()));
            self.last_title = t;
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let files: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for p in files {
            if subforge_core::is_subtitle_path(&p) {
                if self.dialogs.batch.open {
                    self.dialogs.batch.files.push(p);
                } else {
                    self.guard(Pending::Open(Some(p)), ctx);
                }
            } else if media::is_media_path(&p) {
                // a container with subtitle tracks and no document yet -> offer import; else preview
                let has_doc = self.doc.path.is_some() || !self.doc.sub.events.is_empty();
                let container = p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mkv" | "mp4" | "m4v" | "ts" | "m2ts" | "webm" | "mov"));
                if container && !has_doc {
                    self.start_import_from_video(&p, true);
                } else {
                    self.start_open_video(&p);
                }
            }
        }
    }

    fn unsaved_modal(&mut self, ctx: &egui::Context) {
        let Some(p) = self.pending.clone() else { return };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("unsaved")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading("Unsaved changes");
            ui.label(format!("Save changes to \"{}\"?", self.doc.name()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    choice = Some(0);
                }
                if ui.button("Don't save").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(2);
                }
            });
        });
        match choice {
            Some(0) => {
                if self.save() {
                    self.pending = None;
                    self.run_pending(p, ctx);
                }
            }
            Some(1) => {
                self.pending = None;
                self.doc.dirty = false;
                self.run_pending(p, ctx);
            }
            Some(2) => self.pending = None,
            _ => {}
        }
    }
}

pub fn apply_theme(ctx: &egui::Context, s: &Settings) {
    ctx.set_theme(if s.dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.set_zoom_factor(s.ui_scale.clamp(0.5, 3.0));
}

pub(crate) fn find_sibling_video(sub: &Path) -> Option<PathBuf> {
    let dir = sub.parent()?;
    let stem = sub.file_stem()?.to_string_lossy().to_string();
    // "ep01.th.srt" -> also try "ep01"
    let mut stems = vec![stem.clone()];
    if let Some((base, _)) = stem.rsplit_once('.') {
        stems.push(base.to_string());
    }
    for s in stems {
        for ext in ["mkv", "mp4", "webm", "avi", "mov", "m4v", "ts"] {
            let p = dir.join(format!("{s}.{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(p) = &self.player {
            self.settings.volume = p.volume();
        }
        eframe::set_value(storage, "settings", &self.settings);
        if let Some(j) = self.tree.as_ref().and_then(crate::layout::save) {
            storage.set_string("layout_v1", j);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // close handling
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.doc.dirty {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }
        if let Some(p) = &mut self.player {
            p.update();
            if p.is_playing() {
                ctx.request_repaint();
            }
        }
        self.handle_drops(&ctx);
        self.shortcuts(&ctx);
        self.autosave();
        self.update_title(&ctx);
        self.fix_selection();

        self.poll_loading(&ctx);
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(4)).show(ui, |ui| {
            if let Some(mut tree) = self.tree.take() {
                let mut b = crate::layout::Behavior { app: self };
                tree.ui(&mut b, ui);
                self.tree = Some(tree);
            }
        });

        self.show_dialogs(&ctx);
        self.tools_dialogs(&ctx);
        self.fonts_dialog(&ctx);
        self.speech_dialog(&ctx);
        self.ocr_dialog(&ctx);
        self.spell_dialog(&ctx);
        self.fix_errors_dialog(&ctx);
        self.unsaved_modal(&ctx);
        self.loading_modal(&ctx);
    }
}
