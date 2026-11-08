//! Video/audio preview via an external `ffmpeg`/`ffprobe` (no linking, any codec ffmpeg supports).
//!
//! * Video: ffmpeg decodes scaled RGBA frames into a pipe; a worker thread paces them against the clock.
//! * Audio: ffmpeg decodes f32 PCM, played with cpal; the number of samples played drives the clock.
//! * Waveform: a background ffmpeg pass produces 8 kHz mono peaks for the timeline.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Build a command that never pops up a console window on Windows.
pub fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Where to find ffmpeg / ffprobe.
#[derive(Clone, Debug)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Tools {
    /// `dir` = user-configured folder (may be empty). Falls back to the exe folder, then PATH.
    pub fn locate(dir: &str) -> Tools {
        let exe = |n: &str| if cfg!(windows) { format!("{n}.exe") } else { n.to_string() };
        let mut cands: Vec<PathBuf> = Vec::new();
        if !dir.trim().is_empty() {
            cands.push(PathBuf::from(dir.trim()));
        }
        if let Ok(p) = std::env::current_exe() {
            if let Some(d) = p.parent() {
                cands.push(d.to_path_buf());
                cands.push(d.join("ffmpeg"));
                cands.push(d.join("ffmpeg").join("bin"));
            }
        }
        for d in cands {
            let f = d.join(exe("ffmpeg"));
            let p = d.join(exe("ffprobe"));
            if f.is_file() && p.is_file() {
                return Tools { ffmpeg: f, ffprobe: p };
            }
        }
        Tools { ffmpeg: PathBuf::from(exe("ffmpeg")), ffprobe: PathBuf::from(exe("ffprobe")) }
    }

    /// Whether this ffmpeg has the libass `subtitles` filter (for accurate previews).
    pub fn has_libass(&self) -> bool {
        command(&self.ffmpeg)
            .args(["-hide_banner", "-filters"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.split_whitespace().nth(1) == Some("subtitles")))
            .unwrap_or(false)
    }

    pub fn available(&self) -> bool {
        command(&self.ffmpeg).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
    }
}

#[derive(Clone, Debug, Default)]
pub struct MediaInfo {
    pub duration_ms: i64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_video: bool,
    pub has_audio: bool,
    /// Display aspect ratio (accounts for anamorphic SAR)
    pub dar: f64,
    /// Container start time (ms); timestamps are rebased so the first frame is 0
    pub start_ms: i64,
}

pub fn probe(tools: &Tools, path: &Path) -> anyhow::Result<MediaInfo> {
    let out = command(&tools.ffprobe)
        .args(["-v", "error", "-show_entries", "format=duration,start_time:stream=codec_type,width,height,avg_frame_rate,r_frame_rate,sample_aspect_ratio", "-of", "json"])
        .arg(path)
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        anyhow::bail!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let mut info = MediaInfo { fps: 23.976, dar: 16.0 / 9.0, ..Default::default() };
    let rate = |s: &str| -> Option<f64> {
        let (a, b) = s.split_once('/')?;
        let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
        (b > 0.0 && a > 0.0).then(|| a / b)
    };
    if let Some(streams) = v["streams"].as_array() {
        for s in streams {
            match s["codec_type"].as_str() {
                Some("video") if !info.has_video => {
                    info.has_video = true;
                    info.width = s["width"].as_u64().unwrap_or(0) as u32;
                    info.height = s["height"].as_u64().unwrap_or(0) as u32;
                    let fps = s["avg_frame_rate"].as_str().and_then(rate).or_else(|| s["r_frame_rate"].as_str().and_then(rate));
                    if let Some(f) = fps.filter(|f| *f > 1.0 && *f < 300.0) {
                        info.fps = f;
                    }
                    let sar = s["sample_aspect_ratio"].as_str().and_then(|x| {
                        let (a, b) = x.split_once(':')?;
                        let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
                        (a > 0.0 && b > 0.0).then(|| a / b)
                    });
                    if info.height > 0 {
                        info.dar = info.width as f64 / info.height as f64 * sar.unwrap_or(1.0);
                    }
                }
                Some("audio") => info.has_audio = true,
                _ => {}
            }
        }
    }
    info.start_ms = v["format"]["start_time"].as_str().and_then(|d| d.parse::<f64>().ok()).map(|d| (d * 1000.0).round() as i64).unwrap_or(0);
    info.duration_ms = v["format"]["duration"].as_str().and_then(|d| d.parse::<f64>().ok()).map(|d| (d * 1000.0) as i64).unwrap_or(0);
    if !info.has_video && !info.has_audio {
        anyhow::bail!("no audio or video streams found");
    }
    Ok(info)
}

// ---------------------------------------------------------------- clock

/// Playback clock. When audio is playing, the number of audio frames consumed drives it;
/// otherwise wall-clock time.
pub struct Clock {
    playing: AtomicBool,
    base_ms: AtomicI64,
    started: Mutex<Instant>,
    audio_frames: Arc<AtomicU64>,
    audio_rate: AtomicU32,
    use_audio: AtomicBool,
    speed_x100: AtomicU32,
}

