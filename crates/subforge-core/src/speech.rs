//! Speech-based timing:
//!
//! * **Transcribe** — run [whisper.cpp](https://github.com/ggml-org/whisper.cpp) (`whisper-cli`)
//!   locally, read its full JSON output (per-token timestamps) and cut it into subtitle lines.
//! * **Align text** — re-time existing lines whose text matches the spoken language (character
//!   level alignment against the transcript, works without spaces, e.g. Thai/Japanese).
//! * **Sync to speech** — for subtitles in *any* language (e.g. Thai subs over Japanese audio):
//!   estimate offset + linear drift by correlating subtitle intervals with detected speech, then
//!   optionally snap line edges to nearby speech boundaries.

use crate::{Event, Subtitle};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;

// ------------------------------------------------------------------ transcript

/// One character of transcript text with the time span of the token it came from.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TChar {
    pub c: char,
    pub t0: i64,
    pub t1: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Segment {
    pub start: i64,
    pub end: i64,
    pub text: String,
    pub chars: Vec<TChar>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Transcript {
    pub language: String,
    pub segments: Vec<Segment>,
}

impl Transcript {
    /// Speech intervals (one per segment, merged when they touch).
    pub fn speech(&self) -> Vec<(i64, i64)> {
        let mut v: Vec<(i64, i64)> = self.segments.iter().filter(|s| s.end > s.start && !s.text.trim().is_empty()).map(|s| (s.start, s.end)).collect();
        v.sort();
        let mut out: Vec<(i64, i64)> = Vec::new();
        for (a, b) in v {
            match out.last_mut() {
                Some(l) if a <= l.1 => l.1 = l.1.max(b),
                _ => out.push((a, b)),
            }
        }
        out
    }
}

/// whisper.cpp writes token text as raw bytes, so a multi-byte character (Thai, Japanese) can
/// be split across tokens and the JSON is not valid UTF-8. Invalid bytes are mapped to private
/// use code points U+F780..U+F7FF so the JSON parses, and mapped back per token.
fn bytes_to_lossless_str(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len());
    for chunk in b.utf8_chunks() {
        s.push_str(chunk.valid());
        for &x in chunk.invalid() {
            s.push(char::from_u32(0xF700 + x as u32).unwrap());
        }
    }
    s
}

fn lossless_str_to_bytes(s: &str, out: &mut Vec<u8>) {
    for c in s.chars() {
        let u = c as u32;
        if (0xF780..=0xF7FF).contains(&u) {
            out.push((u - 0xF700) as u8);
        } else {
            let mut b = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        }
    }
}

fn is_special_token(t: &str) -> bool {
    let t = t.trim();
    (t.starts_with("[_") && t.ends_with(']')) || (t.starts_with("<|") && t.ends_with("|>"))
}

/// Parse whisper.cpp `--output-json-full` (`-ojf`) output.
pub fn parse_whisper_json(raw: &[u8]) -> Result<Transcript, String> {
    let text = bytes_to_lossless_str(raw);
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("whisper JSON: {e}"))?;
    let language = v["result"]["language"].as_str().unwrap_or("").to_string();
    let mut segments = Vec::new();
    for s in v["transcription"].as_array().ok_or("whisper JSON: no \"transcription\"")? {
        let start = s["offsets"]["from"].as_i64().unwrap_or(0);
        let end = s["offsets"]["to"].as_i64().unwrap_or(start);
        // concatenate token bytes, remembering each byte's token time span
        let mut bytes: Vec<u8> = Vec::new();
        let mut spans: Vec<(usize, usize, i64, i64)> = Vec::new(); // byte range + time
        if let Some(toks) = s["tokens"].as_array() {
            for t in toks {
                let tt = t["text"].as_str().unwrap_or("");
                if is_special_token(tt) {
                    continue;
                }
                let a = bytes.len();
                lossless_str_to_bytes(tt, &mut bytes);
                let t0 = t["offsets"]["from"].as_i64().unwrap_or(start);
                let t1 = t["offsets"]["to"].as_i64().unwrap_or(t0).max(t0);
                spans.push((a, bytes.len(), t0, t1));
            }
        }
        if spans.is_empty() {
            // no token data (older builds): spread the segment text evenly
            lossless_str_to_bytes(s["text"].as_str().unwrap_or(""), &mut bytes);
            spans.push((0, bytes.len(), start, end));
        }
        let decoded = String::from_utf8_lossy(&bytes).into_owned();
        let mut chars = Vec::new();
        if decoded.len() == bytes.len() || !decoded.contains('\u{FFFD}') {
            let mut si = 0;
            for (bi, c) in decoded.char_indices() {
                while si + 1 < spans.len() && spans[si].1 <= bi {
                    si += 1;
                }
                let (a, b, t0, t1) = spans[si];
                let frac = |x: usize| if b > a { (x.saturating_sub(a)).min(b - a) as f64 / (b - a) as f64 } else { 0.0 };
                let c0 = t0 + ((t1 - t0) as f64 * frac(bi)).round() as i64;
                let c1 = t0 + ((t1 - t0) as f64 * frac(bi + c.len_utf8())).round() as i64;
                chars.push(TChar { c, t0: c0, t1: c1.max(c0) });
            }
        }
        // trim leading/trailing whitespace characters
        while chars.first().is_some_and(|c| c.c.is_whitespace()) {
            chars.remove(0);
        }
        while chars.last().is_some_and(|c| c.c.is_whitespace()) {
            chars.pop();
        }
        let text: String = chars.iter().map(|c| c.c).collect();
        if text.is_empty() {
            continue;
        }
        segments.push(Segment { start, end, text, chars });
    }
    Ok(Transcript { language, segments })
}

