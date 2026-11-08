//! Listing and extracting subtitle tracks from video containers (MKV, MP4, TS, ...) with ffmpeg.
//!
//! Text tracks (ASS/SSA, SubRip, WebVTT, mov_text, ...) are extracted losslessly where possible:
//! ASS is stream-copied (styles, `[Fonts]`, layers and original line order kept). Image-based
//! tracks (PGS, VobSub, DVB) need OCR and are reported as unsupported.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A command that never opens a console window on Windows.
pub fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    /// Absolute stream index in the file
    pub index: usize,
    /// Index among subtitle streams (for `-map 0:s:N`)
    pub sub_index: usize,
    pub codec: String,
    pub language: String,
    pub title: String,
    pub default: bool,
    pub forced: bool,
    pub text_based: bool,
}

impl Track {
    /// Best output format for this track.
    pub fn format(&self) -> crate::Format {
        match self.codec.as_str() {
            "ass" | "ssa" => crate::Format::Ass,
            "webvtt" => crate::Format::Vtt,
            _ => crate::Format::Srt,
        }
    }
    pub fn label(&self) -> String {
        let mut s = format!("#{} {}", self.sub_index + 1, self.codec.to_uppercase());
        if !self.language.is_empty() {
            s += &format!(" [{}]", self.language);
        }
        if !self.title.is_empty() {
            s += &format!(" — {}", self.title);
        }
        if self.default {
            s += " (default)";
        }
        if self.forced {
            s += " (forced)";
        }
        s
    }
}

fn is_text_codec(c: &str) -> bool {
    matches!(c, "ass" | "ssa" | "subrip" | "srt" | "webvtt" | "mov_text" | "text" | "microdvd" | "subviewer" | "subviewer1" | "sami" | "realtext" | "jacosub" | "mpl2" | "pjs" | "vplayer" | "stl" | "ttml" | "eia_608")
}

pub struct Probe {
    pub tracks: Vec<Track>,
    pub duration_ms: i64,
}

pub fn probe_tracks(ffprobe: &Path, video: &Path) -> Result<Probe, String> {
    let out = command(ffprobe)
        .args(["-v", "error", "-show_entries", "format=duration:stream=index,codec_type,codec_name:stream_tags=language,title:stream_disposition=default,forced", "-of", "json"])
        .arg(video)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffprobe: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let mut tracks = Vec::new();
    for s in v["streams"].as_array().into_iter().flatten() {
        if s["codec_type"].as_str() != Some("subtitle") {
            continue;
        }
        let codec = s["codec_name"].as_str().unwrap_or("unknown").to_string();
        tracks.push(Track {
            index: s["index"].as_u64().unwrap_or(0) as usize,
            sub_index: tracks.len(),
            text_based: is_text_codec(&codec),
            codec,
            language: s["tags"]["language"].as_str().unwrap_or("").to_string(),
            title: s["tags"]["title"].as_str().unwrap_or("").to_string(),
            default: s["disposition"]["default"].as_i64() == Some(1),
            forced: s["disposition"]["forced"].as_i64() == Some(1),
        });
    }
    let duration_ms = v["format"]["duration"].as_str().and_then(|d| d.parse::<f64>().ok()).map(|d| (d * 1000.0) as i64).unwrap_or(0);
    Ok(Probe { tracks, duration_ms })
}

/// Extract one track to `out` (format chosen by [`Track::format`]). `progress(0..1)` is called
/// while ffmpeg reads through the file; return `false` from it to cancel.
pub fn extract_track(ffmpeg: &Path, video: &Path, track: &Track, duration_ms: i64, out: &Path, mut progress: impl FnMut(f32) -> bool) -> Result<PathBuf, String> {
    if !track.text_based {
        return Err(format!("{} is an image-based subtitle (needs OCR) — not supported", track.codec));
    }
    let (codec_args, fmt): (&[&str], &str) = match track.codec.as_str() {
        "ass" | "ssa" => (&["-c:s", "copy"], "ass"),
        "subrip" | "srt" => (&["-c:s", "copy"], "srt"),
        "webvtt" => (&["-c:s", "copy"], "webvtt"),
        _ => (&["-c:s", "srt"], "srt"),
    };
    let mut c = command(ffmpeg);
    c.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-progress", "pipe:1", "-nostats"]);
    c.arg("-i").arg(video);
    c.args(["-map", &format!("0:{}", track.index)]);
    c.args(codec_args);
    c.args(["-f", fmt]).arg(out);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut s);
        s
    });
    let mut cancelled = false;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(v) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")) {
            if let Ok(us) = v.trim().parse::<i64>() {
                let p = if duration_ms > 0 { (us as f64 / 1000.0 / duration_ms as f64) as f32 } else { 0.0 };
                if !progress(p.clamp(0.0, 1.0)) {
                    cancelled = true;
                    let _ = child.kill();
                    break;
                }
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_thread.join().unwrap_or_default();
    if cancelled {
        let _ = std::fs::remove_file(out);
        return Err("cancelled".into());
    }
    if !status.success() {
        return Err(format!("ffmpeg failed: {}", err.lines().last().unwrap_or("").trim()));
    }
    progress(1.0);
    Ok(out.to_path_buf())
}

/// Default output name next to the video: `video.<lang>.<n>.<ext>`.
pub fn default_output(video: &Path, track: &Track) -> PathBuf {
    let stem = video.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "subtitle".into());
    let lang = if track.language.is_empty() { String::new() } else { format!(".{}", track.language) };
    video.with_file_name(format!("{stem}{lang}.track{}.{}", track.sub_index + 1, track.format().ext()))
}

// ------------------------------------------------------------------ muxing

