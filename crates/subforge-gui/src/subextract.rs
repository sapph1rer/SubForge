//! Find and extract subtitle tracks embedded in video containers (mkv, mp4, ts, ...),
//! like "Import from Matroska" in Subtitle Edit. Text tracks only: bitmap tracks
//! (PGS / VobSub / DVB) would need OCR.

use crate::media::{command, Tools};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    /// ASS / SSA (styles are preserved)
    Ass,
    /// SubRip, WebVTT, mov_text, plain text ... (converted to SRT)
    Text,
    /// Picture based (needs OCR) - cannot be imported
    Bitmap,
}

#[derive(Clone, Debug)]
pub struct SubTrack {
    /// Position among subtitle streams (ffmpeg `0:s:N`)
    pub sub_index: usize,
    pub codec: String,
    pub lang: String,
    pub title: String,
    pub default: bool,
    pub forced: bool,
    pub frames: Option<u64>,
    pub kind: TrackKind,
}

impl SubTrack {
    pub fn importable(&self) -> bool {
        self.kind != TrackKind::Bitmap
    }
    pub fn codec_label(&self) -> &str {
        match self.codec.as_str() {
            "subrip" | "srt" => "SubRip",
            "ass" => "ASS",
            "ssa" => "SSA",
            "webvtt" => "WebVTT",
            "mov_text" => "MP4 text",
            "hdmv_pgs_subtitle" => "PGS (image)",
            "dvd_subtitle" => "VobSub (image)",
            "dvb_subtitle" => "DVB (image)",
            "text" => "Text",
            other => other,
        }
    }
}

fn kind_of(codec: &str) -> TrackKind {
    match codec {
        "ass" | "ssa" => TrackKind::Ass,
        "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub" | "dvb_teletext" | "pgssub" | "dvdsub" => TrackKind::Bitmap,
        _ => TrackKind::Text,
    }
}

/// List the subtitle streams of `path` (a fast container read, no decoding).
pub fn list_tracks(tools: &Tools, path: &Path) -> anyhow::Result<Vec<SubTrack>> {
    let out = command(&tools.ffprobe)
        .args(["-v", "error", "-select_streams", "s", "-show_entries", "stream=index,codec_name:stream_disposition=default,forced:stream_tags", "-of", "json"])
        .arg(path)
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        anyhow::bail!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let mut tracks = Vec::new();
    if let Some(streams) = v["streams"].as_array() {
        for (i, s) in streams.iter().enumerate() {
            let codec = s["codec_name"].as_str().unwrap_or("unknown").to_string();
            let tags = &s["tags"];
            let tag = |k: &str| -> String {
                tags.as_object()
                    .and_then(|m| m.iter().find(|(name, _)| name.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str().unwrap_or("").to_string()))
                    .unwrap_or_default()
            };
            let frames = tags
                .as_object()
                .and_then(|m| m.iter().find(|(name, _)| name.to_ascii_uppercase().starts_with("NUMBER_OF_FRAMES")).and_then(|(_, v)| v.as_str()?.parse::<u64>().ok()));
            tracks.push(SubTrack {
                sub_index: i,
                kind: kind_of(&codec),
                codec,
                lang: tag("language"),
                title: tag("title"),
                default: s["disposition"]["default"].as_i64().unwrap_or(0) == 1,
                forced: s["disposition"]["forced"].as_i64().unwrap_or(0) == 1,
                frames,
            });
        }
    }
    Ok(tracks)
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Where extracted tracks are written.
pub fn out_dir() -> PathBuf {
    std::env::temp_dir().join("subforge-extract")
}

/// Extract one track to a temp file. `progress` is called with 0..1 (based on `duration_ms`).
/// Returns the path of the .srt / .ass file. Honours `cancel` (kills ffmpeg).
pub fn extract(tools: &Tools, video: &Path, track: &SubTrack, duration_ms: i64, cancel: &Arc<AtomicBool>, progress: &dyn Fn(f32)) -> anyhow::Result<PathBuf> {
    let dir = out_dir();
    std::fs::create_dir_all(&dir)?;
    let stem = video.file_stem().map(|s| sanitize(&s.to_string_lossy())).unwrap_or_else(|| "subtitle".into());
    let lang = if track.lang.is_empty() { String::new() } else { format!(".{}", sanitize(&track.lang)) };
    let (ext, codec) = if track.kind == TrackKind::Ass { ("ass", "ass") } else { ("srt", "srt") };
    let out = dir.join(format!("{stem}{lang}.t{}.{ext}", track.sub_index));
    let _ = std::fs::remove_file(&out);

    let mut c = command(&tools.ffmpeg);
    c.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-progress", "pipe:1", "-nostats"]).arg("-i").arg(video);
    c.args(["-map", &format!("0:s:{}", track.sub_index), "-vn", "-an", "-c:s", codec]).arg(&out);
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut s);
        s
    });
    // killer: watch the cancel flag while the reader below blocks
    let done = Arc::new(AtomicBool::new(false));
    let child_id = child.id();
    {
        let (cancel, done) = (cancel.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed) {
                    kill_pid(child_id);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
        });
    }
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(us) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")) {
            if let (Ok(us), true) = (us.trim().parse::<i64>(), duration_ms > 0) {
                progress(((us as f64 / 1000.0) / duration_ms as f64).clamp(0.0, 1.0) as f32);
            }
        }
    }
    let status = child.wait()?;
    done.store(true, Ordering::Relaxed);
    let err = err_thread.join().unwrap_or_default();
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&out);
        anyhow::bail!("cancelled");
    }
    if !status.success() || !out.is_file() {
        anyhow::bail!("ffmpeg could not extract this track: {}", err.trim());
    }
    progress(1.0);
    Ok(out)
}