// ------------------------------------------------------------------ running whisper.cpp

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhisperOpts {
    /// `whisper-cli` (or the older `main`) executable
    pub exe: PathBuf,
    /// ggml model file (`ggml-large-v3-turbo.bin`, ...)
    pub model: PathBuf,
    /// spoken language code (`ja`, `th`, `en`, ...) or `auto`
    pub language: String,
    /// translate to English instead of transcribing
    pub translate: bool,
    /// 0 = let whisper decide
    pub threads: usize,
    /// initial prompt (names, terms)
    pub prompt: String,
    /// extra command line arguments (e.g. `-bs 5`, `--vad -vm model.bin`)
    pub extra: Vec<String>,
}

impl Default for WhisperOpts {
    fn default() -> Self {
        WhisperOpts { exe: PathBuf::from("whisper-cli"), model: PathBuf::new(), language: "auto".into(), translate: false, threads: 0, prompt: String::new(), extra: Vec::new() }
    }
}

/// Model names offered for download (ggml files from the whisper.cpp model repository).
pub const MODELS: &[(&str, &str)] = &[
    ("tiny", "75 MB · fastest, rough"),
    ("base", "142 MB · fast"),
    ("small", "466 MB · good for clear speech"),
    ("medium", "1.5 GB · accurate, slow on CPU"),
    ("large-v3-turbo", "1.6 GB · best quality/speed (recommended with a GPU build)"),
    ("large-v3", "3.1 GB · most accurate, slowest"),
];

pub fn model_url(name: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin")
}

