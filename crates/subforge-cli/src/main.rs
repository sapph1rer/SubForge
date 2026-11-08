//! SubForge CLI — scriptable subtitle tools (designed to be called from other apps).
//!
//! Exit codes: 0 = ok, 1 = error, 2 = `check` found errors.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use subforge_core::qc::{self, FixOpts, QcConfig, Severity};
use subforge_core::time::{fmt_display, parse_clock, parse_fps, parse_offset};
use subforge_core::timing::{self, Field, Scope};
use subforge_core::{anim, audio, edit, extract, fixes, fonts, ocr, speech, spell, tags, tools, track, Event, Format, HtmlOpts, Subtitle, WriteOpts};

#[derive(Parser)]
#[command(name = "subforge-cli", version, about = "SubForge — subtitle toolkit (SRT / ASS / SSA / VTT)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct InOpts {
    /// Input file ("-" = stdin)
    input: PathBuf,
    /// Force input text encoding (e.g. windows-874, shift_jis); default auto-detect
    #[arg(long = "in-encoding")]
    in_encoding: Option<String>,
    /// Input format when reading stdin or when the extension is unusual
    #[arg(long = "from")]
    from_format: Option<String>,
}

#[derive(Args, Clone, Default)]
struct OutOpts {
    /// Output file ("-" or omitted = stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Overwrite the input file
    #[arg(long, conflicts_with = "output")]
    in_place: bool,
    /// Output format: srt, ass, ssa, vtt, txt (default: from output extension, else input format)
    #[arg(long = "to")]
    to: Option<String>,
    /// Output encoding: utf-8 (default), utf-8-bom, utf-16le, windows-874, shift_jis ...
    #[arg(long, default_value = "utf-8")]
    encoding: String,
    /// Use CRLF line endings
    #[arg(long)]
    crlf: bool,
    /// Drop formatting (italic/bold/colour) when writing SRT/VTT
    #[arg(long)]
    no_formatting: bool,
}

#[derive(Args, Clone)]
struct QcArgs {
    #[arg(long)]
    max_cps: Option<f64>,
    #[arg(long)]
    max_line_len: Option<usize>,
    #[arg(long)]
    max_lines: Option<usize>,
    /// Minimum duration (e.g. 833ms, 1s)
    #[arg(long)]
    min_duration: Option<String>,
    /// Maximum duration (e.g. 7s)
    #[arg(long)]
    max_duration: Option<String>,
    /// Minimum gap between subtitles (e.g. 83ms, 0 = off)
    #[arg(long)]
    min_gap: Option<String>,
    /// Don't count spaces in CPS / length
    #[arg(long)]
    ignore_spaces: bool,
    /// Don't count punctuation in CPS / length
    #[arg(long)]
    ignore_punct: bool,
    /// Load QC settings from a JSON file (same keys as `subforge-cli qc-config`)
    #[arg(long)]
    qc_config: Option<PathBuf>,
}

impl QcArgs {
    fn build(&self) -> Result<QcConfig> {
        let mut c = match &self.qc_config {
            Some(p) => serde_json::from_str(&std::fs::read_to_string(p)?).context("invalid QC config JSON")?,
            None => QcConfig::default(),
        };
        if let Some(v) = self.max_cps { c.max_cps = v; }
        if let Some(v) = self.max_line_len { c.max_line_len = v; }
        if let Some(v) = self.max_lines { c.max_lines = v; }
        if let Some(v) = &self.min_duration { c.min_duration_ms = off(v)?; }
        if let Some(v) = &self.max_duration { c.max_duration_ms = off(v)?; }
        if let Some(v) = &self.min_gap { c.min_gap_ms = off(v)?; }
        if self.ignore_spaces { c.count.ignore_spaces = true; }
        if self.ignore_punct { c.count.ignore_punctuation = true; }
        Ok(c)
    }
}

#[derive(Args, Clone)]
struct FixFlags {
    /// Don't trim/collapse spaces
    #[arg(long)]
    no_trim: bool,
    /// Keep empty subtitles
    #[arg(long)]
    keep_empty: bool,
    /// Don't fix overlaps
    #[arg(long)]
    no_overlaps: bool,
    /// Don't extend short / high-CPS subtitles
    #[arg(long)]
    no_extend: bool,
    /// Also enforce the minimum gap
    #[arg(long)]
    enforce_gap: bool,
    /// Also cut subtitles longer than max duration
    #[arg(long)]
    cap_long: bool,
    /// Don't sort by start time
    #[arg(long)]
    no_sort: bool,
    /// Break single lines longer than --max-line-len into two (word-aware, works for Thai)
    #[arg(long)]
    rebalance: bool,
}