fn kill_pid(pid: u32) {
    #[cfg(windows)]
    {
        let _ = command(Path::new("taskkill")).args(["/PID", &pid.to_string(), "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(not(windows))]
    {
        let _ = command(Path::new("kill")).args(["-9", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> bool {
        std::process::Command::new("ffmpeg").args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
    }

    #[test]
    fn list_and_extract_from_mkv() {
        let tools = Tools { ffmpeg: "ffmpeg".into(), ffprobe: "ffprobe".into() };
        if !tools.available() {
            eprintln!("ffmpeg missing, skipping");
            return;
        }
        let dir = std::env::temp_dir().join("sf-test-extract");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.srt"), "1\n00:00:01,000 --> 00:00:02,500\nสวัสดีครับ\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond line\n").unwrap();
        std::fs::write(
            dir.join("b.ass"),
            "[Script Info]\nScriptType: v4.00+\nPlayResX: 640\nPlayResY: 360\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,2,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Hello ass\n",
        )
        .unwrap();
        let mkv = dir.join("v.mkv");
        let _ = std::fs::remove_file(&mkv);
        assert!(run(&["-y", "-f", "lavfi", "-i", "color=c=black:s=64x64:d=5", "-i", dir.join("a.srt").to_str().unwrap(), "-i", dir.join("b.ass").to_str().unwrap(), "-map", "0", "-map", "1", "-map", "2", "-c:v", "mpeg4", "-c:s:0", "srt", "-c:s:1", "ass", "-metadata:s:s:0", "language=tha", "-metadata:s:s:1", "language=eng", mkv.to_str().unwrap()]));
        let tracks = list_tracks(&tools, &mkv).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].lang, "tha");
        assert_eq!(tracks[1].kind, TrackKind::Ass);
        let cancel = Arc::new(AtomicBool::new(false));
        let out = extract(&tools, &mkv, &tracks[0], 5000, &cancel, &|_| {}).unwrap();
        let l = subforge_core::load(&out, None).unwrap();
        assert_eq!(l.sub.events.len(), 2);
        assert!(l.sub.events[0].text.contains("สวัสดี"));
        let out2 = extract(&tools, &mkv, &tracks[1], 5000, &cancel, &|_| {}).unwrap();
        let l2 = subforge_core::load(&out2, None).unwrap();
        assert_eq!(l2.sub.events.len(), 1);
        assert_eq!(l2.format, subforge_core::Format::Ass);
    }
}