/// Look for whisper-cli next to `hint_dirs` or on PATH.
pub fn locate_whisper(hint_dirs: &[PathBuf]) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) { &["whisper-cli.exe", "main.exe", "whisper.exe"] } else { &["whisper-cli", "whisper-cpp", "whisper"] };
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for d in hint_dirs.iter().chain(path_dirs.iter()) {
        for n in names {
            let p = d.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Extract 16 kHz mono audio to a WAV file for whisper. Returns the sample count.
pub fn extract_wav(ffmpeg: &Path, input: &Path, track: usize, out: &Path, duration_ms: i64, mut progress: impl FnMut(f32) -> bool) -> Result<usize, String> {
    let mut all: Vec<i16> = Vec::new();
    crate::audio::decode_16k(ffmpeg, input, track, |s| {
        all.extend_from_slice(s);
        let ms = all.len() as i64 * 1000 / crate::audio::SAMPLE_RATE as i64;
        progress(if duration_ms > 0 { (ms as f32 / duration_ms as f32).min(1.0) } else { 0.0 })
    })?;
    crate::audio::write_wav(out, &all).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    Ok(all.len())
}

/// Run whisper.cpp on a 16 kHz WAV. `progress(0..1)` returns `false` to cancel.
pub fn run_whisper(opts: &WhisperOpts, wav: &Path, work_dir: &Path, mut progress: impl FnMut(f32) -> bool, mut log: impl FnMut(&str)) -> Result<Transcript, String> {
    use std::io::{BufRead, BufReader};
    if !opts.model.is_file() {
        return Err(format!("model not found: {}", opts.model.display()));
    }
    std::fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let base = work_dir.join("whisper-out");
    let json = base.with_extension("json");
    let _ = std::fs::remove_file(&json);
    let mut c = crate::extract::command(&opts.exe);
    c.arg("-m").arg(&opts.model).arg("-f").arg(wav);
    c.args(["-l", if opts.language.trim().is_empty() { "auto" } else { opts.language.trim() }]);
    if opts.translate {
        c.arg("-tr");
    }
    if opts.threads > 0 {
        c.args(["-t", &opts.threads.to_string()]);
    }
    if !opts.prompt.trim().is_empty() {
        c.args(["--prompt", opts.prompt.trim()]);
    }
    c.args(&opts.extra);
    c.args(["-ojf", "-pp", "-of"]).arg(&base);
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("cannot run {}: {e}", opts.exe.display()))?;
    let err = child.stderr.take().unwrap();
    let mut tail: Vec<String> = Vec::new();
    // stderr carries "whisper_print_progress_callback: progress =  45%" and log lines
    let mut reader = BufReader::new(err);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let l = String::from_utf8_lossy(&line).trim().to_string();
        if let Some(p) = l.split("progress =").nth(1) {
            if let Ok(v) = p.trim().trim_end_matches('%').trim().parse::<f32>() {
                if !progress(v / 100.0) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("cancelled".into());
                }
            }
            continue;
        }
        if !l.is_empty() {
            log(&l);
            tail.push(l);
            if tail.len() > 12 {
                tail.remove(0);
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() || !json.is_file() {
        return Err(format!("whisper failed ({}):\n{}", status, tail.join("\n")));
    }
    let raw = std::fs::read(&json).map_err(|e| e.to_string())?;
    parse_whisper_json(&raw)
}

// ------------------------------------------------------------------ transcript → subtitle lines

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SegmentOpts {
    /// characters per line (two lines max)
    pub max_chars: usize,
    pub max_duration_ms: i64,
    pub min_duration_ms: i64,
    /// a pause at least this long always starts a new line
    pub split_pause_ms: i64,
    /// extend line ends by up to this much into silence (reading time)
    pub extend_ms: i64,
    /// minimum gap kept between lines
    pub gap_ms: i64,
}

impl Default for SegmentOpts {
    fn default() -> Self {
        SegmentOpts { max_chars: 42, max_duration_ms: 7000, min_duration_ms: 900, split_pause_ms: 700, extend_ms: 400, gap_ms: 84 }
    }
}

struct Word {
    text: String,
    t0: i64,
    t1: i64,
}

fn words_of(seg: &Segment) -> Vec<Word> {
    let s: String = seg.chars.iter().map(|c| c.c).collect();
    // char index of each byte offset
    let mut byte_to_char = vec![0usize; s.len() + 1];
    for (ci, (bi, c)) in s.char_indices().enumerate() {
        for k in 0..c.len_utf8() {
            byte_to_char[bi + k] = ci;
        }
    }
    byte_to_char[s.len()] = seg.chars.len();
    let mut cuts: Vec<usize> = vec![0];
    cuts.extend(crate::text::word_boundaries(&s));
    cuts.push(s.len());
    cuts.dedup();
    let mut out: Vec<Word> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (byte_to_char[w[0]], byte_to_char[w[1]]);
        if a >= b {
            continue;
        }
        let text: String = seg.chars[a..b].iter().map(|c| c.c).collect();
        let vis: Vec<&TChar> = seg.chars[a..b].iter().filter(|c| !c.c.is_whitespace()).collect();
        if vis.is_empty() {
            // whitespace: attach to the previous word so spacing is preserved
            if let Some(l) = out.last_mut() {
                l.text.push_str(&text);
            }
            continue;
        }
        out.push(Word { text, t0: vis.first().unwrap().t0, t1: vis.last().unwrap().t1 });
    }
    out
}

fn ends_sentence(w: &str) -> bool {
    let t = w.trim_end();
    t.ends_with(['.', '?', '!', '。', '？', '！', '…']) && !t.ends_with("...") || t.ends_with("?!")
}

/// Turn a transcript into subtitle events (plain text, max two balanced lines each).
pub fn transcript_to_events(tr: &Transcript, opts: SegmentOpts) -> Vec<Event> {
    let max_total = opts.max_chars * 2;
    let mut lines: Vec<(i64, i64, String)> = Vec::new();
    for seg in &tr.segments {
        let words = words_of(seg);
        let mut cur: Vec<&Word> = Vec::new();
        let flush = |cur: &mut Vec<&Word>, lines: &mut Vec<(i64, i64, String)>| {
            if cur.is_empty() {
                return;
            }
            let text: String = cur.iter().map(|w| w.text.as_str()).collect::<String>().trim().to_string();
            if !text.is_empty() {
                lines.push((cur.first().unwrap().t0, cur.last().unwrap().t1, text));
            }
            cur.clear();
        };
        for w in &words {
            if let Some(last) = cur.last() {
                let len: usize = crate::text::grapheme_len(&cur.iter().map(|w| w.text.as_str()).collect::<String>()) + crate::text::grapheme_len(w.text.trim_end());
                let dur = w.t1 - cur[0].t0;
                let pause = w.t0 - last.t1;
                let cur_len = crate::text::grapheme_len(cur.iter().map(|w| w.text.as_str()).collect::<String>().trim());
                if len > max_total || dur > opts.max_duration_ms || pause >= opts.split_pause_ms || (ends_sentence(&last.text) && cur_len >= opts.max_chars / 2) {
                    flush(&mut cur, &mut lines);
                }
            }
            cur.push(w);
        }
        flush(&mut cur, &mut lines);
    }
    lines.sort_by_key(|l| l.0);
    // timing polish: minimum duration, extend into silence, keep a gap
    let n = lines.len();
    for i in 0..n {
        let next_start = if i + 1 < n { lines[i + 1].0 } else { i64::MAX };
        let limit = next_start.saturating_sub(opts.gap_ms);
        let (s, e) = (lines[i].0, lines[i].1);
        let mut e2 = (e + opts.extend_ms).max(s + opts.min_duration_ms);
        e2 = e2.min(limit).max(e.min(limit)).max(s + 1);
        lines[i].1 = e2;
    }
    lines
        .into_iter()
        .map(|(s, e, t)| {
            let t = if crate::text::grapheme_len(&t) > opts.max_chars { crate::text::balance_two_lines(&t) } else { t };
            Event::new(s, e, t)
        })
        .collect()
}

pub fn transcript_to_subtitle(tr: &Transcript, opts: SegmentOpts) -> Subtitle {
    let mut sub = Subtitle::new();
    sub.ensure_defaults();
    sub.events = transcript_to_events(tr, opts);
    sub
}

// ------------------------------------------------------------------ align text

fn norm_char(c: char) -> Option<char> {
    if c.is_alphanumeric() {
        Some(c.to_lowercase().next().unwrap_or(c))
    } else {
        None
    }
}

/// Lines that should be re-timed (dialogue, not comments / generated animation frames).
fn timable(e: &Event) -> bool {
    !e.comment && !e.effect.starts_with(crate::anim::EFFECT_GEN) && !e.plain().trim().is_empty()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlignReport {
    /// lines re-timed from matched text
    pub matched: usize,
    /// lines moved by interpolation from their neighbours
    pub interpolated: Vec<usize>,
    pub total: usize,
}

/// Re-time lines whose text is in the spoken language by aligning characters to the transcript.
/// `min_ratio` is the share of a line's characters that must match (0.3 is a good default).
pub fn align_text(sub: &mut Subtitle, tr: &Transcript, min_ratio: f64, gap_ms: i64) -> AlignReport {
    let tchars: Vec<TChar> = tr.segments.iter().flat_map(|s| s.chars.iter().copied()).filter_map(|c| norm_char(c.c).map(|n| TChar { c: n, ..c })).collect();
    let a: Vec<char> = tchars.iter().map(|c| c.c).collect();
    let idx: Vec<usize> = (0..sub.events.len()).filter(|&i| timable(&sub.events[i])).collect();
    let mut b: Vec<char> = Vec::new();
    let mut owner: Vec<(usize, usize)> = Vec::new(); // (event slot, char index within the line)
    let mut line_len = vec![0usize; idx.len()];
    for (slot, &i) in idx.iter().enumerate() {
        for c in sub.events[i].plain().chars().filter_map(norm_char) {
            owner.push((slot, line_len[slot]));
            line_len[slot] += 1;
            b.push(c);
        }
    }
    let mut report = AlignReport { total: idx.len(), ..Default::default() };
    if a.is_empty() || b.is_empty() {
        report.interpolated = idx.clone();
        return report;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let ops = similar::capture_diff_slices_deadline(similar::Algorithm::Myers, &a, &b, Some(deadline));
    // per line: matched (char-in-line, transcript char)
    let mut hits: Vec<Vec<(usize, TChar)>> = vec![Vec::new(); idx.len()];
    for op in ops {
        if let similar::DiffOp::Equal { old_index, new_index, len } = op {
            for k in 0..len {
                let (slot, pos) = owner[new_index + k];
                hits[slot].push((pos, tchars[old_index + k]));
            }
        }
    }
    let old: Vec<(i64, i64)> = idx.iter().map(|&i| (sub.events[i].start, sub.events[i].end)).collect();
    let mut new: Vec<Option<(i64, i64)>> = vec![None; idx.len()];
    for slot in 0..idx.len() {
        let h = &hits[slot];
        let n = line_len[slot];
        if n == 0 || h.len() < 2 || (h.len() as f64) < n as f64 * min_ratio {
            continue;
        }
        // reject scattered matches: the matched span must not be wildly longer than the line
        let (p0, c0) = h[0];
        let (p1, c1) = *h.last().unwrap();
        let span = c1.t1 - c0.t0;
        let per_char = (span as f64 / (p1 - p0 + 1).max(1) as f64).clamp(40.0, 400.0);
        if span > 20_000 + n as i64 * 400 {
            continue;
        }
        let start = c0.t0 - (p0 as f64 * per_char) as i64;
        let end = c1.t1 + ((n - 1 - p1) as f64 * per_char) as i64;
        new[slot] = Some((start.max(0), end.max(start + 1)));
    }
    // interpolate unmatched lines from the offsets of matched neighbours
    let matched: Vec<usize> = (0..idx.len()).filter(|&s| new[s].is_some()).collect();
    report.matched = matched.len();
    if !matched.is_empty() {
        for slot in 0..idx.len() {
            if new[slot].is_some() {
                continue;
            }
            let prev = matched.iter().rev().find(|&&m| m < slot).copied();
            let next = matched.iter().find(|&&m| m > slot).copied();
            let off = |m: usize| new[m].unwrap().0 - old[m].0;
            let o = match (prev, next) {
                (Some(p), Some(q)) => {
                    let f = (old[slot].0 - old[p].0) as f64 / ((old[q].0 - old[p].0) as f64).max(1.0);
                    off(p) + ((off(q) - off(p)) as f64 * f.clamp(0.0, 1.0)) as i64
                }
                (Some(p), None) => off(p),
                (None, Some(q)) => off(q),
                (None, None) => 0,
            };
            new[slot] = Some(((old[slot].0 + o).max(0), (old[slot].1 + o).max(1)));
            report.interpolated.push(idx[slot]);
        }
    }
    for (slot, &i) in idx.iter().enumerate() {
        if let Some((s, e)) = new[slot] {
            sub.events[i].start = s;
            sub.events[i].end = e;
        }
    }
    fix_overlaps(sub, &idx, gap_ms);
    report
}

/// Resolve overlaps between consecutive re-timed lines (same style/layer only) by trimming ends.
fn fix_overlaps(sub: &mut Subtitle, idx: &[usize], gap_ms: i64) {
    let mut order: Vec<usize> = idx.to_vec();
    order.sort_by_key(|&i| sub.events[i].start);
    for w in order.windows(2) {
        let (a, b) = (w[0], w[1]);
        if sub.events[a].style != sub.events[b].style || sub.events[a].layer != sub.events[b].layer {
            continue;
        }
        let bs = sub.events[b].start;
        if sub.events[a].end > bs - gap_ms && sub.events[a].end > bs - gap_ms {
            let e = (bs - gap_ms).max(sub.events[a].start + 200.min((bs - sub.events[a].start).max(1)));
            sub.events[a].end = e.min(sub.events[a].end);
        }
    }
}

// ------------------------------------------------------------------ sync to speech

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SyncOpts {
    /// search range for the global offset (ms, ±)
    pub max_offset_ms: i64,
    /// also estimate a linear drift (frame-rate mismatch)
    pub drift: bool,
    /// snap line edges to speech boundaries within this distance (0 = off)
    pub snap_ms: i64,
    /// keep lines this far past the end of speech when snapping ends
    pub lead_out_ms: i64,
    pub gap_ms: i64,
}

impl Default for SyncOpts {
    fn default() -> Self {
        SyncOpts { max_offset_ms: 120_000, drift: true, snap_ms: 0, lead_out_ms: 250, gap_ms: 84 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncReport {
    /// new_time = old_time * rate + offset_ms
    pub offset_ms: i64,
    pub rate: f64,
    /// normalised correlation peak (0..1); below ~0.15 the result is unreliable
    pub confidence: f64,
    /// (window centre ms, local offset ms) estimates used for the drift fit
    pub windows: Vec<(i64, i64)>,
    pub snapped_starts: usize,
    pub snapped_ends: usize,
    pub lines: usize,
}

const RES: i64 = 10; // ms per sample in the activity signals

fn activity(intervals: impl Iterator<Item = (i64, i64)>, len: usize) -> Vec<f32> {
    let mut v = vec![0f32; len];
    for (a, b) in intervals {
        let a = (a / RES).clamp(0, len as i64) as usize;
        let b = (b / RES).clamp(0, len as i64) as usize;
        for x in &mut v[a..b.max(a)] {
            *x = 1.0;
        }
    }
    v
}

/// Cross-correlation r[lag] = Σ x[t] * y[t - lag] for lag in -max..=max (via FFT).
fn xcorr(x: &[f32], y: &[f32], max_lag: usize) -> Vec<f32> {
    use rustfft::{num_complex::Complex32, FftPlanner};
    let n = (x.len().max(y.len()) + max_lag + 1).next_power_of_two();
    let mut planner = FftPlanner::new();
    let f = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let mut a: Vec<Complex32> = (0..n).map(|i| Complex32::new(*x.get(i).unwrap_or(&0.0), 0.0)).collect();
    let mut b: Vec<Complex32> = (0..n).map(|i| Complex32::new(*y.get(i).unwrap_or(&0.0), 0.0)).collect();
    f.process(&mut a);
    f.process(&mut b);
    for (p, q) in a.iter_mut().zip(&b) {
        *p *= q.conj();
    }
    inv.process(&mut a);
    let scale = 1.0 / n as f32;
    (0..=2 * max_lag)
        .map(|k| {
            let lag = k as i64 - max_lag as i64;
            a[lag.rem_euclid(n as i64) as usize].re * scale
        })
        .collect()
}

fn centred(v: &[f32]) -> Vec<f32> {
    let m = v.iter().sum::<f32>() / v.len().max(1) as f32;
    v.iter().map(|x| x - m).collect()
}

/// Best lag (ms) of `subs` relative to `speech` within ±max_ms, and its normalised score.
fn best_lag(speech: &[f32], subs: &[f32], max_ms: i64) -> (i64, f64) {
    let max_lag = (max_ms / RES).max(1) as usize;
    let (x, y) = (centred(speech), centred(subs));
    let r = xcorr(&x, &y, max_lag);
    let (k, peak) = r.iter().enumerate().fold((0, f32::MIN), |acc, (i, &v)| if v > acc.1 { (i, v) } else { acc });
    let nx = x.iter().map(|v| v * v).sum::<f32>().sqrt();
    let ny = y.iter().map(|v| v * v).sum::<f32>().sqrt();
    let score = if nx > 0.0 && ny > 0.0 { (peak / (nx * ny)) as f64 } else { 0.0 };
    ((k as i64 - max_lag as i64) * RES, score)
}

/// Estimate how the subtitle timing maps onto the detected speech.
pub fn estimate_sync(sub: &Subtitle, speech: &[(i64, i64)], opts: &SyncOpts) -> SyncReport {
    let idx: Vec<usize> = (0..sub.events.len()).filter(|&i| timable(&sub.events[i])).collect();
    let mut rep = SyncReport { rate: 1.0, lines: idx.len(), ..Default::default() };
    if idx.is_empty() || speech.is_empty() {
        return rep;
    }
    let end = speech.last().unwrap().1.max(idx.iter().map(|&i| sub.events[i].end).max().unwrap_or(0)) + opts.max_offset_ms;
    let len = (end / RES) as usize + 1;
    let sp = activity(speech.iter().copied(), len);
    // frame-rate conversions (PAL speed-up etc.) are tried as whole ratios first
    let rates: &[f64] = if opts.drift { &[1.0, 25.0 / 23.976, 23.976 / 25.0, 25.0 / 24.0, 24.0 / 25.0, 24.0 / 23.976, 23.976 / 24.0, 30.0 / 25.0, 25.0 / 30.0] } else { &[1.0] };
    let (mut lag, mut score, mut rate) = (0i64, f64::MIN, 1.0f64);
    for &r in rates {
        let su = activity(idx.iter().map(|&i| ((sub.events[i].start as f64 * r) as i64, (sub.events[i].end as f64 * r) as i64)), len);
        let (l, sc) = best_lag(&sp, &su, opts.max_offset_ms);
        // prefer 1.0 unless another ratio is clearly better
        if sc > score * if r == 1.0 { 1.0 } else { 1.08 } || score == f64::MIN {
            (lag, score, rate) = (l, sc, r);
        }
    }
    rep.offset_ms = lag;
    rep.confidence = score;
    rep.rate = rate;
    if !opts.drift {
        refine_onsets(sub, &idx, speech, &mut rep);
        return rep;
    }
    // local offsets in windows of subtitle time, searched around the global offset
    let map = |t: i64| (t as f64 * rate) as i64;
    let first = idx.iter().map(|&i| map(sub.events[i].start)).min().unwrap();
    let last = idx.iter().map(|&i| map(sub.events[i].end)).max().unwrap();
    let total = last - first;
    let nwin = (total / 120_000).clamp(3, 12);
    let win = total / nwin;
    let mut pts: Vec<(f64, f64, f64)> = Vec::new(); // (t, offset, weight)
    for w in 0..nwin {
        let a = first + w * win;
        let b = a + win;
        let local: Vec<(i64, i64)> = idx.iter().map(|&i| (map(sub.events[i].start), map(sub.events[i].end))).filter(|&(s, _)| s >= a && s < b).map(|(s, e)| (s + lag, e + lag)).collect();
        if local.len() < 6 {
            continue;
        }
        let su_w = activity(local.iter().copied(), len);
        // compare only inside the window (± search range) to keep it local
        let lo = ((a + lag - 15_000).max(0) / RES) as usize;
        let hi = (((b + lag + 15_000) / RES) as usize).min(len);
        let (l2, s2) = best_lag(&sp[lo..hi], &su_w[lo..hi], 12_000);
        if s2 > 0.08 {
            pts.push(((a + b) as f64 / 2.0, (lag + l2) as f64, s2));
            rep.windows.push(((a + b) / 2, lag + l2));
        }
    }
    if pts.len() >= 3 {
        // weighted least squares offset = c + d*t, with one round of outlier rejection
        let fit = |p: &[(f64, f64, f64)]| {
            let sw: f64 = p.iter().map(|q| q.2).sum();
            let mt = p.iter().map(|q| q.0 * q.2).sum::<f64>() / sw;
            let mo = p.iter().map(|q| q.1 * q.2).sum::<f64>() / sw;
            let var = p.iter().map(|q| q.2 * (q.0 - mt).powi(2)).sum::<f64>();
            let cov = p.iter().map(|q| q.2 * (q.0 - mt) * (q.1 - mo)).sum::<f64>();
            let d = if var > 0.0 { cov / var } else { 0.0 };
            (mo - d * mt, d)
        };
        let (c, d) = fit(&pts);
        let kept: Vec<_> = pts.iter().copied().filter(|q| (q.1 - (c + d * q.0)).abs() < 600.0).collect();
        let (c, d) = if kept.len() >= 3 { fit(&kept) } else { (c, d) };
        // windows were measured in rate-mapped time: new = (old*rate)*(1+d) + c
        // only accept a residual drift that is clearly there (>0.05 % ≈ 0.7 s per 23 min)
        if d.abs() > 0.0005 && d.abs() < 0.1 {
            // keep it only if the whole file correlates better with it
            let score_of = |r: f64, o: i64| {
                let su = activity(idx.iter().map(|&i| ((sub.events[i].start as f64 * r) as i64 + o, (sub.events[i].end as f64 * r) as i64 + o)), len);
                let (x, y) = (centred(&sp), centred(&su));
                let dot: f32 = x.iter().zip(&y).map(|(a, b)| a * b).sum();
                let nx = x.iter().map(|v| v * v).sum::<f32>().sqrt();
                let ny = y.iter().map(|v| v * v).sum::<f32>().sqrt();
                (dot / (nx * ny).max(1e-6)) as f64
            };
            let (r2, o2) = (rate * (1.0 + d), c.round() as i64);
            if score_of(r2, o2) > score_of(rate, lag) + 0.01 {
                rep.rate = r2;
                rep.offset_ms = o2;
            }
        }
    }
    refine_onsets(sub, &idx, speech, &mut rep);
    rep
}

/// Interval correlation is biased when lines linger after speech (they usually do), so finish by
/// aligning line starts with speech onsets: median distance to the nearest onset within ±1 s.
fn refine_onsets(sub: &Subtitle, idx: &[usize], speech: &[(i64, i64)], rep: &mut SyncReport) {
    let starts: Vec<i64> = speech.iter().map(|s| s.0).collect();
    let mut d: Vec<i64> = Vec::new();
    for &i in idx {
        let t = (sub.events[i].start as f64 * rep.rate).round() as i64 + rep.offset_ms;
        let k = starts.partition_point(|&x| x < t);
        let best = [k.checked_sub(1), Some(k)].into_iter().flatten().filter_map(|j| starts.get(j)).map(|&x| x - t).min_by_key(|x| x.abs());
        if let Some(x) = best.filter(|x| x.abs() <= 1000) {
            d.push(x);
        }
    }
    if d.len() >= 8 && d.len() * 10 >= idx.len() * 3 {
        d.sort_unstable();
        rep.offset_ms += d[d.len() / 2];
    }
}

/// Apply [`estimate_sync`]'s result (and optional snapping) to the timable lines.
pub fn apply_sync(sub: &mut Subtitle, speech: &[(i64, i64)], rep: &mut SyncReport, opts: &SyncOpts) {
    let idx: Vec<usize> = (0..sub.events.len()).filter(|&i| timable(&sub.events[i])).collect();
    let map = |t: i64| ((t as f64) * rep.rate).round() as i64 + rep.offset_ms;
    for &i in &idx {
        let e = &mut sub.events[i];
        let (s, en) = (map(e.start).max(0), map(e.end).max(1));
        e.start = s;
        e.end = en.max(s + 1);
    }
    if opts.snap_ms > 0 && !speech.is_empty() {
        let starts: Vec<i64> = speech.iter().map(|s| s.0).collect();
        let ends: Vec<i64> = speech.iter().map(|s| s.1).collect();
        let nearest = |v: &[i64], t: i64| -> Option<i64> {
            let k = v.partition_point(|&x| x < t);
            [k.checked_sub(1), Some(k)].into_iter().flatten().filter_map(|j| v.get(j).copied()).min_by_key(|&x| (x - t).abs()).filter(|&x| (x - t).abs() <= opts.snap_ms)
        };
        for &i in &idx {
            let e = &mut sub.events[i];
            if let Some(s) = nearest(&starts, e.start) {
                if s < e.end - 300 {
                    e.start = s;
                    rep.snapped_starts += 1;
                }
            }
            if let Some(x) = nearest(&ends, e.end - opts.lead_out_ms) {
                let ne = x + opts.lead_out_ms;
                if ne > e.start + 300 {
                    e.end = ne;
                    rep.snapped_ends += 1;
                }
            }
        }
        fix_overlaps(sub, &idx, opts.gap_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whisper_sample() -> Vec<u8> {
        // "สวัสดี" split mid-character across tokens, like whisper.cpp does
        let s = "สวัสดี".as_bytes();
        let (t1, t2) = (&s[..4], &s[4..]);
        let mut j = Vec::new();
        j.extend_from_slice(br#"{"result":{"language":"th"},"transcription":[{"offsets":{"from":1000,"to":2600},"text":" "#);
        j.extend_from_slice(s);
        j.extend_from_slice(br#" ok.","tokens":[{"text":"[_BEG_]","offsets":{"from":1000,"to":1000}},{"text":" "#);
        j.extend_from_slice(t1);
        j.extend_from_slice(br#"","offsets":{"from":1000,"to":1500}},{"text":""#);
        j.extend_from_slice(t2);
        j.extend_from_slice(br#"","offsets":{"from":1500,"to":2000}},{"text":" ok.","offsets":{"from":2100,"to":2600}},{"text":"[_TT_130]","offsets":{"from":2600,"to":2600}}]},"#);
        j.extend_from_slice(br#"{"offsets":{"from":3000,"to":4000},"text":" Next line?","tokens":[{"text":" Next","offsets":{"from":3000,"to":3400}},{"text":" line?","offsets":{"from":3500,"to":4000}}]}]}"#);
        j
    }

    #[test]
    fn parses_split_utf8_tokens() {
        let raw = whisper_sample();
        assert!(std::str::from_utf8(&raw).is_err(), "sample must contain split UTF-8");
        let t = parse_whisper_json(&raw).unwrap();
        assert_eq!(t.language, "th");
        assert_eq!(t.segments.len(), 2);
        assert_eq!(t.segments[0].text, "สวัสดี ok.");
        let first = t.segments[0].chars[0];
        assert_eq!(first.c, 'ส');
        assert!(first.t0 >= 1000 && first.t1 <= 1500, "{first:?}");
        let o = t.segments[0].chars.iter().find(|c| c.c == 'o').unwrap();
        assert!(o.t0 >= 2100, "{o:?}");
        let ev = transcript_to_events(&t, SegmentOpts::default());
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].text, "สวัสดี ok.");
        assert!(ev[0].start >= 1000 && ev[0].start < 1200);
        assert!(ev[0].end > 2600 && ev[0].end <= ev[1].start - 84, "{} {}", ev[0].end, ev[1].start);
        assert_eq!(ev[1].text, "Next line?");
    }

    #[test]
    fn align_retimes_shifted_lines() {
        let raw = whisper_sample();
        let t = parse_whisper_json(&raw).unwrap();
        let mut sub = Subtitle::new();
        sub.events = vec![Event::new(9000, 10000, "สวัสดี OK!"), Event::new(10500, 11000, "{\\i1}next line{\\i0}"), Event::new(12000, 13000, "ไม่มีในเสียง")];
        let r = align_text(&mut sub, &t, 0.3, 84);
        assert_eq!(r.matched, 2);
        assert_eq!(r.interpolated, vec![2]);
        assert!((sub.events[0].start - 1000).abs() < 150, "{:?}", sub.events[0]);
        assert!((sub.events[1].start - 3000).abs() < 150 && (sub.events[1].end - 4000).abs() < 150, "{:?}", sub.events[1]);
        // third line keeps its distance to the previous matched line
        assert_eq!(sub.events[2].start, 12000 + (sub.events[1].start - 10500));
    }

    #[test]
    fn sync_finds_offset_and_drift() {
        // pseudo-random speech pattern over 20 minutes
        let mut seed = 7u64;
        let mut rnd = |m: i64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as i64) % m
        };
        let mut speech = Vec::new();
        let mut t = 5_000;
        while t < 1_200_000 {
            let d = 600 + rnd(3500);
            speech.push((t, t + d));
            t += d + 300 + rnd(4000);
        }
        // subtitles: speech mapped by old = (new - 2300) / 1.001  (i.e. new = old*1.001 + 2300), with jitter
        let mut sub = Subtitle::new();
        for &(a, b) in &speech {
            let inv = |x: i64| ((x - 2300) as f64 / 1.001) as i64;
            let (s, e) = (inv(a) + rnd(160) - 80, inv(b) + 200 + rnd(200));
            if s > 0 {
                sub.events.push(Event::new(s, e, "บทพูด"));
            }
        }
        let opts = SyncOpts { snap_ms: 250, ..Default::default() };
        let mut rep = estimate_sync(&sub, &speech, &opts);
        assert!(rep.confidence > 0.5, "{rep:?}");
        assert!((rep.rate - 1.001).abs() < 0.0003, "{rep:?}");
        // check mapped mid-file time instead of raw intercept
        let mid = 600_000.0;
        let want = mid * 1.001 + 2300.0;
        let got = mid * rep.rate + rep.offset_ms as f64;
        assert!((got - want).abs() < 120.0, "{rep:?} got {got} want {want}");
        apply_sync(&mut sub, &speech, &mut rep, &opts);
        let err: Vec<i64> = sub.events.iter().zip(speech.iter().skip_while(|s| s.0 < 2300)).map(|(e, s)| (e.start - s.0).abs()).collect();
        let mean = err.iter().sum::<i64>() as f64 / err.len() as f64;
        assert!(mean < 60.0, "mean start error {mean} ms, {rep:?}");
        assert!(rep.snapped_starts > sub.events.len() / 2);
    }
}