impl FixFlags {
    fn build(&self) -> FixOpts {
        FixOpts {
            trim_spaces: !self.no_trim,
            remove_empty: !self.keep_empty,
            fix_invalid: true,
            fix_overlaps: !self.no_overlaps,
            enforce_min_gap: self.enforce_gap,
            extend_short: !self.no_extend,
            extend_for_cps: !self.no_extend,
            cap_long: self.cap_long,
            sort: !self.no_sort,
            rebalance_long: self.rebalance,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Show information and statistics about a subtitle file
    Info {
        #[command(flatten)]
        input: InOpts,
        #[arg(long)]
        json: bool,
    },
    /// Convert one file to another format / encoding
    Convert {
        #[command(flatten)]
        input: InOpts,
        #[command(flatten)]
        out: OutOpts,
        /// Remove all ASS override tags
        #[arg(long)]
        strip_tags: bool,
    },
    /// Convert / process many files (files or folders)
    Batch {
        /// Input files or directories
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Target format: srt, ass, ssa, vtt, txt
        #[arg(long = "to")]
        to: String,
        /// Output directory (default: next to each input)
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Recurse into sub-directories
        #[arg(short, long)]
        recursive: bool,
        /// Suffix added to output file names (before extension), e.g. ".th"
        #[arg(long, default_value = "")]
        suffix: String,
        /// Overwrite existing outputs
        #[arg(long)]
        overwrite: bool,
        #[arg(long = "in-encoding")]
        in_encoding: Option<String>,
        #[arg(long, default_value = "utf-8")]
        encoding: String,
        #[arg(long)]
        crlf: bool,
        /// Shift all times (e.g. +1.5s, -200ms)
        #[arg(long, allow_hyphen_values = true)]
        shift: Option<String>,
        /// Frame-rate conversion FROM:TO (e.g. 23.976:25)
        #[arg(long)]
        fps: Option<String>,
        /// Run auto-fix with default QC settings
        #[arg(long)]
        fix: bool,
        #[arg(long)]
        strip_tags: bool,
        #[arg(long)]
        json: bool,
    },
    /// Shift times by an offset (e.g. 1.5s, -250ms, -00:00:01,200)
    Shift {
        #[command(flatten)]
        input: InOpts,
        /// Offset
        #[arg(allow_hyphen_values = true)]
        offset: String,
        /// Only events starting at/after this time
        #[arg(long)]
        after: Option<String>,
        /// Only these 1-based event numbers / ranges, e.g. 5,7,10-20
        #[arg(long)]
        lines: Option<String>,
        /// Only shift start times
        #[arg(long, conflicts_with = "end_only")]
        start_only: bool,
        /// Only shift end times
        #[arg(long)]
        end_only: bool,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Change frame rate (e.g. --from 23.976 --to 25)
    Fps {
        #[command(flatten)]
        input: InOpts,
        #[arg(long = "from-fps")]
        from_fps: String,
        #[arg(long = "to-fps")]
        to_fps: String,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Synchronise using reference points OLD=NEW (1 point = shift, 2 = linear, more = piecewise)
    Sync {
        #[command(flatten)]
        input: InOpts,
        /// e.g. --point 0:01:02.500=0:01:04.100 (or #12=0:01:04.100 to use event 12's start)
        #[arg(long = "point", required = true)]
        points: Vec<String>,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Quality check: CPS, line length, durations, overlaps, gaps...
    Check {
        /// Files to check
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[command(flatten)]
        qc: QcArgs,
        /// Hide info-level issues
        #[arg(long)]
        no_info: bool,
        #[arg(long)]
        json: bool,
    },
    /// Automatically fix common timing/text issues
    Fix {
        #[command(flatten)]
        input: InOpts,
        #[command(flatten)]
        qc: QcArgs,
        #[command(flatten)]
        flags: FixFlags,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Find (and optionally replace) text
    Find {
        #[command(flatten)]
        input: InOpts,
        pattern: String,
        #[arg(long)]
        regex: bool,
        #[arg(short = 'c', long)]
        case_sensitive: bool,
        #[arg(short = 'w', long)]
        whole_word: bool,
        /// Replace matches with this (writes output)
        #[arg(long)]
        replace: Option<String>,
        #[command(flatten)]
        out: OutOpts,
        #[arg(long)]
        json: bool,
    },
    /// Dump the whole subtitle as JSON (for integration with other programs)
    Dump {
        #[command(flatten)]
        input: InOpts,
        /// Include per-event plain text and statistics
        #[arg(long)]
        stats: bool,
        /// Pretty-print
        #[arg(long)]
        pretty: bool,
    },
    /// Apply edits from JSON: `[{"index":0,"text":"..."}]` or a full `dump` document
    Apply {
        #[command(flatten)]
        input: InOpts,
        /// JSON file with edits ("-" = stdin)
        #[arg(long)]
        edits: PathBuf,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Extract keyframes (scene changes) from a video with ffprobe
    Keyframes {
        video: PathBuf,
        /// Output file (default stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// aegisub (frame numbers, loadable in Aegisub) or ms
        #[arg(long, default_value = "aegisub")]
        format: String,
        /// Path to ffprobe (default: PATH)
        #[arg(long, default_value = "ffprobe")]
        ffprobe: PathBuf,
    },
    /// Snap line starts/ends to nearby keyframes
    SnapKf {
        #[command(flatten)]
        input: InOpts,
        /// Get keyframes from this video (needs ffprobe)
        #[arg(long, conflicts_with = "keyframes")]
        video: Option<PathBuf>,
        /// Keyframe file (Aegisub v1, or list of frames/seconds/ms/times)
        #[arg(long)]
        keyframes: Option<PathBuf>,
        /// Frame rate for frame-number keyframe files
        #[arg(long)]
        fps: Option<String>,
        /// Snap if a keyframe is this far before the time
        #[arg(long, default_value = "300ms")]
        before: String,
        /// Snap if a keyframe is this far after the time
        #[arg(long, default_value = "300ms")]
        after: String,
        #[arg(long, default_value = "ffprobe")]
        ffprobe: PathBuf,
        #[command(flatten)]
        out: OutOpts,
    },
    /// List subtitle tracks inside a video file (mkv/mp4/ts...)
    Tracks {
        video: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value = "ffprobe")]
        ffprobe: PathBuf,
    },
    /// Extract a subtitle track from a video (ASS is stream-copied losslessly)
    Extract {
        video: PathBuf,
        /// Track number as shown by `tracks` (1-based); default = first text track
        #[arg(short, long)]
        track: Option<usize>,
        /// Output file (default: next to the video)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Extract every text track
        #[arg(long, conflicts_with = "track")]
        all: bool,
        #[arg(long, default_value = "ffmpeg")]
        ffmpeg: PathBuf,
        #[arg(long, default_value = "ffprobe")]
        ffprobe: PathBuf,
    },
    /// Position lines: --x/--y (\pos), --dx/--dy (nudge), --an (alignment) or --clear
    Pos {
        #[command(flatten)]
        input: InOpts,
        /// 1-based line numbers / ranges, e.g. 3,5-9 (default: all)
        #[arg(long)]
        lines: Option<String>,
        #[arg(long)]
        x: Option<f32>,
        #[arg(long)]
        y: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        dx: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        dy: Option<f32>,
        #[arg(long)]
        an: Option<i32>,
        /// Remove \pos/\move/\an so the style decides
        #[arg(long)]
        clear: bool,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Animate lines (tween): movement, scale, rotation, alpha, fades, with easing
    Tween {
        #[command(flatten)]
        input: InOpts,
        #[arg(long)]
        lines: Option<String>,
        /// Preset: slide-left, slide-right, slide-top, slide-bottom, pop, zoom-out, fade-rise, spin, drop, pulse
        #[arg(long)]
        preset: Option<String>,
        /// Move from this offset to the resting position, e.g. -200,0
        #[arg(long, allow_hyphen_values = true)]
        from_offset: Option<String>,
        /// Scale from this percent to the resting scale
        #[arg(long)]
        from_scale: Option<f32>,
        /// Rotate (z) from this many degrees offset
        #[arg(long, allow_hyphen_values = true)]
        from_rot: Option<f32>,
        /// Start fully transparent
        #[arg(long)]
        fade_from: bool,
        /// \fad in/out, e.g. 200,150
        #[arg(long)]
        fad: Option<String>,
        /// Animation length (default 400ms)
        #[arg(long, default_value = "400ms")]
        duration: String,
        /// Easing: linear | sine|quad|cubic|quart|expo|back|elastic|bounce [-in|-out|-inout]
        #[arg(long, default_value = "cubic-out")]
        ease: String,
        /// Generate one line per frame (exact easing for any curve)
        #[arg(long)]
        fbf: bool,
        /// Frame rate for --fbf
        #[arg(long, default_value = "23.976")]
        fps: String,
        /// Remove animation instead
        #[arg(long)]
        remove: bool,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Adjust durations: --add 300ms | --percent 110 | --fixed 2s | --cps 17
    Durations {
        #[command(flatten)]
        input: InOpts,
        #[arg(long, allow_hyphen_values = true)]
        add: Option<String>,
        #[arg(long)]
        percent: Option<f64>,
        #[arg(long)]
        fixed: Option<String>,
        #[arg(long)]
        cps: Option<f64>,
        /// Keep at least this gap to the next line (default 83ms)
        #[arg(long, default_value = "83ms")]
        gap: String,
        #[arg(long)]
        lines: Option<String>,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Close small gaps between lines (chaining)
    BridgeGaps {
        #[command(flatten)]
        input: InOpts,
        /// Bridge gaps smaller than this
        #[arg(long, default_value = "500ms")]
        max: String,
        /// Gap left after bridging
        #[arg(long, default_value = "83ms")]
        min: String,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Remove text for the hearing impaired ([sounds], (sounds), SPEAKER:, ♪ lines)
    RemoveHi {
        #[command(flatten)]
        input: InOpts,
        #[arg(long)]
        keep_brackets: bool,
        #[arg(long)]
        keep_parens: bool,
        #[arg(long)]
        keep_speakers: bool,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Change letter case: upper, lower, sentence, title
    Case {
        #[command(flatten)]
        input: InOpts,
        mode: String,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Change script resolution (PlayRes) and rescale everything, e.g. 1920x1080
    Resample {
        #[command(flatten)]
        input: InOpts,
        /// New resolution, e.g. 1920x1080
        #[arg(value_name = "WIDTHxHEIGHT")]
        resolution: String,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Check the fonts a script uses (system, extra folders, video attachments, [Fonts])
    Fonts {
        #[command(flatten)]
        input: InOpts,
        /// Also look at fonts attached to this video (MKV)
        #[arg(long)]
        video: Option<PathBuf>,
        /// Extra font folder(s)
        #[arg(long = "fonts-dir")]
        fonts_dir: Vec<PathBuf>,
        /// Copy every needed font file into this folder
        #[arg(long)]
        collect: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value = "ffmpeg")]
        ffmpeg: PathBuf,
    },
    /// Put subtitles (and their fonts) into an MKV without re-encoding
    Mux {
        video: PathBuf,
        /// Subtitle file(s) to add
        #[arg(required = true)]
        subs: Vec<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
        /// Language code per subtitle (ISO 639-2), e.g. tha
        #[arg(long = "lang")]
        langs: Vec<String>,
        /// Track title per subtitle
        #[arg(long = "title")]
        titles: Vec<String>,
        /// Make the first added track the default
        #[arg(long)]
        default: bool,
        /// Drop subtitle tracks already in the video
        #[arg(long)]
        replace_subs: bool,
        /// Don't attach the fonts the subtitles use
        #[arg(long)]
        no_fonts: bool,
        #[arg(long = "fonts-dir")]
        fonts_dir: Vec<PathBuf>,
        #[arg(long, default_value = "ffmpeg")]
        ffmpeg: PathBuf,
        #[arg(long, default_value = "ffprobe")]
        ffprobe: PathBuf,
    },
    /// Motion-track 1–2 points in the video and attach lines to them (frame-by-frame)
    Track {
        #[command(flatten)]
        input: InOpts,
        #[arg(long)]
        video: PathBuf,
        /// Lines to attach (1-based); the tracked range covers them
        #[arg(long)]
        lines: String,
        /// Time of the frame where the points are given (default: start of the first line)
        #[arg(long)]
        at: Option<String>,
        /// Point in script coordinates, e.g. --point 812,440 (give twice for scale & rotation)
        #[arg(long = "point", required = true)]
        points: Vec<String>,
        #[arg(long, default_value_t = 31)]
        patch: usize,
        #[arg(long, default_value_t = 32)]
        search: usize,
        /// Tracking resolution (width in pixels)
        #[arg(long, default_value_t = 960)]
        width: u32,
        #[arg(long)]
        no_scale: bool,
        #[arg(long)]
        no_rotation: bool,
        /// Print the tracked path as JSON instead of writing subtitles
        #[arg(long)]
        json: bool,
        #[arg(long, default_value = "ffmpeg")]
        ffmpeg: PathBuf,
        #[arg(long, default_value = "ffprobe")]
        ffprobe: PathBuf,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Speech → subtitles with whisper.cpp (runs locally)
    Transcribe {
        /// Video or audio file
        media: PathBuf,
        #[command(flatten)]
        whisper: WhisperArgs,
        /// Characters per line (lines are cut at pauses / sentence ends, max two lines)
        #[arg(long, default_value_t = 42)]
        max_chars: usize,
        /// Maximum line duration
        #[arg(long, default_value = "7s")]
        max_duration: String,
        /// Write the raw transcript (with per-character times) as JSON instead of subtitles
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Re-time lines whose text is in the spoken language by aligning them to a transcript
    Align {
        #[command(flatten)]
        input: InOpts,
        #[arg(long)]
        video: PathBuf,
        #[command(flatten)]
        whisper: WhisperArgs,
        /// Share of a line's characters that must match (0..1)
        #[arg(long, default_value_t = 0.3)]
        min_ratio: f64,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Fix offset / drift of subtitles in any language by matching them to detected speech
    SyncSpeech {
        #[command(flatten)]
        input: InOpts,
        #[arg(long)]
        video: PathBuf,
        /// Detect speech with whisper (needs --model) instead of the built-in voice detector
        #[arg(long)]
        use_whisper: bool,
        #[command(flatten)]
        whisper: WhisperArgs,
        /// Search range for the offset (±)
        #[arg(long, default_value = "120s")]
        max_offset: String,
        /// Don't estimate a drift (rate) — offset only
        #[arg(long)]
        no_drift: bool,
        /// Snap line starts/ends to speech boundaries within this distance (e.g. 300ms)
        #[arg(long)]
        snap: Option<String>,
        /// Only print the estimate
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        out: OutOpts,
    },
    /// OCR image subtitles (Blu-ray PGS / DVD VobSub) with Tesseract
    Ocr {
        /// Video, .sup or .idx file
        media: PathBuf,
        /// Subtitle track number as shown by `tracks` (default: first image track)
        #[arg(short, long)]
        track: Option<usize>,
        /// Tesseract languages
        #[arg(long, default_value = "tha+eng")]
        lang: String,
        #[arg(long, default_value = "tesseract")]
        tesseract: PathBuf,
        #[arg(long)]
        tessdata: Option<PathBuf>,
        /// Tesseract page segmentation mode per text line
        #[arg(long, default_value_t = 7)]
        psm: u8,
        /// Upscale factor (0 = auto: 3× for DVD)
        #[arg(long, default_value_t = 0)]
        scale: usize,
        /// Replacement list: lines "wrong<TAB>right"
        #[arg(long)]
        replace: Option<PathBuf>,
        /// Save the bitmaps as PNG-ready PGM files into this folder (debug)
        #[arg(long)]
        dump_images: Option<PathBuf>,
        /// Print lines with confidence as JSON
        #[arg(long)]
        json: bool,
        #[arg(long, default_value = "ffprobe")]
        ffprobe: PathBuf,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Spell check (built-in English + Thai dictionaries); exit code 2 if words are unknown
    Spell {
        #[command(flatten)]
        input: InOpts,
        /// Dictionaries: en, th (comma separated)
        #[arg(long, default_value = "en,th")]
        lang: String,
        /// Extra words, one per line
        #[arg(long)]
        user_dict: Option<PathBuf>,
        /// Show up to N suggestions per word
        #[arg(long, default_value_t = 3)]
        suggest: usize,
        #[arg(long)]
        json: bool,
    },
    /// Fix common errors (Thai sara am / เเ / mark order, spaces, punctuation, ...)
    FixErrors {
        #[command(flatten)]
        input: InOpts,
        /// Rules to run (comma separated ids; default: all default rules). See --list
        #[arg(long)]
        rules: Option<String>,
        /// List rule ids and exit
        #[arg(long)]
        list: bool,
        /// Only print the changes
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        out: OutOpts,
    },
    /// Print the default QC configuration as JSON
    QcConfig,
}

#[derive(Args, Clone)]
struct WhisperArgs {
    /// whisper.cpp executable (whisper-cli)
    #[arg(long, default_value = "whisper-cli")]
    whisper: PathBuf,
    /// ggml model file
    #[arg(long)]
    model: Option<PathBuf>,
    /// Spoken language (ja, th, en, ... or auto)
    #[arg(long = "lang", default_value = "auto")]
    language: String,
    /// Translate to English
    #[arg(long)]
    translate: bool,
    #[arg(long, default_value_t = 0)]
    threads: usize,
    /// Initial prompt (names, terms)
    #[arg(long, default_value = "")]
    prompt: String,
    /// Audio stream (0-based among audio streams)
    #[arg(long, default_value_t = 0)]
    audio_track: usize,
    /// Reuse a transcript JSON (from `transcribe --json` or whisper.cpp -ojf) instead of running whisper
    #[arg(long)]
    transcript: Option<PathBuf>,
    #[arg(long, default_value = "ffmpeg")]
    ffmpeg: PathBuf,
}

/// Run whisper on `media` (or load `--transcript`).
fn get_transcript(media: &Path, w: &WhisperArgs) -> Result<speech::Transcript> {
    if let Some(t) = &w.transcript {
        let raw = std::fs::read(t)?;
        if let Ok(tr) = serde_json::from_slice::<speech::Transcript>(&raw) {
            return Ok(tr);
        }
        return speech::parse_whisper_json(&raw).map_err(|e| anyhow!(e));
    }
    let model = w.model.clone().ok_or_else(|| anyhow!("--model is required (or --transcript)"))?;
    let tmp = std::env::temp_dir().join(format!("subforge-whisper-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let wav = tmp.join("audio16k.wav");
    eprint!("decoding audio…");
    let n = speech::extract_wav(&w.ffmpeg, media, w.audio_track, &wav, 0, |_| true).map_err(|e| anyhow!(e))?;
    eprintln!("\rdecoded {:.1} min of audio", n as f64 / 16000.0 / 60.0);
    let opts = speech::WhisperOpts { exe: w.whisper.clone(), model, language: w.language.clone(), translate: w.translate, threads: w.threads, prompt: w.prompt.clone(), extra: Vec::new() };
    let mut last = -1;
    let r = speech::run_whisper(
        &opts,
        &wav,
        &tmp,
        |p| {
            let pc = (p * 100.0) as i32;
            if pc != last {
                eprint!("\rwhisper {pc:3}%");
                last = pc;
            }
            true
        },
        |_| {},
    );
    eprintln!();
    let _ = std::fs::remove_dir_all(&tmp);
    r.map_err(|e| anyhow!(e))
}

fn new_input(format: Format) -> Input {
    Input { sub: Subtitle::new(), format, encoding: "utf-8".into(), path: None }
}

/// Keyframe times (ms) and frame rate of the first video stream, read from packet flags (no decoding).
fn probe_keyframes(ffprobe: &Path, video: &Path) -> Result<(Vec<i64>, f64)> {
    let out = std::process::Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time,flags:stream=avg_frame_rate", "-of", "compact=p=0:nk=1"])
        .arg(video)
        .output()
        .with_context(|| format!("running {}", ffprobe.display()))?;
    if !out.status.success() {
        bail!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut kfs = Vec::new();
    let mut fps = 24000.0 / 1001.0;
    for line in text.lines() {
        let parts: Vec<&str> = line.split('|').collect();
        match parts.as_slice() {
            [t, flags] if flags.contains('K') => {
                if let Ok(v) = t.parse::<f64>() {
                    kfs.push((v * 1000.0).round() as i64);
                }
            }
            [r] if r.contains('/') => {
                if let Some(f) = parse_fps(r) {
                    fps = f;
                }
            }
            _ => {}
        }
    }
    kfs.sort_unstable();
    kfs.dedup();
    Ok((kfs, fps))
}

/// Font check across system fonts, extra folders, the video's attachments and [Fonts].
fn font_reports(sub: &Subtitle, video: Option<&Path>, extra: &[PathBuf], ffmpeg: &Path) -> Result<Vec<fonts::FontReport>> {
    let tmp = std::env::temp_dir().join(format!("subforge-fonts-{}", std::process::id()));
    let embedded = fonts::index_embedded(sub, &tmp.join("embedded"));
    let attached = match video {
        Some(v) => {
            let dir = tmp.join("attached");
            std::fs::create_dir_all(&dir)?;
            let _ = extract::command(ffmpeg)
                .current_dir(&dir)
                .args(["-hide_banner", "-loglevel", "quiet", "-nostdin", "-y", "-dump_attachment:t", ""])
                .arg("-i")
                .arg(std::path::absolute(v)?)
                .args(["-t", "0", "-f", "null", "-"])
                .status();
            fonts::FontIndex::scan(&[dir], None, |_, _| {})
        }
        None => fonts::FontIndex::default(),
    };
    let folder = fonts::FontIndex::scan(extra, None, |_, _| {});
    let system = fonts::FontIndex::scan(&fonts::FontIndex::system_dirs(), None, |_, _| {});
    let r = fonts::check(sub, &[(fonts::Source::Embedded, &embedded), (fonts::Source::Attached, &attached), (fonts::Source::Folder, &folder), (fonts::Source::System, &system)]);
    Ok(r)
}

fn parse_ease(s: &str) -> Result<anim::Easing> {
    let (k, d) = s.split_once('-').unwrap_or((s, "out"));
    let kind = match k {
        "linear" | "lin" => anim::EaseKind::Linear,
        "sine" => anim::EaseKind::Sine,
        "quad" => anim::EaseKind::Quad,
        "cubic" => anim::EaseKind::Cubic,
        "quart" => anim::EaseKind::Quart,
        "expo" => anim::EaseKind::Expo,
        "back" => anim::EaseKind::Back,
        "elastic" => anim::EaseKind::Elastic,
        "bounce" => anim::EaseKind::Bounce,
        o => bail!("unknown easing {o}"),
    };
    let dir = match d {
        "in" => anim::EaseDir::In,
        "out" => anim::EaseDir::Out,
        "inout" | "in-out" => anim::EaseDir::InOut,
        o => bail!("unknown easing direction {o}"),
    };
    Ok(anim::Easing::new(kind, dir))
}

fn off(s: &str) -> Result<i64> {
    parse_offset(s).ok_or_else(|| anyhow!("invalid time/offset: {s}"))
}

fn fmt_arg(s: &str) -> Result<Format> {
    Format::from_ext(s).ok_or_else(|| anyhow!("unknown format '{s}' (use srt, ass, ssa, vtt, txt)"))
}

struct Input {
    sub: Subtitle,
    format: Format,
    encoding: String,
    path: Option<PathBuf>,
}

fn read_input(i: &InOpts) -> Result<Input> {
    let hint = i.from_format.as_deref().map(fmt_arg).transpose()?;
    if i.input.as_os_str() == "-" {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        let dec = subforge_core::encoding::decode(&bytes, i.in_encoding.as_deref());
        let (sub, format) = subforge_core::parse_str(&dec.text, hint)?;
        return Ok(Input { sub, format, encoding: dec.encoding, path: None });
    }
    let l = subforge_core::load(&i.input, i.in_encoding.as_deref()).with_context(|| format!("reading {}", i.input.display()))?;
    let (sub, format) = match hint {
        Some(h) if h != l.format => {
            let bytes = std::fs::read(&i.input)?;
            let dec = subforge_core::encoding::decode(&bytes, i.in_encoding.as_deref());
            subforge_core::parse_str(&dec.text, Some(h))?
        }
        _ => (l.sub, l.format),
    };
    Ok(Input { sub, format, encoding: l.encoding, path: Some(i.input.clone()) })
}

fn write_output(mut sub: Subtitle, input: &Input, out: &OutOpts) -> Result<()> {
    let target_path: Option<PathBuf> = if out.in_place {
        Some(input.path.clone().ok_or_else(|| anyhow!("--in-place needs a file input"))?)
    } else {
        out.output.clone().filter(|p| p.as_os_str() != "-")
    };
    let fmt = match &out.to {
        Some(t) => fmt_arg(t)?,
        None => target_path.as_deref().and_then(Format::from_path).unwrap_or(input.format),
    };
    subforge_core::prepare_for(&mut sub, fmt);
    let opts = WriteOpts {
        encoding: out.encoding.clone(),
        crlf: out.crlf,
        html: if out.no_formatting { HtmlOpts { keep_formatting: false, keep_colors: false, keep_an: true } } else { HtmlOpts::default() },
    };
    match target_path {
        Some(p) => {
            subforge_core::save(&p, &sub, Some(fmt), &opts)?;
            eprintln!("wrote {} ({}, {} events)", p.display(), fmt.ext(), sub.events.len());
        }
        None => {
            let bytes = subforge_core::to_bytes(&sub, fmt, &opts)?;
            std::io::stdout().write_all(&bytes)?;
        }
    }
    Ok(())
}

/// Parse "5,7,10-20" (1-based) into 0-based indices.
fn parse_lines(s: &str) -> Result<Vec<usize>> {
    let mut v = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let a: usize = a.trim().parse()?;
            let b: usize = b.trim().parse()?;
            if a == 0 || b < a {
                bail!("bad range {part}");
            }
            v.extend((a - 1)..b);
        } else {
            let n: usize = part.parse()?;
            if n == 0 {
                bail!("line numbers are 1-based");
            }
            v.push(n - 1);
        }
    }
    Ok(v)
}

#[derive(Serialize)]
struct InfoOut<'a> {
    file: String,
    format: Format,
    encoding: &'a str,
    events: usize,
    dialogue: usize,
    comments: usize,
    styles: Vec<&'a str>,
    first_start_ms: i64,
    last_end_ms: i64,
    total_chars: usize,
    avg_cps: f64,
    max_cps: f64,
    max_line_len: usize,
    play_res: (i32, i32),
}

#[derive(Serialize, Deserialize)]
struct DumpEvent {
    index: usize,
    #[serde(flatten)]
    event: Event,
    #[serde(skip_serializing_if = "Option::is_none")]
    plain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats: Option<qc::EventStats>,
}

#[derive(Serialize)]
struct DumpDoc<'a> {
    format: Format,
    encoding: &'a str,
    info: &'a [(String, String)],
    styles: &'a [subforge_core::Style],
    events: Vec<DumpEvent>,
}

#[derive(Deserialize)]
struct EditItem {
    index: usize,
    text: Option<String>,
    /// Plain text with real newlines (alternative to `text`)
    plain: Option<String>,
    start: Option<i64>,
    end: Option<i64>,
    style: Option<String>,
    actor: Option<String>,
    comment: Option<bool>,
    #[serde(default)]
    delete: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EditsDoc {
    List(Vec<EditItem>),
    Full {
        #[serde(default)]
        info: Option<Vec<(String, String)>>,
        #[serde(default)]
        styles: Option<Vec<subforge_core::Style>>,
        events: Vec<Event>,
    },
}

fn main() {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

fn run(cli: Cli) -> Result<i32> {
    match cli.cmd {
        Cmd::Info { input, json } => {
            let inp = read_input(&input)?;
            let s = &inp.sub;
            let count = Default::default();
            let dlg: Vec<&Event> = s.events.iter().filter(|e| !e.comment).collect();
            let stats: Vec<qc::EventStats> = dlg.iter().map(|e| qc::stats(e, count)).collect();
            let total_chars: usize = stats.iter().map(|x| x.chars).sum();
            let total_ms: i64 = stats.iter().map(|x| x.duration_ms.max(0)).sum();
            let info = InfoOut {
                file: inp.path.as_ref().map(|p| p.display().to_string()).unwrap_or("-".into()),
                format: inp.format,
                encoding: &inp.encoding,
                events: s.events.len(),
                dialogue: dlg.len(),
                comments: s.events.len() - dlg.len(),
                styles: s.styles.iter().map(|x| x.name.as_str()).collect(),
                first_start_ms: dlg.iter().map(|e| e.start).min().unwrap_or(0),
                last_end_ms: dlg.iter().map(|e| e.end).max().unwrap_or(0),
                total_chars,
                avg_cps: if total_ms > 0 { total_chars as f64 / (total_ms as f64 / 1000.0) } else { 0.0 },
                max_cps: stats.iter().map(|x| x.cps).filter(|c| c.is_finite()).fold(0.0, f64::max),
                max_line_len: stats.iter().map(|x| x.max_line_len).max().unwrap_or(0),
                play_res: s.play_res(),
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!("File:        {}", info.file);
                println!("Format:      {}", info.format.name());
                println!("Encoding:    {}", info.encoding);
                println!("Events:      {} ({} dialogue, {} comments)", info.events, info.dialogue, info.comments);
                println!("Styles:      {}", info.styles.join(", "));
                println!("Span:        {} → {}", fmt_display(info.first_start_ms), fmt_display(info.last_end_ms));
                println!("Characters:  {}", info.total_chars);
                println!("CPS:         avg {:.1}, max {:.1}", info.avg_cps, info.max_cps);
                println!("Longest line:{} chars", info.max_line_len);
                println!("PlayRes:     {}x{}", info.play_res.0, info.play_res.1);
            }
            Ok(0)
        }
        Cmd::Convert { input, out, strip_tags } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            if strip_tags {
                edit::strip_all_tags(&mut sub);
            }
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Batch { inputs, to, out_dir, recursive, suffix, overwrite, in_encoding, encoding, crlf, shift, fps, fix, strip_tags, json } => {
            let target = fmt_arg(&to)?;
            let shift = shift.as_deref().map(off).transpose()?;
            let fps = fps
                .as_deref()
                .map(|s| -> Result<(f64, f64)> {
                    let (a, b) = s.split_once(':').ok_or_else(|| anyhow!("--fps expects FROM:TO"))?;
                    Ok((parse_fps(a).ok_or_else(|| anyhow!("bad fps {a}"))?, parse_fps(b).ok_or_else(|| anyhow!("bad fps {b}"))?))
                })
                .transpose()?;
            if let Some(d) = &out_dir {
                std::fs::create_dir_all(d)?;
            }
            let files = subforge_core::collect_inputs(&inputs, recursive);
            if files.is_empty() {
                bail!("no subtitle files found");
            }
            #[derive(Serialize)]
            struct Res {
                input: String,
                output: Option<String>,
                ok: bool,
                message: String,
            }
            let mut results = Vec::new();
            let mut failed = 0;
            for f in &files {
                let r = (|| -> Result<PathBuf> {
                    let l = subforge_core::load(f, in_encoding.as_deref())?;
                    let mut sub = l.sub;
                    if let Some((a, b)) = fps {
                        timing::change_fps(&mut sub, a, b, &Scope::All);
                    }
                    if let Some(ms) = shift {
                        timing::shift(&mut sub, ms, &Scope::All, Field::Both);
                    }
                    if fix {
                        qc::fix(&mut sub, &QcConfig::default(), &FixOpts::default());
                    }
                    if strip_tags {
                        edit::strip_all_tags(&mut sub);
                    }
                    subforge_core::prepare_for(&mut sub, target);
                    let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                    let name = format!("{}{}.{}", stem, suffix, target.ext());
                    let dir = out_dir.clone().unwrap_or_else(|| f.parent().map(Path::to_path_buf).unwrap_or_default());
                    let dest = dir.join(name);
                    if dest == *f && !overwrite {
                        bail!("output would overwrite input (use --suffix or --overwrite)");
                    }
                    if dest.exists() && !overwrite {
                        bail!("{} exists (use --overwrite)", dest.display());
                    }
                    subforge_core::save(&dest, &sub, Some(target), &WriteOpts { encoding: encoding.clone(), crlf, html: HtmlOpts::default() })?;
                    Ok(dest)
                })();
                match r {
                    Ok(dest) => {
                        if !json {
                            println!("ok   {} -> {}", f.display(), dest.display());
                        }
                        results.push(Res { input: f.display().to_string(), output: Some(dest.display().to_string()), ok: true, message: String::new() });
                    }
                    Err(e) => {
                        failed += 1;
                        if !json {
                            println!("FAIL {}: {e:#}", f.display());
                        }
                        results.push(Res { input: f.display().to_string(), output: None, ok: false, message: format!("{e:#}") });
                    }
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else {
                eprintln!("{} converted, {} failed", files.len() - failed, failed);
            }
            Ok(if failed > 0 { 1 } else { 0 })
        }
        Cmd::Shift { input, offset, after, lines, start_only, end_only, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let ms = off(&offset)?;
            let scope = match (lines, after) {
                (Some(l), _) => Scope::Indices(parse_lines(&l)?),
                (None, Some(a)) => Scope::From(parse_clock(&a).ok_or_else(|| anyhow!("bad time {a}"))?),
                _ => Scope::All,
            };
            let field = if start_only { Field::Start } else if end_only { Field::End } else { Field::Both };
            timing::shift(&mut sub, ms, &scope, field);
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Fps { input, from_fps, to_fps, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let a = parse_fps(&from_fps).ok_or_else(|| anyhow!("bad fps {from_fps}"))?;
            let b = parse_fps(&to_fps).ok_or_else(|| anyhow!("bad fps {to_fps}"))?;
            timing::change_fps(&mut sub, a, b, &Scope::All);
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Sync { input, points, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let mut pts = Vec::new();
            for p in &points {
                let (a, b) = p.split_once('=').ok_or_else(|| anyhow!("point must be OLD=NEW: {p}"))?;
                let old = if let Some(n) = a.trim().strip_prefix('#') {
                    let n: usize = n.parse()?;
                    sub.events.get(n.wrapping_sub(1)).map(|e| e.start).ok_or_else(|| anyhow!("no event #{n}"))?
                } else {
                    parse_clock(a).ok_or_else(|| anyhow!("bad time {a}"))?
                };
                let new = parse_clock(b).ok_or_else(|| anyhow!("bad time {b}"))?;
                pts.push((old, new));
            }
            timing::sync_points(&mut sub, &pts, &Scope::All).map_err(|e| anyhow!(e))?;
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Check { inputs, qc: qa, no_info, json } => {
            let cfg = qa.build()?;
            let files = subforge_core::collect_inputs(&inputs, false);
            let mut any_err = false;
            #[derive(Serialize)]
            struct FileReport {
                file: String,
                errors: usize,
                warnings: usize,
                issues: Vec<qc::Issue>,
            }
            let mut reports = Vec::new();
            for f in &files {
                let l = subforge_core::load(f, None)?;
                let mut issues = qc::check(&l.sub, &cfg);
                if no_info {
                    issues.retain(|i| i.severity != Severity::Info);
                }
                let errors = issues.iter().filter(|i| i.severity == Severity::Error).count();
                let warnings = issues.iter().filter(|i| i.severity == Severity::Warning).count();
                any_err |= errors > 0;
                if !json {
                    println!("{} — {} errors, {} warnings, {} total", f.display(), errors, warnings, issues.len());
                    for i in &issues {
                        let e = &l.sub.events[i.index];
                        let tag = match i.severity {
                            Severity::Error => "ERR ",
                            Severity::Warning => "WARN",
                            Severity::Info => "info",
                        };
                        let preview: String = e.plain().replace('\n', " / ").chars().take(40).collect();
                        println!("  {} #{:<5} {} {:<15} {}  | {}", tag, i.index + 1, fmt_display(e.start), i.kind.label(), i.message, preview);
                    }
                }
                reports.push(FileReport { file: f.display().to_string(), errors, warnings, issues });
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&reports)?);
            }
            Ok(if any_err { 2 } else { 0 })
        }
        Cmd::Fix { input, qc: qa, flags, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let rep = qc::fix(&mut sub, &qa.build()?, &flags.build());
            eprintln!(
                "fixed: {} trimmed, {} empty removed, {} invalid, {} overlaps, {} gaps, {} extended, {} capped, {} rebalanced",
                rep.trimmed, rep.removed_empty, rep.fixed_invalid, rep.fixed_overlaps, rep.gaps_enforced, rep.extended, rep.capped, rep.rebalanced
            );
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Find { input, pattern, regex, case_sensitive, whole_word, replace, out, json } => {
            let inp = read_input(&input)?;
            let opts = edit::FindOpts { pattern, regex, case_sensitive, whole_word, field: edit::FindField::Text };
            let re = opts.build().map_err(|e| anyhow!(e))?;
            if let Some(rep) = replace {
                let mut sub = inp.sub.clone();
                let n = edit::replace_all(&mut sub, &opts, &rep).map_err(|e| anyhow!(e))?;
                eprintln!("{n} replacement(s)");
                write_output(sub, &inp, &out)?;
                return Ok(0);
            }
            #[derive(Serialize)]
            struct Hit {
                index: usize,
                start_ms: i64,
                text: String,
            }
            let hits: Vec<Hit> = inp
                .sub
                .events
                .iter()
                .enumerate()
                .filter(|(_, e)| re.is_match(&e.text))
                .map(|(i, e)| Hit { index: i, start_ms: e.start, text: e.text.clone() })
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else {
                for h in &hits {
                    println!("#{:<5} {}  {}", h.index + 1, fmt_display(h.start_ms), h.text);
                }
                eprintln!("{} match(es)", hits.len());
            }
            Ok(if hits.is_empty() { 1 } else { 0 })
        }
        Cmd::Dump { input, stats, pretty } => {
            let inp = read_input(&input)?;
            let s = &inp.sub;
            let doc = DumpDoc {
                format: inp.format,
                encoding: &inp.encoding,
                info: &s.info,
                styles: &s.styles,
                events: s
                    .events
                    .iter()
                    .enumerate()
                    .map(|(i, e)| DumpEvent {
                        index: i,
                        event: e.clone(),
                        plain: stats.then(|| e.plain()),
                        stats: stats.then(|| qc::stats(e, Default::default())),
                    })
                    .collect(),
            };
            let j = if pretty { serde_json::to_string_pretty(&doc)? } else { serde_json::to_string(&doc)? };
            println!("{j}");
            Ok(0)
        }
        Cmd::Apply { input, edits, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let raw = if edits.as_os_str() == "-" {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s
            } else {
                std::fs::read_to_string(&edits)?
            };
            let doc: EditsDoc = serde_json::from_str(&raw).context("invalid edits JSON")?;
            match doc {
                EditsDoc::List(items) => {
                    let mut del = Vec::new();
                    let n = items.len();
                    for it in items {
                        let Some(e) = sub.events.get_mut(it.index) else { bail!("index {} out of range", it.index) };
                        if let Some(t) = it.text { e.text = t; }
                        if let Some(p) = it.plain { e.set_editable(&p); }
                        if let Some(v) = it.start { e.start = v; }
                        if let Some(v) = it.end { e.end = v; }
                        if let Some(v) = it.style { e.style = v; }
                        if let Some(v) = it.actor { e.actor = v; }
                        if let Some(v) = it.comment { e.comment = v; }
                        if it.delete { del.push(it.index); }
                    }
                    del.sort_unstable();
                    del.dedup();
                    for i in del.into_iter().rev() {
                        sub.events.remove(i);
                    }
                    eprintln!("applied {n} edit(s)");
                }
                EditsDoc::Full { info, styles, events } => {
                    if let Some(i) = info { sub.info = i; }
                    if let Some(s) = styles { sub.styles = s; }
                    sub.events = events;
                }
            }
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Keyframes { video, output, format, ffprobe } => {
            let (kfs, fps) = probe_keyframes(&ffprobe, &video)?;
            let text = match format.as_str() {
                "ms" => kfs.iter().map(|k| format!("{k}ms\n")).collect::<String>(),
                "aegisub" => timing::keyframes_to_aegisub(&kfs, fps),
                f => bail!("unknown keyframe format {f} (aegisub, ms)"),
            };
            match output {
                Some(p) => {
                    std::fs::write(&p, text)?;
                    eprintln!("{} keyframes ({:.3} fps) -> {}", kfs.len(), fps, p.display());
                }
                None => print!("{text}"),
            }
            Ok(0)
        }
        Cmd::SnapKf { input, video, keyframes, fps, before, after, ffprobe, out } => {
            let inp = read_input(&input)?;
            let kfs = match (video, keyframes) {
                (Some(v), _) => probe_keyframes(&ffprobe, &v)?.0,
                (None, Some(k)) => {
                    let fps = fps.as_deref().map(|f| parse_fps(f).ok_or_else(|| anyhow!("bad fps {f}"))).transpose()?;
                    timing::parse_keyframes(&std::fs::read_to_string(&k)?, fps).map_err(|e| anyhow!(e))?
                }
                _ => bail!("give --video or --keyframes"),
            };
            let mut sub = inp.sub.clone();
            let n = timing::snap_to_keyframes(&mut sub, &kfs, off(&before)?, off(&after)?, &Scope::All);
            eprintln!("snapped {n} line(s) to {} keyframes", kfs.len());
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Tracks { video, json, ffprobe } => {
            let p = extract::probe_tracks(&ffprobe, &video).map_err(|e| anyhow!(e))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&p.tracks)?);
            } else if p.tracks.is_empty() {
                println!("no subtitle tracks");
            } else {
                for t in &p.tracks {
                    println!("{}{}", t.label(), if t.text_based { "" } else { "  [image-based, needs OCR]" });
                }
            }
            Ok(0)
        }
        Cmd::Extract { video, track, output, all, ffmpeg, ffprobe } => {
            let p = extract::probe_tracks(&ffprobe, &video).map_err(|e| anyhow!(e))?;
            let chosen: Vec<&extract::Track> = if all {
                p.tracks.iter().filter(|t| t.text_based).collect()
            } else if let Some(n) = track {
                vec![p.tracks.get(n.wrapping_sub(1)).ok_or_else(|| anyhow!("no track {n} (see `tracks`)"))?]
            } else {
                vec![p.tracks.iter().find(|t| t.text_based).ok_or_else(|| anyhow!("no text subtitle track"))?]
            };
            for t in chosen {
                let dest = match (&output, all) {
                    (Some(o), false) => o.clone(),
                    _ => extract::default_output(&video, t),
                };
                let mut last = -1i32;
                extract::extract_track(&ffmpeg, &video, t, p.duration_ms, &dest, |f| {
                    let pc = (f * 100.0) as i32;
                    if pc / 10 != last / 10 {
                        eprint!("\r{}  {pc:3}%", t.label());
                        last = pc;
                    }
                    true
                })
                .map_err(|e| anyhow!(e))?;
                eprintln!("\r{} -> {}", t.label(), dest.display());
            }
            Ok(0)
        }
        Cmd::Pos { input, lines, x, y, dx, dy, an, clear, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let idx: Vec<usize> = match lines {
                Some(l) => parse_lines(&l)?,
                None => (0..sub.events.len()).collect(),
            };
            let snapshot = sub.clone();
            for i in idx {
                let Some(e) = sub.events.get_mut(i) else { continue };
                if clear {
                    tags::clear_position(e);
                    continue;
                }
                if let Some(a) = an {
                    tags::set_alignment(e, a);
                }
                if x.is_some() || y.is_some() {
                    let (cx, cy) = tags::explicit_position(e).unwrap_or_else(|| tags::default_anchor(&snapshot, e));
                    tags::set_position(&snapshot, e, x.unwrap_or(cx), y.unwrap_or(cy));
                }
                if dx.is_some() || dy.is_some() {
                    tags::nudge(&snapshot, e, dx.unwrap_or(0.0), dy.unwrap_or(0.0));
                }
            }
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Tween { input, lines, preset, from_offset, from_scale, from_rot, fade_from, fad, duration, ease, fbf, fps, remove, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let fps = parse_fps(&fps).ok_or_else(|| anyhow!("bad fps"))?;
            let mut idx: Vec<usize> = match lines {
                Some(l) => parse_lines(&l)?,
                None => (0..sub.events.len()).filter(|&i| !sub.events[i].comment).collect(),
            };
            idx.sort_unstable();
            let win = off(&duration)?;
            let easing = parse_ease(&ease)?;
            let mut warned = std::collections::BTreeSet::new();
            // process from the end so inserted frame-by-frame lines don't shift pending indices
            for &i in idx.iter().rev() {
                if i >= sub.events.len() {
                    continue;
                }
                if remove {
                    anim::remove_tween(&mut sub, i);
                    continue;
                }
                let here = anim::State::of_event(&sub, &sub.events[i]);
                let dur = sub.events[i].duration();
                let mut tw = match &preset {
                    Some(p) => {
                        let pr = match p.as_str() {
                            "slide-left" => anim::Preset::SlideFromLeft,
                            "slide-right" => anim::Preset::SlideFromRight,
                            "slide-top" => anim::Preset::SlideFromTop,
                            "slide-bottom" => anim::Preset::SlideFromBottom,
                            "pop" => anim::Preset::PopIn,
                            "zoom-out" => anim::Preset::ZoomOut,
                            "fade-rise" => anim::Preset::FadeRise,
                            "spin" => anim::Preset::SpinIn,
                            "drop" => anim::Preset::Drop,
                            "pulse" => anim::Preset::Pulse,
                            o => bail!("unknown preset {o}"),
                        };
                        pr.build(here, sub.play_res(), dur)
                    }
                    None => {
                        let mut t = anim::Tween { from: here, to: here, t2: Some(win), easing, ..Default::default() };
                        if let Some(o) = &from_offset {
                            let (a, b) = o.split_once(',').ok_or_else(|| anyhow!("--from-offset expects DX,DY"))?;
                            t.animate.pos = true;
                            t.from.x += a.trim().parse::<f32>()?;
                            t.from.y += b.trim().parse::<f32>()?;
                        }
                        if let Some(sc) = from_scale {
                            t.animate.scale = true;
                            t.from.scale_x = sc;
                            t.from.scale_y = sc;
                        }
                        if let Some(r) = from_rot {
                            t.animate.rot_z = true;
                            t.from.rot_z += r;
                        }
                        if fade_from {
                            t.animate.alpha = true;
                            t.from.alpha = 255;
                        }
                        t
                    }
                };
                if let Some(f) = &fad {
                    let (a, b) = f.split_once(',').ok_or_else(|| anyhow!("--fad expects IN,OUT"))?;
                    tw.fade_in = off(a)?;
                    tw.fade_out = off(b)?;
                }
                if fbf {
                    tw.mode = anim::Mode::FrameByFrame;
                } else if preset.is_none() {
                    tw.mode = anim::Mode::Tags;
                }
                if !tw.animate.any() && tw.fade_in == 0 && tw.fade_out == 0 {
                    bail!("nothing to animate: give --preset, --from-offset, --from-scale, --from-rot, --fade-from or --fad");
                }
                match anim::apply_tween(&mut sub, i, &tw, fps) {
                    Ok(r) => {
                        for w in r.warnings {
                            warned.insert(w);
                        }
                    }
                    Err(e) => eprintln!("line {}: {e}", i + 1),
                }
            }
            for w in warned {
                eprintln!("note: {w}");
            }
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Durations { input, add, percent, fixed, cps, gap, lines, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let mode = match (add, percent, fixed, cps) {
                (Some(a), None, None, None) => tools::DurationMode::AddMs(off(&a)?),
                (None, Some(p), None, None) => tools::DurationMode::Percent(p),
                (None, None, Some(f), None) => tools::DurationMode::Fixed(off(&f)?),
                (None, None, None, Some(c)) => tools::DurationMode::TargetCps(c),
                _ => bail!("give exactly one of --add, --percent, --fixed, --cps"),
            };
            let scope = match lines {
                Some(l) => Scope::Indices(parse_lines(&l)?),
                None => Scope::All,
            };
            let n = tools::adjust_durations(&mut sub, mode, &scope, Some(off(&gap)?), 0, 0);
            eprintln!("{n} line(s) changed");
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::BridgeGaps { input, max, min, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let n = tools::bridge_gaps(&mut sub, off(&max)?, off(&min)?, &Scope::All);
            eprintln!("{n} gap(s) bridged");
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::RemoveHi { input, keep_brackets, keep_parens, keep_speakers, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let opts = tools::HiOpts { brackets: !keep_brackets, parens: !keep_parens, speakers: !keep_speakers, ..Default::default() };
            let (c, r) = tools::remove_hi(&mut sub, opts, &Scope::All);
            eprintln!("{c} line(s) changed, {r} removed");
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Case { input, mode, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let c = match mode.as_str() {
                "upper" => tools::Case::Upper,
                "lower" => tools::Case::Lower,
                "sentence" => tools::Case::Sentence,
                "title" => tools::Case::Title,
                o => bail!("unknown case {o}"),
            };
            let n = tools::change_case(&mut sub, c, &Scope::All);
            eprintln!("{n} line(s) changed");
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Resample { input, resolution: to, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let (w, h) = to.split_once(['x', 'X']).ok_or_else(|| anyhow!("expected WIDTHxHEIGHT"))?;
            let from = sub.play_res();
            tools::resample(&mut sub, (w.trim().parse()?, h.trim().parse()?));
            eprintln!("resampled {}x{} -> {}", from.0, from.1, to);
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Fonts { input, video, fonts_dir, collect, json, ffmpeg } => {
            let inp = read_input(&input)?;
            let reports = font_reports(&inp.sub, video.as_deref(), &fonts_dir, &ffmpeg)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&reports)?);
            } else {
                for r in &reports {
                    let style = match (r.usage.bold, r.usage.italic) {
                        (true, true) => " Bold Italic",
                        (true, false) => " Bold",
                        (false, true) => " Italic",
                        _ => "",
                    };
                    let (tag, detail) = match &r.status {
                        fonts::Status::Ok => ("ok  ", format!("{} ({})", r.face.as_ref().map(|f| f.path.display().to_string()).unwrap_or_default(), r.source.map(|s| s.label()).unwrap_or(""))),
                        fonts::Status::Synthesized { bold, italic } => (
                            "fake",
                            format!(
                                "no {}{}{} face, renderer will fake it — {}",
                                if *bold { "bold" } else { "" },
                                if *bold && *italic { "/" } else { "" },
                                if *italic { "italic" } else { "" },
                                r.face.as_ref().map(|f| f.path.display().to_string()).unwrap_or_default()
                            ),
                        ),
                        fonts::Status::Missing => ("MISS", "not found".to_string()),
                    };
                    println!("{tag} {}{style}  [{} line(s)]  {detail}", r.usage.family, r.usage.lines.len());
                }
            }
            if let Some(dir) = collect {
                let files = fonts::collect(&reports, &dir).map_err(|e| anyhow!(e))?;
                eprintln!("collected {} font file(s) into {}", files.len(), dir.display());
            }
            let missing = reports.iter().filter(|r| r.status == fonts::Status::Missing).count();
            if missing > 0 {
                eprintln!("{missing} font(s) missing");
            }
            Ok(if missing > 0 { 2 } else { 0 })
        }
        Cmd::Mux { video, subs, output, langs, titles, default, replace_subs, no_fonts, fonts_dir, ffmpeg, ffprobe } => {
            let mut tracks = Vec::new();
            let mut font_files = Vec::new();
            for (i, f) in subs.iter().enumerate() {
                // normalise every subtitle to UTF-8 (ASS keeps its own format)
                let l = subforge_core::load(f, None).with_context(|| format!("reading {}", f.display()))?;
                let tmp = std::env::temp_dir().join(format!("subforge-mux-{}-{i}.{}", std::process::id(), l.format.ext()));
                subforge_core::save(&tmp, &l.sub, Some(l.format), &WriteOpts::default())?;
                if !no_fonts && matches!(l.format, Format::Ass | Format::Ssa) {
                    let r = font_reports(&l.sub, Some(&video), &fonts_dir, &ffmpeg)?;
                    let mut missing: Vec<&str> = r.iter().filter(|r| r.status == fonts::Status::Missing).map(|m| m.usage.family.as_str()).collect();
                    missing.dedup();
                    for m in missing {
                        eprintln!("warning: font not found, not attached: {m}");
                    }
                    // fonts already inside the video don't need attaching again
                    font_files.extend(r.iter().filter(|r| r.source != Some(fonts::Source::Attached)).filter_map(|r| r.face.as_ref().map(|f| f.path.clone())));
                }
                tracks.push(extract::NewTrack {
                    file: tmp,
                    language: langs.get(i).cloned().unwrap_or_default(),
                    title: titles.get(i).cloned().unwrap_or_default(),
                    default: default && i == 0,
                    forced: false,
                });
            }
            font_files.sort();
            font_files.dedup();
            let opts = extract::MuxOpts { video, tracks: tracks.clone(), keep_existing_subs: !replace_subs, fonts: font_files.clone(), output: output.clone() };
            let mut last = -1;
            let r = extract::mux(&ffmpeg, &ffprobe, &opts, |p| {
                let pc = (p * 100.0) as i32;
                if pc / 10 != last / 10 {
                    eprint!("\rmuxing {pc:3}%");
                    last = pc;
                }
                true
            });
            for t in &tracks {
                let _ = std::fs::remove_file(&t.file);
            }
            r.map_err(|e| anyhow!(e))?;
            eprintln!("\rwrote {} ({} subtitle track(s), {} font(s) attached)", output.display(), tracks.len(), font_files.len());
            Ok(0)
        }
        Cmd::Track { input, video, lines, at, points, patch, search, width, no_scale, no_rotation, json, ffmpeg, ffprobe, out } => {
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let idx = parse_lines(&lines)?;
            let evs: Vec<&Event> = idx.iter().filter_map(|&i| sub.events.get(i)).collect();
            if evs.is_empty() {
                bail!("no such lines");
            }
            // video geometry
            let o = extract::command(&ffprobe)
                .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,avg_frame_rate,r_frame_rate", "-of", "json"])
                .arg(&video)
                .output()?;
            let v: serde_json::Value = serde_json::from_slice(&o.stdout)?;
            let st = &v["streams"][0];
            let (vw, vh) = (st["width"].as_u64().unwrap_or(0) as f64, st["height"].as_u64().unwrap_or(0) as f64);
            let fps = st["avg_frame_rate"].as_str().and_then(parse_fps).or_else(|| st["r_frame_rate"].as_str().and_then(parse_fps)).ok_or_else(|| anyhow!("unknown frame rate"))?;
            if vw <= 0.0 {
                bail!("no video stream");
            }
            let tw = (width as f64).min(vw).max(64.0) as usize & !1;
            let th = ((tw as f64 * vh / vw).round() as usize).max(2) & !1;
            let fm = 1000.0 / fps;
            let start = evs.iter().map(|e| e.start).min().unwrap();
            let end = evs.iter().map(|e| e.end).max().unwrap();
            let k0 = (start as f64 / fm - 1e-6).ceil() as i64;
            let k1 = ((end as f64 / fm - 1e-6).ceil() as i64 - 1).max(k0);
            let reference = match &at {
                Some(t) => ((parse_clock(t).ok_or_else(|| anyhow!("bad time {t}"))? as f64 + 1.0) / fm).floor() as i64,
                None => k0,
            };
            let (pw, ph) = sub.play_res();
            let (fx, fy) = (tw as f32 / pw as f32, th as f32 / ph as f32);
            let pts: Vec<(f32, f32)> = points
                .iter()
                .map(|p| -> Result<(f32, f32)> {
                    let (a, b) = p.split_once(',').ok_or_else(|| anyhow!("point must be X,Y"))?;
                    Ok((a.trim().parse::<f32>()? * fx, b.trim().parse::<f32>()? * fy))
                })
                .collect::<Result<_>>()?;
            let params = track::TrackParams { patch, search, ..Default::default() };
            let (kk0, kk1) = (k0.min(reference), k1.max(reference));
            let mut last = -1i64;
            let mut res = track::track_video(&ffmpeg, &std::path::absolute(&video)?, fps, (tw, th), kk0, kk1, reference, &pts, params, |d, t| {
                let pc = (d * 100 / t.max(1)) as i64;
                if pc / 10 != last / 10 {
                    eprint!("\rtracking {pc:3}%");
                    last = pc;
                }
                true
            })
            .map_err(|e| anyhow!(e))?;
            eprintln!();
            for s in res.iter_mut().flatten().flatten() {
                s.x /= fx;
                s.y /= fy;
            }
            let b = if res.len() > 1 { Some(res.remove(1)) } else { None };
            let tr = track::Track { fps, first: kk0, reference, a: res.remove(0), b };
            eprintln!("tracked frames {}-{} ({} weak)", tr.first, tr.last(), tr.weak_frames(params.min_score));
            if json {
                println!("{}", serde_json::to_string_pretty(&tr)?);
                return Ok(0);
            }
            let opts = track::ApplyOpts { position: true, scale: !no_scale, rotation: !no_rotation };
            let mut sorted = idx.clone();
            sorted.sort_unstable();
            for &i in sorted.iter().rev() {
                match track::apply_track(&mut sub, i, &tr, opts) {
                    Ok(r) => {
                        for w in r.warnings {
                            eprintln!("note: {w}");
                        }
                    }
                    Err(e) => eprintln!("line {}: {e}", i + 1),
                }
            }
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Transcribe { media, whisper, max_chars, max_duration, json, out } => {
            let tr = get_transcript(&media, &whisper)?;
            if json {
                let s = serde_json::to_string_pretty(&tr)?;
                match out.output.as_ref().filter(|p| p.as_os_str() != "-") {
                    Some(p) => std::fs::write(p, s)?,
                    None => println!("{s}"),
                }
                return Ok(0);
            }
            let opts = speech::SegmentOpts { max_chars, max_duration_ms: off(&max_duration)?, ..Default::default() };
            let sub = speech::transcript_to_subtitle(&tr, opts);
            eprintln!("language {}, {} segments → {} lines", tr.language, tr.segments.len(), sub.events.len());
            write_output(sub, &new_input(Format::Srt), &out)?;
            Ok(0)
        }
        Cmd::Align { input, video, whisper, min_ratio, out } => {
            let inp = read_input(&input)?;
            let tr = get_transcript(&video, &whisper)?;
            let mut sub = inp.sub.clone();
            let r = speech::align_text(&mut sub, &tr, min_ratio, 84);
            eprintln!("re-timed {} of {} lines from the transcript, {} interpolated", r.matched, r.total, r.interpolated.len());
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::SyncSpeech { input, video, use_whisper, whisper, max_offset, no_drift, snap, dry_run, out } => {
            let inp = read_input(&input)?;
            let speech_iv = if use_whisper || whisper.transcript.is_some() {
                get_transcript(&video, &whisper)?.speech()
            } else {
                eprint!("analysing audio…");
                let mut b = audio::SpecBuilder::new();
                audio::decode_16k(&whisper.ffmpeg, &video, whisper.audio_track, |s| {
                    b.push(s);
                    true
                })
                .map_err(|e| anyhow!(e))?;
                b.finish();
                let v = audio::vad(&b.data, audio::VadOpts::default());
                eprintln!("\r{} speech regions in {:.1} min", v.len(), b.columns() as f64 / 6000.0);
                v
            };
            let opts = speech::SyncOpts { max_offset_ms: off(&max_offset)?, drift: !no_drift, snap_ms: snap.as_deref().map(off).transpose()?.unwrap_or(0), ..Default::default() };
            let mut sub = inp.sub.clone();
            let mut rep = speech::estimate_sync(&sub, &speech_iv, &opts);
            eprintln!(
                "offset {:+.3}s  rate {:.5}{}  confidence {:.2}{}",
                rep.offset_ms as f64 / 1000.0,
                rep.rate,
                if (rep.rate - 1.0).abs() > 1e-9 { format!(" (≈ {:+.2}s at 20 min)", (rep.rate - 1.0) * 1_200.0) } else { String::new() },
                rep.confidence,
                if rep.confidence < 0.15 { "  — LOW, check the result" } else { "" }
            );
            if dry_run {
                return Ok(0);
            }
            speech::apply_sync(&mut sub, &speech_iv, &mut rep, &opts);
            if opts.snap_ms > 0 {
                eprintln!("snapped {} starts, {} ends", rep.snapped_starts, rep.snapped_ends);
            }
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::Ocr { media, track, lang, tesseract, tessdata, psm, scale, replace, dump_images, json, ffprobe, out } => {
            let probe = extract::probe_tracks(&ffprobe, &media).map_err(|e| anyhow!(e))?;
            let t = match track {
                Some(n) => probe.tracks.get(n.wrapping_sub(1)).ok_or_else(|| anyhow!("no track {n} (see `tracks`)"))?,
                None => probe.tracks.iter().find(|t| ocr::is_bitmap_codec(&t.codec)).ok_or_else(|| anyhow!("no PGS/VobSub track"))?,
            };
            let bt = ocr::read_track(&ffprobe, &media, t.sub_index).map_err(|e| anyhow!(e))?;
            eprintln!("{}: {} pictures, {}x{}", t.label(), bt.items.len(), bt.width, bt.height);
            if let Some(d) = &dump_images {
                std::fs::create_dir_all(d)?;
                for (i, b) in bt.items.iter().enumerate() {
                    ocr::binarize(b, 1, 8).write_pgm(&d.join(format!("{:04}.pgm", i + 1)))?;
                }
            }
            let mut pairs = Vec::new();
            if let Some(r) = &replace {
                for l in std::fs::read_to_string(r)?.lines() {
                    if let Some((a, b)) = l.split_once('\t') {
                        pairs.push((a.to_string(), b.to_string()));
                    }
                }
            }
            let opts = ocr::OcrOpts { tesseract, lang, psm, scale, threads: 0, replace: pairs, tessdata };
            let work = std::env::temp_dir().join(format!("subforge-ocr-{}", std::process::id()));
            let t0 = std::time::Instant::now();
            // dictionary repair of Thai OCR slips when Thai is among the languages
            let speller = if opts.lang.contains("tha") { spell::SpellChecker::builtin(false, true).ok() } else { None };
            let lines = ocr::ocr(&bt, &opts, &work, speller.as_ref(), &|d, n| {
                eprint!("\rOCR {d}/{n}");
                true
            });
            let _ = std::fs::remove_dir_all(&work);
            let lines = lines.map_err(|e| anyhow!(e))?;
            let low = lines.iter().filter(|l| l.confidence < 60.0).count();
            eprintln!("\rOCR {} lines in {:.1}s ({} low-confidence)", lines.len(), t0.elapsed().as_secs_f64(), low);
            if json {
                println!("{}", serde_json::to_string_pretty(&lines)?);
                return Ok(0);
            }
            write_output(ocr::to_subtitle(&lines), &new_input(Format::Srt), &out)?;
            Ok(0)
        }
        Cmd::Spell { input, lang, user_dict, suggest, json } => {
            let inp = read_input(&input)?;
            let langs: Vec<String> = lang.split(',').map(|s| s.trim().to_lowercase()).collect();
            let mut sp = spell::SpellChecker::builtin(langs.iter().any(|l| l == "en"), langs.iter().any(|l| l == "th")).map_err(|e| anyhow!(e))?;
            if let Some(u) = &user_dict {
                sp.user.extend(std::fs::read_to_string(u)?.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()));
            }
            let words = spell::check_subtitle(&sp, &inp.sub);
            if json {
                let v: Vec<serde_json::Value> = words
                    .iter()
                    .map(|w| serde_json::json!({"word": w.word, "lines": w.places.iter().map(|p| p.0 + 1).collect::<Vec<_>>(), "suggestions": sp.suggest(&w.word, suggest)}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                for w in &words {
                    let lines: Vec<String> = w.places.iter().map(|p| format!("#{}", p.0 + 1)).collect();
                    let sug = sp.suggest(&w.word, suggest);
                    println!("{:<20} {}{}", w.word, lines.join(" "), if sug.is_empty() { String::new() } else { format!("  → {}", sug.join(", ")) });
                }
                eprintln!("{} unknown word(s)", words.len());
            }
            Ok(if words.is_empty() { 0 } else { 2 })
        }
        Cmd::FixErrors { input, rules, list, dry_run, out } => {
            if list {
                for r in fixes::Rule::ALL {
                    println!("{:<20} {}{}", r.id(), r.label(), if r.default_on() { "" } else { "  (off by default)" });
                }
                return Ok(0);
            }
            let rs: Vec<fixes::Rule> = match &rules {
                Some(s) => s.split(',').map(|x| fixes::Rule::from_id(x.trim()).ok_or_else(|| anyhow!("unknown rule {x} (see --list)"))).collect::<Result<_>>()?,
                None => fixes::Rule::ALL.into_iter().filter(|r| r.default_on()).collect(),
            };
            let inp = read_input(&input)?;
            let mut sub = inp.sub.clone();
            let ch = fixes::preview(&sub, &rs);
            for c in &ch {
                eprintln!("#{:<5} {}\n       {}   [{}]", c.index + 1, c.before, c.after, c.rules.iter().map(|r| r.id()).collect::<Vec<_>>().join(", "));
            }
            eprintln!("{} line(s) changed", ch.len());
            if dry_run {
                return Ok(0);
            }
            fixes::apply(&mut sub, &ch);
            write_output(sub, &inp, &out)?;
            Ok(0)
        }
        Cmd::QcConfig => {
            println!("{}", serde_json::to_string_pretty(&QcConfig::default())?);
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }
}