/// Existing attachment file names in a container.
pub fn probe_attachments(ffprobe: &Path, video: &Path) -> Result<Vec<String>, String> {
    let out = command(ffprobe)
        .args(["-v", "error", "-show_entries", "stream=codec_type:stream_tags=filename", "-of", "json"])
        .arg(video)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run ffprobe: {e}"))?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    Ok(v["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["codec_type"].as_str() == Some("attachment"))
        .filter_map(|s| s["tags"]["filename"].as_str().map(|x| x.to_string()))
        .collect())
}

/// A subtitle file to add as a new track.
#[derive(Debug, Clone)]
pub struct NewTrack {
    pub file: PathBuf,
    /// ISO 639-2 code, e.g. "tha", "eng", "jpn"
    pub language: String,
    pub title: String,
    pub default: bool,
    pub forced: bool,
}

#[derive(Debug, Clone)]
pub struct MuxOpts {
    pub video: PathBuf,
    pub tracks: Vec<NewTrack>,
    /// Keep the subtitle tracks already in the video
    pub keep_existing_subs: bool,
    /// Font files to attach (ones already attached under the same name are skipped)
    pub fonts: Vec<PathBuf>,
    pub output: PathBuf,
}

pub fn font_mimetype(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("otf") => "application/vnd.ms-opentype",
        _ => "application/x-truetype-font",
    }
}

/// Build the ffmpeg argument list for [`mux`] (exposed for testing / display).
pub fn mux_args(opts: &MuxOpts, existing_subs: usize, existing_attachments: &[String]) -> Result<Vec<String>, String> {
    let ext = opts.output.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext != "mkv" && ext != "mka" {
        return Err("output must be .mkv (Matroska keeps ASS styling and fonts)".into());
    }
    if opts.output == opts.video {
        return Err("output must be a different file than the input video".into());
    }
    let s = |x: &str| x.to_string();
    let mut a: Vec<String> = vec![s("-hide_banner"), s("-loglevel"), s("error"), s("-nostdin"), s("-y"), s("-progress"), s("pipe:1"), s("-nostats")];
    a.push(s("-i"));
    a.push(opts.video.to_string_lossy().to_string());
    for t in &opts.tracks {
        // read our own files as UTF-8 regardless of locale
        a.extend([s("-sub_charenc"), s("UTF-8"), s("-i"), t.file.to_string_lossy().to_string()]);
    }
    a.extend([s("-map"), s("0:v?"), s("-map"), s("0:a?")]);
    let kept = if opts.keep_existing_subs {
        a.extend([s("-map"), s("0:s?")]);
        existing_subs
    } else {
        0
    };
    a.extend([s("-map"), s("0:t?"), s("-map"), s("0:d?")]);
    for (i, _) in opts.tracks.iter().enumerate() {
        a.extend([s("-map"), format!("{}:0", i + 1)]);
    }
    a.extend([s("-c"), s("copy")]);
    let any_default = opts.tracks.iter().any(|t| t.default);
    if any_default {
        for k in 0..kept {
            a.extend([format!("-disposition:s:{k}"), s("0")]);
        }
    }
    for (i, t) in opts.tracks.iter().enumerate() {
        let n = kept + i;
        if !t.language.is_empty() {
            a.extend([format!("-metadata:s:s:{n}"), format!("language={}", t.language)]);
        }
        if !t.title.is_empty() {
            a.extend([format!("-metadata:s:s:{n}"), format!("title={}", t.title)]);
        }
        let mut disp = Vec::new();
        if t.default {
            disp.push("default");
        }
        if t.forced {
            disp.push("forced");
        }
        a.extend([format!("-disposition:s:{n}"), if disp.is_empty() { s("0") } else { disp.join("+") }]);
    }
    let mut att = existing_attachments.len();
    let mut seen: Vec<String> = existing_attachments.iter().map(|x| x.to_lowercase()).collect();
    for f in &opts.fonts {
        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if name.is_empty() || seen.contains(&name.to_lowercase()) {
            continue;
        }
        seen.push(name.to_lowercase());
        a.extend([s("-attach"), f.to_string_lossy().to_string()]);
        a.extend([format!("-metadata:s:t:{att}"), format!("mimetype={}", font_mimetype(f))]);
        a.extend([format!("-metadata:s:t:{att}"), format!("filename={name}")]);
        att += 1;
    }
    a.push(opts.output.to_string_lossy().to_string());
    Ok(a)
}

/// Mux subtitle tracks and fonts into a new MKV without re-encoding.
pub fn mux(ffmpeg: &Path, ffprobe: &Path, opts: &MuxOpts, mut progress: impl FnMut(f32) -> bool) -> Result<(), String> {
    let probe = probe_tracks(ffprobe, &opts.video)?;
    let attached = probe_attachments(ffprobe, &opts.video)?;
    let args = mux_args(opts, probe.tracks.len(), &attached)?;
    let mut c = command(ffmpeg);
    c.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut s);
        s
    });
    let mut cancelled = false;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(v) = line.strip_prefix("out_time_us=") {
            if let Ok(us) = v.trim().parse::<i64>() {
                let p = if probe.duration_ms > 0 { (us as f64 / 1000.0 / probe.duration_ms as f64) as f32 } else { 0.0 };
                if !progress(p.clamp(0.0, 1.0)) {
                    cancelled = true;
                    let _ = child.kill();
                    break;
                }
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_thread.join().unwrap_or_default();
    if cancelled {
        let _ = std::fs::remove_file(&opts.output);
        return Err("cancelled".into());
    }
    if !status.success() {
        let _ = std::fs::remove_file(&opts.output);
        return Err(format!("ffmpeg failed: {}", err.lines().last().unwrap_or("").trim()));
    }
    progress(1.0);
    Ok(())
}