impl Clock {
    fn new() -> Arc<Clock> {
        Arc::new(Clock {
            playing: AtomicBool::new(false),
            base_ms: AtomicI64::new(0),
            started: Mutex::new(Instant::now()),
            audio_frames: Arc::new(AtomicU64::new(0)),
            audio_rate: AtomicU32::new(48000),
            use_audio: AtomicBool::new(false),
            speed_x100: AtomicU32::new(100),
        })
    }
    pub fn now_ms(&self) -> i64 {
        let base = self.base_ms.load(Ordering::Relaxed);
        if !self.playing.load(Ordering::Relaxed) {
            return base;
        }
        if self.use_audio.load(Ordering::Relaxed) {
            let frames = self.audio_frames.load(Ordering::Relaxed);
            let rate = self.audio_rate.load(Ordering::Relaxed).max(1) as i64;
            base + frames as i64 * 1000 / rate
        } else {
            let speed = self.speed_x100.load(Ordering::Relaxed) as f64 / 100.0;
            base + (self.started.lock().unwrap().elapsed().as_secs_f64() * 1000.0 * speed) as i64
        }
    }
    fn start(&self, at: i64, use_audio: bool) {
        self.base_ms.store(at, Ordering::Relaxed);
        self.audio_frames.store(0, Ordering::Relaxed);
        *self.started.lock().unwrap() = Instant::now();
        self.use_audio.store(use_audio, Ordering::Relaxed);
        self.playing.store(true, Ordering::Relaxed);
    }
    fn stop_at(&self, at: i64) {
        self.playing.store(false, Ordering::Relaxed);
        self.base_ms.store(at, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------- video
//
// Seeking strategy (spawning ffmpeg costs ~200-350 ms, decoding one more frame ~3-10 ms):
// * every decoded frame goes into an LRU frame cache  -> revisiting / stepping back is instant
// * after a paused seek the decoder process is kept open and positioned
//   -> stepping forward / short forward seeks just read on, no new process
// * while idle the open decoder prefetches a few frames ahead
// * a fresh seek starts a few frames early (or a window behind when going backwards)
//   so the next step in either direction is already cached
// Frames are decoded straight into `Vec<Color32>` and shared as `Arc<ColorImage>`,
// so nothing is converted or copied on the UI thread (egui takes the Arc as-is).

use eframe::egui::{Color32, ColorImage};

pub struct Frame {
    /// frame number (time = idx * frame duration)
    pub idx: i64,
    pub image: Arc<ColorImage>,
    /// Some(doc generation) when rendered with libass (accurate subtitle preview)
    pub rendered: Option<u64>,
}

/// Burn subtitles in with libass while playing (exact ASS rendering).
#[derive(Clone, Debug, PartialEq)]
pub struct PlaySubs {
    /// work dir containing `preview.ass` and `fonts/`
    pub dir: PathBuf,
    pub start_offset_ms: i64,
    /// original video size (libass scales borders/blur relative to it)
    pub orig_w: u32,
    pub orig_h: u32,
}

enum VCmd {
    Seek(i64),
    Play(i64, Option<PlaySubs>),
    Pause,
    /// libass render of frame at time t for document generation `gen`; files live in `dir`
    Render { t: i64, gen: u64, dir: PathBuf, start_offset_ms: i64 },
    Quit,
}

/// Frame timing helpers shared by the UI and the worker.
#[derive(Clone, Copy, Debug)]
pub struct FrameClock {
    pub frame_ms: f64,
}
impl FrameClock {
    /// Frame displayed at time `t` (ms). The +1 tolerates container timestamps rounded to 1 ms.
    pub fn idx_of(&self, t: i64) -> i64 {
        ((t.max(0) as f64 + 1.0) / self.frame_ms).floor() as i64
    }
    pub fn time_of(&self, idx: i64) -> i64 {
        (idx as f64 * self.frame_ms).round() as i64
    }
    /// ffmpeg input `-ss` (seconds) that yields exactly frame `idx` (ffmpeg returns the first frame with pts >= ss).
    fn seek_arg(&self, idx: i64) -> String {
        format!("{:.4}", ((idx as f64 - 0.5) * self.frame_ms).max(0.0) / 1000.0)
    }
}

struct FrameCache {
    map: std::collections::HashMap<i64, Arc<ColorImage>>,
    order: VecDeque<i64>,
    cap: usize,
}
impl FrameCache {
    fn new(frame_bytes: usize) -> Self {
        // ~256 MB budget, at least a second or so of frames
        let cap = (256 * 1024 * 1024 / frame_bytes.max(1)).clamp(24, 400);
        FrameCache { map: Default::default(), order: VecDeque::new(), cap }
    }
    fn get(&mut self, idx: i64) -> Option<Arc<ColorImage>> {
        let f = self.map.get(&idx).cloned()?;
        if let Some(p) = self.order.iter().position(|&i| i == idx) {
            self.order.remove(p);
        }
        self.order.push_back(idx);
        Some(f)
    }
    fn put(&mut self, idx: i64, img: Arc<ColorImage>) {
        if self.map.insert(idx, img).is_none() {
            self.order.push_back(idx);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }
    fn contains(&self, idx: i64) -> bool {
        self.map.contains_key(&idx)
    }
}

struct Decoder {
    child: Child,
    out: std::process::ChildStdout,
    next_idx: i64,
}
impl Decoder {
    fn spawn(tools: &Tools, path: &Path, fc: FrameClock, idx: i64, w: u32, h: u32, subs: Option<&PlaySubs>) -> std::io::Result<Decoder> {
        let mut c = command(&tools.ffmpeg);
        c.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        let vf = match subs {
            Some(s) => {
                // keep real timestamps so libass sees the right time after seeking
                c.current_dir(&s.dir);
                c.arg("-copyts");
                let shift = if s.start_offset_ms != 0 { format!("setpts=PTS-{:.3}/TB,", s.start_offset_ms as f64 / 1000.0) } else { String::new() };
                format!("{shift}scale={w}:{h}:flags=bilinear,setsar=1,subtitles=preview.ass:fontsdir=fonts:original_size={}x{}", s.orig_w, s.orig_h)
            }
            None => format!("scale={w}:{h}:flags=bilinear,setsar=1"),
        };
        c.args(["-ss", &fc.seek_arg(idx)]);
        c.arg("-i").arg(path);
        c.args(["-map", "0:v:0", "-an", "-sn", "-dn"]);
        c.args(["-vf", &vf, "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"]);
        let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        let out = child.stdout.take().expect("piped stdout");
        Ok(Decoder { child, out, next_idx: idx })
    }
    /// Read the next frame into a fresh pixel buffer.
    fn read(&mut self, w: usize, h: usize) -> Option<(i64, Arc<ColorImage>)> {
        let mut px = vec![Color32::BLACK; w * h];
        if !read_exact_or_eof(&mut self.out, bytemuck::cast_slice_mut(&mut px)) {
            return None;
        }
        let i = self.next_idx;
        self.next_idx += 1;
        Some((i, Arc::new(ColorImage::new([w, h], px))))
    }
    /// Read and throw away one frame (reuses `scratch`, no allocation).
    fn skip(&mut self, scratch: &mut [u8]) -> bool {
        let ok = read_exact_or_eof(&mut self.out, scratch);
        if ok {
            self.next_idx += 1;
        }
        ok
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_exact_or_eof(r: &mut impl Read, buf: &mut [u8]) -> bool {
    let mut off = 0;
    while off < buf.len() {
        match r.read(&mut buf[off..]) {
            Ok(0) => return false,
            Ok(n) => off += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

/// How far a paused decoder may read forward instead of re-seeking (frames).
const READ_ON_LIMIT: i64 = 72;
/// Frames prefetched ahead of the shown frame while paused.
const PREFETCH: i64 = 12;
/// Frames decoded before the target on a fresh seek (makes the first step back instant).
const LEAD_IN: i64 = 4;
/// Window decoded when seeking backwards to an uncached frame.
const BACK_WINDOW: i64 = 24;

struct VideoWorker {
    tools: Tools,
    path: PathBuf,
    fc: FrameClock,
    w: u32,
    h: u32,
    max_idx: i64,
    tx: Sender<Frame>,
    clock: Arc<Clock>,
    repaint: Box<dyn Fn() + Send>,
    status: Arc<Mutex<String>>,
    cache: FrameCache,
    paused: Option<Decoder>,
    playing: Option<Decoder>,
    shown: i64,
    scratch: Vec<u8>,
    stats: Arc<VideoStats>,
}

/// Counters shown in the UI (debug overlay) to verify cache behaviour.
#[derive(Default)]
pub struct VideoStats {
    pub spawns: AtomicU64,
    pub cache_hits: AtomicU64,
    pub last_seek_us: AtomicU64,
}

impl VideoWorker {
    fn send(&self, idx: i64, image: Arc<ColorImage>, rendered: Option<u64>) {
        let _ = self.tx.send(Frame { idx, image, rendered });
        (self.repaint)();
    }

    fn spawn(&mut self, idx: i64) -> Option<Decoder> {
        self.spawn_with(idx, None)
    }
    fn spawn_with(&mut self, idx: i64, subs: Option<&PlaySubs>) -> Option<Decoder> {
        self.stats.spawns.fetch_add(1, Ordering::Relaxed);
        match Decoder::spawn(&self.tools, &self.path, self.fc, idx.clamp(0, self.max_idx), self.w, self.h, subs) {
            Ok(d) => Some(d),
            Err(e) => {
                *self.status.lock().unwrap() = format!("ffmpeg: {e}");
                None
            }
        }
    }

    /// Read frames from the paused decoder into the cache until `target` (inclusive) is decoded.
    fn read_to(&mut self, target: i64) -> bool {
        let (w, h) = (self.w as usize, self.h as usize);
        loop {
            let Some(d) = self.paused.as_mut() else { return false };
            if d.next_idx > target {
                return true;
            }
            match d.read(w, h) {
                Some((i, img)) => self.cache.put(i, img),
                None => {
                    // end of stream
                    self.paused = None;
                    return false;
                }
            }
        }
    }

    fn seek(&mut self, t: i64) {
        let started = Instant::now();
        self.playing = None;
        let idx = self.fc.idx_of(t).min(self.max_idx);
        let backwards = idx < self.shown;
        self.shown = idx;
        if let Some(img) = self.cache.get(idx) {
            self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
            self.send(idx, img, None);
        } else {
            let reuse = self.paused.as_ref().is_some_and(|d| idx >= d.next_idx && idx - d.next_idx <= READ_ON_LIMIT);
            if !reuse {
                let start = if backwards { idx - BACK_WINDOW + 1 } else { idx - LEAD_IN };
                // don't re-decode frames we already have just before the target
                let mut s = start.max(0);
                while s < idx && self.cache.contains(s) {
                    s += 1;
                }
                self.paused = self.spawn(s);
            }
            self.read_to(idx);
            match self.cache.get(idx) {
                Some(img) => self.send(idx, img, None),
                None => {
                    // past the last decodable frame: show the last one we have
                    if let Some((&i, img)) = self.cache.map.iter().filter(|(i, _)| **i <= idx).max_by_key(|(i, _)| **i) {
                        let (i, img) = (i, img.clone());
                        self.send(i, img, None);
                    }
                }
            }
        }
        self.stats.last_seek_us.store(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    }

    /// One prefetch step while paused. Returns false when there is nothing to do.
    fn prefetch_step(&mut self) -> bool {
        let shown = self.shown;
        let Some(d) = self.paused.as_mut() else { return false };
        if d.next_idx > shown + PREFETCH || d.next_idx > self.max_idx {
            return false;
        }
        let (w, h) = (self.w as usize, self.h as usize);
        match d.read(w, h) {
            Some((i, img)) => {
                self.cache.put(i, img);
                true
            }
            None => {
                self.paused = None;
                false
            }
        }
    }

    fn render(&mut self, t: i64, gen: u64, dir: &Path, start_offset_ms: i64) {
        let idx = self.fc.idx_of(t).min(self.max_idx);
        let mut c = command(&self.tools.ffmpeg);
        c.current_dir(dir); // relative paths in the filter graph: no escaping trouble on Windows
        c.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-copyts", "-ss", &self.fc.seek_arg(idx)]);
        c.arg("-i").arg(&self.path);
        let shift = if start_offset_ms != 0 { format!("setpts=PTS-{:.3}/TB,", start_offset_ms as f64 / 1000.0) } else { String::new() };
        let vf = format!("{shift}subtitles=preview.ass:fontsdir=fonts,scale={}:{}:flags=bicubic,setsar=1", self.w, self.h);
        c.args(["-map", "0:v:0", "-an", "-sn", "-dn", "-frames:v", "1", "-vf", &vf, "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"]);
        let Ok(mut child) = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() else { return };
        let (w, h) = (self.w as usize, self.h as usize);
        let mut px = vec![Color32::BLACK; w * h];
        let ok = child.stdout.as_mut().map(|o| read_exact_or_eof(o, bytemuck::cast_slice_mut(&mut px))).unwrap_or(false);
        if !ok {
            let mut err = String::new();
            if let Some(e) = child.stderr.as_mut() {
                let _ = e.read_to_string(&mut err);
            }
            if !err.trim().is_empty() {
                *self.status.lock().unwrap() = format!("libass preview: {}", err.lines().next().unwrap_or(""));
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        if ok && self.playing.is_none() {
            self.send(idx, Arc::new(ColorImage::new([w, h], px)), Some(gen));
        }
    }

    fn play(&mut self, t: i64, subs: Option<PlaySubs>) {
        self.paused = None;
        let idx = self.fc.idx_of(t).min(self.max_idx);
        self.playing = self.spawn_with(idx, subs.as_ref());
    }

    /// Decode, pace and show the next playback frame. Returns a command that arrived while waiting.
    fn play_step(&mut self, rx: &Receiver<VCmd>) -> Option<VCmd> {
        let fc = self.fc;
        let (w, h) = (self.w as usize, self.h as usize);
        let Some(d) = self.playing.as_mut() else { return None };
        let idx = d.next_idx;
        let pts = fc.time_of(idx);
        let late_ms = (fc.frame_ms * 1.5) as i64;
        // already late: skip without allocating
        if self.clock.now_ms() - pts > late_ms {
            if !d.skip(&mut self.scratch) {
                self.playing = None;
            }
            return None;
        }
        let Some((i, img)) = d.read(w, h) else {
            self.playing = None;
            return None;
        };
        loop {
            let now = self.clock.now_ms();
            if now + 2 >= pts {
                break;
            }
            std::thread::sleep(Duration::from_millis(((pts - now) as u64).min(10)));
            if let Ok(c) = rx.try_recv() {
                return Some(c);
            }
        }
        if self.clock.now_ms() - pts <= late_ms {
            self.shown = i;
            self.send(i, img, None);
        }
        None
    }

    fn run(mut self, rx: Receiver<VCmd>) {
        let mut pending: Option<VCmd> = None;
        loop {
            let cmd = if let Some(c) = pending.take() {
                Some(c)
            } else if self.playing.is_some() {
                match rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(VCmd::Quit),
                }
            } else {
                // paused: prefetch while no command is waiting
                match rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Disconnected) => Some(VCmd::Quit),
                    Err(TryRecvError::Empty) => {
                        if self.prefetch_step() {
                            continue;
                        }
                        match rx.recv() {
                            Ok(c) => Some(c),
                            Err(_) => Some(VCmd::Quit),
                        }
                    }
                }
            };
            match cmd {
                Some(VCmd::Quit) => return,
                Some(VCmd::Pause) => self.playing = None,
                Some(VCmd::Seek(mut t)) => {
                    // coalesce rapid seeks (scrubbing): only decode the latest
                    while let Ok(c) = rx.try_recv() {
                        match c {
                            VCmd::Seek(t2) => t = t2,
                            other => {
                                pending = Some(other);
                                break;
                            }
                        }
                    }
                    self.seek(t);
                }
                Some(VCmd::Play(t, subs)) => self.play(t, subs),
                Some(VCmd::Render { t, gen, dir, start_offset_ms }) => {
                    // only the latest render request matters
                    let (mut t, mut gen, mut dir) = (t, gen, dir);
                    while let Ok(c) = rx.try_recv() {
                        match c {
                            VCmd::Render { t: t2, gen: g2, dir: d2, .. } => {
                                t = t2;
                                gen = g2;
                                dir = d2;
                            }
                            other => {
                                pending = Some(other);
                                break;
                            }
                        }
                    }
                    if pending.is_none() {
                        self.render(t, gen, &dir, start_offset_ms);
                    }
                }
                None => pending = self.play_step(&rx),
            }
        }
    }
}

// ---------------------------------------------------------------- audio

struct AudioShared {
    buf: Mutex<VecDeque<f32>>,
    eof: AtomicBool,
    generation: AtomicU64,
    volume: AtomicU32,
}

pub struct AudioOut {
    _stream: cpal::Stream,
    shared: Arc<AudioShared>,
    pub rate: u32,
    pub channels: u16,
    feeder: Option<std::thread::JoinHandle<()>>,
    feeder_child: Arc<Mutex<Option<Child>>>,
}

impl AudioOut {
    fn new(frames_counter: Arc<AtomicU64>) -> anyhow::Result<AudioOut> {
        let host = cpal::default_host();
        let dev = host.default_output_device().ok_or_else(|| anyhow::anyhow!("no audio output device"))?;
        let sup = dev.default_output_config()?;
        let mut cfg = sup.config();
        cfg.channels = cfg.channels.clamp(1, 8);
        let channels = cfg.channels;
        let rate = cfg.sample_rate;
        let shared = Arc::new(AudioShared {
            buf: Mutex::new(VecDeque::with_capacity(rate as usize * channels as usize)),
            eof: AtomicBool::new(true),
            generation: AtomicU64::new(0),
            volume: AtomicU32::new(1.0f32.to_bits()),
        });
        let sh = shared.clone();
        let fc = frames_counter.clone();
        let ch = channels as usize;
        let err = |e| eprintln!("audio stream error: {e}");
        let stream = match sup.sample_format() {
            cpal::SampleFormat::I16 => dev.build_output_stream::<i16, _, _>(
                cfg.clone(),
                move |data: &mut [i16], _| {
                    let mut tmp = vec![0f32; data.len()];
                    fill(&sh, &fc, &mut tmp, ch);
                    for (d, s) in data.iter_mut().zip(tmp) {
                        *d = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                },
                err,
                None,
            )?,
            _ => dev.build_output_stream::<f32, _, _>(cfg.clone(), move |data: &mut [f32], _| fill(&sh, &fc, data, ch), err, None)?,
        };
        stream.play()?;
        Ok(AudioOut { _stream: stream, shared, rate, channels, feeder: None, feeder_child: Arc::new(Mutex::new(None)) })
    }

    fn stop(&mut self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(mut c) = self.feeder_child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(h) = self.feeder.take() {
            let _ = h.join();
        }
        self.shared.buf.lock().unwrap().clear();
        self.shared.eof.store(true, Ordering::SeqCst);
    }

    fn start(&mut self, tools: &Tools, path: &Path, at_ms: i64) {
        self.stop();
        self.shared.buf.lock().unwrap().clear();
        self.shared.eof.store(false, Ordering::SeqCst);
        let gen = self.shared.generation.load(Ordering::SeqCst);
        let mut c = command(&tools.ffmpeg);
        c.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss", &format!("{:.3}", at_ms.max(0) as f64 / 1000.0)]);
        c.arg("-i").arg(path);
        c.args(["-map", "0:a:0", "-vn", "-sn", "-ac", &self.channels.to_string(), "-ar", &self.rate.to_string(), "-f", "f32le", "pipe:1"]);
        let child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
        let Ok(mut child) = child else {
            self.shared.eof.store(true, Ordering::SeqCst);
            return;
        };
        let mut out = child.stdout.take().unwrap();
        *self.feeder_child.lock().unwrap() = Some(child);
        let sh = self.shared.clone();
        let max_buffered = (self.rate as usize * self.channels as usize) / 2; // ~0.5 s
        self.feeder = Some(std::thread::spawn(move || {
            let mut bytes = vec![0u8; 16384];
            let mut carry: Vec<u8> = Vec::new();
            loop {
                if sh.generation.load(Ordering::SeqCst) != gen {
                    return;
                }
                if sh.buf.lock().unwrap().len() > max_buffered {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                match out.read(&mut bytes) {
                    Ok(0) | Err(_) => {
                        sh.eof.store(true, Ordering::SeqCst);
                        return;
                    }
                    Ok(n) => {
                        carry.extend_from_slice(&bytes[..n]);
                        let whole = carry.len() / 4 * 4;
                        let mut b = sh.buf.lock().unwrap();
                        for c in carry[..whole].chunks_exact(4) {
                            b.push_back(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                        }
                        drop(b);
                        carry.drain(..whole);
                    }
                }
            }
        }));
    }
}

fn fill(sh: &AudioShared, frames: &AtomicU64, data: &mut [f32], ch: usize) {
    let vol = f32::from_bits(sh.volume.load(Ordering::Relaxed));
    let mut b = sh.buf.lock().unwrap();
    let eof = sh.eof.load(Ordering::Relaxed);
    let mut played = 0u64;
    for frame in data.chunks_mut(ch) {
        if b.len() >= ch {
            for s in frame.iter_mut() {
                *s = b.pop_front().unwrap_or(0.0) * vol;
            }
            played += 1;
        } else {
            for s in frame.iter_mut() {
                *s = 0.0;
            }
            // after EOF keep the clock running on silence; before EOF it's an underrun (clock waits)
            if eof {
                played += 1;
            }
        }
    }
    frames.fetch_add(played, Ordering::Relaxed);
}

// ---------------------------------------------------------------- waveform

/// Peak amplitude (0..=255) per 10 ms bucket, filled progressively by a background thread
/// (or instantly from the disk cache).
pub struct Waveform {
    pub peaks: Arc<Mutex<Vec<u8>>>,
    pub done: Arc<AtomicBool>,
    pub from_cache: bool,
    cancel: Arc<AtomicBool>,
}

pub const WAVE_BUCKET_MS: i64 = 10;
const WAVE_MAGIC: &[u8; 4] = b"SFW1";
const KF_MAGIC: &[u8; 4] = b"SFK1";

impl Waveform {
    fn start(tools: &Tools, path: &Path, key: Option<String>, repaint: Box<dyn Fn() + Send>) -> Waveform {
        let done = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(cached) = key.as_deref().and_then(|k| crate::cache::load(k, "wave", WAVE_MAGIC)) {
            done.store(true, Ordering::SeqCst);
            return Waveform { peaks: Arc::new(Mutex::new(cached)), done, from_cache: true, cancel };
        }
        let peaks = Arc::new(Mutex::new(Vec::new()));
        let (p, d, cn) = (peaks.clone(), done.clone(), cancel.clone());
        let mut c = command(&tools.ffmpeg);
        c.args(["-hide_banner", "-loglevel", "error", "-nostdin"]).arg("-i").arg(path);
        c.args(["-map", "0:a:0", "-vn", "-sn", "-ac", "1", "-ar", "8000", "-f", "s16le", "pipe:1"]);
        let child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
        std::thread::spawn(move || {
            let Ok(mut child) = child else {
                d.store(true, Ordering::SeqCst);
                return;
            };
            let mut out = child.stdout.take().unwrap();
            const PER_BUCKET: usize = 80; // 8000 Hz * 10 ms
            let mut buf = vec![0u8; PER_BUCKET * 2 * 400];
            let mut carry: Vec<u8> = Vec::new();
            let mut last_repaint = Instant::now();
            let mut complete = false;
            loop {
                if cn.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    break;
                }
                match out.read(&mut buf) {
                    Ok(0) => {
                        complete = true;
                        break;
                    }
                    Err(_) => break,
                    Ok(n) => {
                        carry.extend_from_slice(&buf[..n]);
                        let whole = carry.len() / (PER_BUCKET * 2) * (PER_BUCKET * 2);
                        let local: Vec<u8> = carry[..whole]
                            .chunks_exact(PER_BUCKET * 2)
                            .map(|chunk| {
                                let m = chunk.chunks_exact(2).map(|s| (i16::from_le_bytes([s[0], s[1]]) as i32).abs()).max().unwrap_or(0);
                                // mild compression so quiet dialogue is visible
                                ((m as f32 / 32768.0).sqrt() * 255.0).min(255.0) as u8
                            })
                            .collect();
                        carry.drain(..whole);
                        p.lock().unwrap().extend(local);
                        if last_repaint.elapsed() > Duration::from_millis(250) {
                            repaint();
                            last_repaint = Instant::now();
                        }
                    }
                }
            }
            let status_ok = child.wait().map(|s| s.success()).unwrap_or(false);
            if complete && status_ok {
                if let Some(k) = key {
                    crate::cache::save(&k, "wave", WAVE_MAGIC, &p.lock().unwrap());
                }
            }
            d.store(true, Ordering::SeqCst);
            repaint();
        });
        Waveform { peaks, done, from_cache: false, cancel }
    }
}

impl Drop for Waveform {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------- spectrum

/// Mel spectrogram (`subforge_core::audio::ROWS` bytes per 10 ms column), built on demand in the
/// background and cached on disk like the waveform.
pub struct Spectrum {
    pub data: Arc<Mutex<Vec<u8>>>,
    pub done: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

const SPEC_MAGIC: &[u8; 4] = b"SFS1";

impl Spectrum {
    fn start(tools: &Tools, path: &Path, key: Option<String>, repaint: Box<dyn Fn() + Send>) -> Spectrum {
        use subforge_core::audio;
        let done = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(cached) = key.as_deref().and_then(|k| crate::cache::load(k, "spec", SPEC_MAGIC)) {
            done.store(true, Ordering::SeqCst);
            return Spectrum { data: Arc::new(Mutex::new(cached)), done, cancel };
        }
        let data = Arc::new(Mutex::new(Vec::new()));
        let (d, dn, cn) = (data.clone(), done.clone(), cancel.clone());
        let ffmpeg = tools.ffmpeg.clone();
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let mut b = audio::SpecBuilder::new();
            let mut last = Instant::now();
            let mut flushed = 0;
            let r = audio::decode_16k(&ffmpeg, &path, 0, |s| {
                b.push(s);
                if last.elapsed() > Duration::from_millis(300) {
                    d.lock().unwrap().extend_from_slice(&b.data[flushed..]);
                    flushed = b.data.len();
                    repaint();
                    last = Instant::now();
                }
                !cn.load(Ordering::Relaxed)
            });
            b.finish();
            {
                let mut g = d.lock().unwrap();
                g.extend_from_slice(&b.data[flushed..]);
                if r.is_ok() {
                    if let Some(k) = key {
                        crate::cache::save(&k, "spec", SPEC_MAGIC, &g);
                    }
                }
            }
            dn.store(true, Ordering::SeqCst);
            repaint();
        });
        Spectrum { data, done, cancel }
    }
}

impl Drop for Spectrum {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------- keyframes

/// Keyframe times (ms, sorted). `None` while still loading.
pub type Keyframes = Arc<Mutex<Option<Arc<Vec<i64>>>>>;

/// Read keyframe positions from packet flags (demux only, no decoding: fast even for long files).
fn load_keyframes(tools: &Tools, path: &Path, key: Option<String>, start_ms: i64, duration_ms: i64, out: Keyframes, progress: Arc<AtomicU32>, repaint: Box<dyn Fn() + Send>) {
    if let Some(data) = key.as_deref().and_then(|k| crate::cache::load(k, "kf", KF_MAGIC)) {
        let v: Vec<i64> = data.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect();
        *out.lock().unwrap() = Some(Arc::new(v));
        progress.store(1000, Ordering::Relaxed);
        return;
    }
    let mut c = command(&tools.ffprobe);
    c.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time,flags", "-of", "csv=p=0"]).arg(path);
    c.stdin(Stdio::null()).stderr(Stdio::null()).stdout(Stdio::piped());
    std::thread::spawn(move || {
        let Ok(mut child) = c.spawn() else {
            *out.lock().unwrap() = Some(Arc::new(Vec::new()));
            return;
        };
        let mut v: Vec<i64> = Vec::new();
        let reader = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut n = 0u64;
        for l in std::io::BufRead::lines(reader).map_while(Result::ok) {
            let Some((t, flags)) = l.split_once(',') else { continue };
            let Ok(sec) = t.trim().parse::<f64>() else { continue };
            let ms = (sec * 1000.0).round() as i64 - start_ms;
            n += 1;
            if n % 512 == 0 && duration_ms > 0 {
                progress.store(((ms as f64 / duration_ms as f64).clamp(0.0, 0.999) * 1000.0) as u32, Ordering::Relaxed);
            }
            if flags.contains('K') {
                v.push(ms);
            }
        }
        let ok = child.wait().map(|s| s.success()).unwrap_or(false);
        v.sort_unstable();
        v.dedup();
        if ok {
            if let Some(k) = key {
                let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
                crate::cache::save(&k, "kf", KF_MAGIC, &bytes);
            }
        }
        progress.store(1000, Ordering::Relaxed);
        *out.lock().unwrap() = Some(Arc::new(v));
        repaint();
    });
}

/// Extract fonts embedded in the video (mkv attachments) so libass previews use them.
fn extract_fonts(tools: &Tools, path: &Path, work: &Path, ready: Arc<AtomicBool>) {
    let fonts = work.join("fonts");
    if fonts.join(".done").exists() {
        ready.store(true, Ordering::Relaxed);
        return;
    }
    if std::fs::create_dir_all(&fonts).is_err() {
        ready.store(true, Ordering::Relaxed);
        return;
    }
    let mut c = command(&tools.ffmpeg);
    c.current_dir(&fonts);
    c.args(["-hide_banner", "-loglevel", "quiet", "-nostdin", "-y", "-dump_attachment:t", ""]).arg("-i").arg(path);
    c.args(["-t", "0", "-f", "null", "-"]);
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    std::thread::spawn(move || {
        let _ = c.status();
        let _ = std::fs::write(fonts.join(".done"), b"");
        ready.store(true, Ordering::Relaxed);
    });
}

// ---------------------------------------------------------------- player

pub struct WarmStatus {
    pub first_frame: bool,
    pub keyframes: bool,
    pub keyframe_progress: f32,
    pub keyframe_count: Option<usize>,
    pub fonts: bool,
    pub waveform: bool,
    pub waveform_progress: f32,
    pub waveform_cached: bool,
}

impl WarmStatus {
    pub fn all_done(&self) -> bool {
        self.first_frame && self.keyframes && self.fonts && self.waveform
    }
}

pub struct Player {
    pub path: PathBuf,
    pub info: MediaInfo,
    pub out_w: u32,
    pub out_h: u32,
    pub fc: FrameClock,
    tools: Tools,
    vtx: Option<Sender<VCmd>>,
    frx: Option<Receiver<Frame>>,
    pub clock: Arc<Clock>,
    audio: Option<AudioOut>,
    pub waveform: Option<Waveform>,
    pub spectrum: Option<Spectrum>,
    pub keyframes: Keyframes,
    pub kf_progress: Arc<AtomicU32>,
    /// When set, playback frames are rendered with libass (exact ASS during playback)
    pub play_subs: Option<PlaySubs>,
    pub fonts_ready: Arc<AtomicBool>,
    pub stats: Arc<VideoStats>,
    /// Latest plain decoded frame
    pub plain: Option<Frame>,
    /// libass-rendered version of `plain` (same frame index), if any
    pub rendered: Option<Frame>,
    pub status: Arc<Mutex<String>>,
    /// Stop automatically at this time (play-line / play-range)
    pub stop_at: Option<i64>,
    /// Per-video work dir (extracted fonts + libass preview script); None if uncachable
    pub work_dir: Option<PathBuf>,
    volume: f32,
}

impl Player {
    /// Open with a probe result computed elsewhere (e.g. on a loader thread).
    pub fn open_probed(tools: &Tools, path: &Path, info: MediaInfo, ctx: &eframe::egui::Context, max_width: u32) -> anyhow::Result<Player> {
        // absolute path: some ffmpeg runs use the cache folder as working directory
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let path = abs.as_path();
        let clock = Clock::new();
        let status = Arc::new(Mutex::new(String::new()));
        let key = crate::cache::key(path);
        let fc = FrameClock { frame_ms: 1000.0 / info.fps };
        let stats = Arc::new(VideoStats::default());
        let (mut out_w, mut out_h) = (0, 0);
        let (mut vtx, mut frx) = (None, None);
        if info.has_video && info.width > 0 {
            let disp_w = (info.height as f64 * info.dar).round() as u32;
            out_w = disp_w.min(max_width).max(16) & !1;
            out_h = ((out_w as f64 / info.dar).round() as u32).max(16) & !1;
            let (ctx_tx, ctx_rx) = mpsc::channel();
            let (ftx, f_rx) = mpsc::channel();
            let c2 = ctx.clone();
            let frame_bytes = out_w as usize * out_h as usize * 4;
            let worker = VideoWorker {
                tools: tools.clone(),
                path: path.to_path_buf(),
                fc,
                w: out_w,
                h: out_h,
                max_idx: fc.idx_of(info.duration_ms).saturating_sub(1).max(0),
                tx: ftx,
                clock: clock.clone(),
                repaint: Box::new(move || c2.request_repaint()),
                status: status.clone(),
                cache: FrameCache::new(frame_bytes),
                paused: None,
                playing: None,
                shown: 0,
                scratch: vec![0u8; frame_bytes],
                stats: stats.clone(),
            };
            std::thread::Builder::new().name("video".into()).spawn(move || worker.run(ctx_rx))?;
            vtx = Some(ctx_tx);
            frx = Some(f_rx);
        }
        let audio = if info.has_audio {
            match AudioOut::new(clock.audio_frames.clone()) {
                Ok(a) => {
                    clock.audio_rate.store(a.rate, Ordering::Relaxed);
                    Some(a)
                }
                Err(e) => {
                    *status.lock().unwrap() = format!("audio disabled: {e}");
                    None
                }
            }
        } else {
            None
        };
        let waveform = info.has_audio.then(|| {
            let c2 = ctx.clone();
            Waveform::start(tools, path, key.clone(), Box::new(move || c2.request_repaint()))
        });
        let keyframes: Keyframes = Arc::new(Mutex::new(None));
        let kf_progress = Arc::new(AtomicU32::new(0));
        if info.has_video {
            let c2 = ctx.clone();
            load_keyframes(tools, path, key.clone(), info.start_ms, info.duration_ms, keyframes.clone(), kf_progress.clone(), Box::new(move || c2.request_repaint()));
        } else {
            *keyframes.lock().unwrap() = Some(Arc::new(Vec::new()));
            kf_progress.store(1000, Ordering::Relaxed);
        }
        let work_dir = key.as_deref().map(crate::cache::work_dir);
        let fonts_ready = Arc::new(AtomicBool::new(!info.has_video || work_dir.is_none()));
        if let Some(w) = &work_dir {
            if info.has_video {
                extract_fonts(tools, path, w, fonts_ready.clone());
            }
        }
        let mut p = Player {
            path: path.to_path_buf(),
            info,
            out_w,
            out_h,
            fc,
            tools: tools.clone(),
            vtx,
            frx,
            clock,
            audio,
            waveform,
            spectrum: None,
            keyframes,
            kf_progress,
            play_subs: None,
            fonts_ready,
            stats,
            plain: None,
            rendered: None,
            status,
            stop_at: None,
            work_dir,
            volume: 1.0,
        };
        p.seek(0);
        Ok(p)
    }

    /// Start building the spectrogram (no-op if running/done or there is no audio).
    pub fn ensure_spectrum(&mut self, ctx: &eframe::egui::Context) {
        if self.spectrum.is_none() && self.info.has_audio {
            let c2 = ctx.clone();
            self.spectrum = Some(Spectrum::start(&self.tools, &self.path, crate::cache::key(&self.path), Box::new(move || c2.request_repaint())));
        }
    }

    /// Finished spectrogram data, if available.
    pub fn spectrum_data(&self) -> Option<Vec<u8>> {
        self.spectrum.as_ref().filter(|s| s.done.load(Ordering::SeqCst)).map(|s| s.data.lock().unwrap().clone())
    }

    pub fn ffmpeg(&self) -> PathBuf {
        self.tools.ffmpeg.clone()
    }

    pub fn is_playing(&self) -> bool {
        self.clock.playing.load(Ordering::Relaxed)
    }

    /// Readiness of background preparation (shown on the loading screen).
    pub fn warm_status(&self) -> WarmStatus {
        let kfs = self.keyframes.lock().unwrap().clone();
        let (waveform, waveform_progress, waveform_cached) = match &self.waveform {
            Some(w) => {
                let done = w.done.load(Ordering::Relaxed);
                let n = w.peaks.lock().unwrap().len() as f32;
                let p = if self.info.duration_ms > 0 { (n * WAVE_BUCKET_MS as f32 / self.info.duration_ms as f32).clamp(0.0, 1.0) } else { 0.0 };
                (done, if done { 1.0 } else { p }, w.from_cache)
            }
            None => (true, 1.0, false),
        };
        WarmStatus {
            first_frame: !self.info.has_video || self.plain.is_some(),
            keyframes: kfs.is_some(),
            keyframe_progress: self.kf_progress.load(Ordering::Relaxed) as f32 / 1000.0,
            keyframe_count: kfs.map(|k| k.len()),
            fonts: self.fonts_ready.load(Ordering::Relaxed),
            waveform,
            waveform_progress,
            waveform_cached,
        }
    }
    pub fn time(&self) -> i64 {
        self.clock.now_ms()
    }
    /// Keyframes if loaded.
    pub fn keyframes(&self) -> Option<Arc<Vec<i64>>> {
        self.keyframes.lock().unwrap().clone()
    }

    pub fn seek(&mut self, t: i64) {
        let t = t.clamp(0, self.info.duration_ms.max(0));
        if self.is_playing() {
            self.play_from(t, self.stop_at);
            return;
        }
        self.clock.stop_at(t);
        if let Some(tx) = &self.vtx {
            let _ = tx.send(VCmd::Seek(t));
        }
    }

    /// Ask for a libass-rendered version of the current paused frame.
    pub fn request_render(&self, t: i64, gen: u64) {
        if let (Some(tx), Some(dir)) = (&self.vtx, &self.work_dir) {
            let _ = tx.send(VCmd::Render { t, gen, dir: dir.clone(), start_offset_ms: self.info.start_ms });
        }
    }

    pub fn play_from(&mut self, t: i64, stop_at: Option<i64>) {
        let t = t.clamp(0, self.info.duration_ms.max(0));
        self.stop_at = stop_at;
        if let Some(a) = &mut self.audio {
            a.start(&self.tools, &self.path, t);
        }
        self.clock.start(t, self.audio.is_some());
        if let Some(tx) = &self.vtx {
            let _ = tx.send(VCmd::Play(t, self.play_subs.clone()));
        }
    }

    /// Restart only the video decoder at the current time (e.g. after the script changed while
    /// playing); audio and the clock keep running so there is no hiccup in sound.
    pub fn restart_video(&mut self) {
        if !self.is_playing() {
            return;
        }
        let t = self.time();
        if let Some(tx) = &self.vtx {
            let _ = tx.send(VCmd::Play(t, self.play_subs.clone()));
        }
    }

    pub fn play(&mut self) {
        let t = self.time();
        let t = if t >= self.info.duration_ms - 50 { 0 } else { t };
        self.play_from(t, None);
    }

    pub fn pause(&mut self) {
        let t = self.time();
        self.clock.stop_at(t);
        if let Some(a) = &mut self.audio {
            a.stop();
        }
        if let Some(tx) = &self.vtx {
            let _ = tx.send(VCmd::Pause);
            let _ = tx.send(VCmd::Seek(t));
        }
        self.stop_at = None;
    }

    pub fn toggle(&mut self) {
        if self.is_playing() {
            self.pause()
        } else {
            self.play()
        }
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 2.0);
        if let Some(a) = &self.audio {
            a.shared.volume.store(self.volume.to_bits(), Ordering::Relaxed);
        }
    }
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Call every frame: pulls decoded frames, handles auto-stop / end of media.
    pub fn update(&mut self) {
        if let Some(rx) = &self.frx {
            while let Ok(f) = rx.try_recv() {
                if f.rendered.is_some() {
                    // drop renders that are out of date (user moved on / started playing)
                    let cur = self.fc.idx_of(self.clock.now_ms());
                    if f.idx == cur && !self.is_playing() && self.plain.as_ref().is_some_and(|p| p.idx == f.idx) {
                        self.rendered = Some(f);
                    }
                } else {
                    if self.rendered.as_ref().is_some_and(|r| r.idx != f.idx) {
                        self.rendered = None;
                    }
                    self.plain = Some(f);
                }
            }
        }
        if self.is_playing() {
            self.rendered = None;
            let t = self.time();
            if let Some(s) = self.stop_at {
                if t >= s {
                    self.pause();
                    self.clock.stop_at(s);
                    return;
                }
            }
            if self.info.duration_ms > 0 && t >= self.info.duration_ms {
                self.pause();
            }
        }
    }

    pub fn has_audio_out(&self) -> bool {
        self.audio.is_some()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Some(a) = &mut self.audio {
            a.stop();
        }
        if let Some(tx) = &self.vtx {
            let _ = tx.send(VCmd::Quit);
        }
    }
}

/// Typical video extensions (for drag & drop and auto-load).
pub const VIDEO_EXTS: &[&str] = &["mkv", "mp4", "m4v", "avi", "mov", "webm", "ts", "m2ts", "wmv", "flv", "mpg", "mpeg", "ogv", "mp3", "flac", "wav", "m4a", "aac", "opus", "ogg"];

pub fn is_media_path(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).map(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false)
}
