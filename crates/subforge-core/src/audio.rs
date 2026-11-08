//! Audio analysis shared by the timeline spectrum view and speech sync:
//! a streaming STFT → mel spectrogram (one 8-bit column per 10 ms) and a simple
//! voice-activity detector working on that spectrogram.
//!
//! Input is 16 kHz mono 16-bit PCM (what `ffmpeg -ac 1 -ar 16000 -f s16le` produces).

use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::Arc;

pub const SAMPLE_RATE: usize = 16_000;
/// Hop between columns: 160 samples = 10 ms (same bucket size as the waveform).
pub const HOP: usize = 160;
pub const WIN: usize = 512;
/// Mel bands per column (row 0 = lowest frequency).
pub const ROWS: usize = 96;
pub const COL_MS: i64 = 10;
const F_MIN: f32 = 40.0;
const F_MAX: f32 = 8_000.0;
/// dB range mapped to 0..=255
const DB_FLOOR: f32 = 40.0;
const DB_CEIL: f32 = 125.0;

fn hz_to_mel(f: f32) -> f32 {
    2595.0 * (1.0 + f / 700.0).log10()
}
fn mel_to_hz(m: f32) -> f32 {
    700.0 * (10f32.powf(m / 2595.0) - 1.0)
}

/// Centre frequency (Hz) of a spectrogram row.
pub fn row_hz(row: usize) -> f32 {
    let (a, b) = (hz_to_mel(F_MIN), hz_to_mel(F_MAX));
    mel_to_hz(a + (b - a) * (row as f32 + 1.0) / (ROWS as f32 + 1.0))
}

/// Incremental spectrogram builder. Feed samples with [`push`](Self::push); finished columns are
/// appended to `data` (`ROWS` bytes per column, column `k` centred on `k*10 ms + 5 ms`).
pub struct SpecBuilder {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// (first bin, weights) per mel band
    filters: Vec<(usize, Vec<f32>)>,
    pending: Vec<f32>,
    scratch: Vec<Complex32>,
    fft_scratch: Vec<Complex32>,
    pub data: Vec<u8>,
}

impl Default for SpecBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SpecBuilder {
    pub fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(WIN);
        let window = (0..WIN).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / WIN as f32).cos()).collect();
        // triangular mel filters
        let bins = WIN / 2 + 1;
        let bin_hz = SAMPLE_RATE as f32 / WIN as f32;
        let (ma, mb) = (hz_to_mel(F_MIN), hz_to_mel(F_MAX));
        let pts: Vec<f32> = (0..ROWS + 2).map(|i| mel_to_hz(ma + (mb - ma) * i as f32 / (ROWS + 1) as f32) / bin_hz).collect();
        let mut filters = Vec::with_capacity(ROWS);
        for r in 0..ROWS {
            let (l, c, h) = (pts[r], pts[r + 1], pts[r + 2]);
            let first = l.floor().max(0.0) as usize;
            let last = (h.ceil() as usize).min(bins - 1);
            let mut w = Vec::new();
            for b in first..=last {
                let x = b as f32;
                let v = if x <= c { (x - l) / (c - l).max(1e-3) } else { (h - x) / (h - c).max(1e-3) };
                w.push(v.max(0.0));
            }
            // narrow low bands may fall between bins: make sure each band sees something
            if w.iter().all(|&v| v == 0.0) {
                let nb = (c.round() as usize).clamp(first, last);
                w[nb - first] = 1.0;
            }
            let sum: f32 = w.iter().sum();
            w.iter_mut().for_each(|v| *v /= sum);
            filters.push((first, w));
        }
        let scratch_len = fft.get_inplace_scratch_len();
        SpecBuilder {
            fft,
            window,
            filters,
            // pad so that column k is centred on the middle of 10 ms bucket k
            pending: vec![0.0; WIN / 2 - HOP / 2],
            scratch: vec![Complex32::default(); WIN],
            fft_scratch: vec![Complex32::default(); scratch_len],
            data: Vec::new(),
        }
    }

    pub fn columns(&self) -> usize {
        self.data.len() / ROWS
    }

    pub fn push(&mut self, samples: &[i16]) {
        self.pending.extend(samples.iter().map(|&s| s as f32));
        let mut off = 0;
        while self.pending.len() - off >= WIN {
            self.column(off);
            off += HOP;
        }
        self.pending.drain(..off);
    }

    /// Flush the tail (zero padded) so the last partial 10 ms bucket gets a column.
    pub fn finish(&mut self) {
        let tail = self.pending.len().saturating_sub(WIN / 2 - HOP / 2);
        let n = tail.div_ceil(HOP);
        self.pending.extend(std::iter::repeat_n(0.0, WIN));
        for k in 0..n {
            self.column(k * HOP);
        }
        self.pending.clear();
    }

    fn column(&mut self, off: usize) {
        for i in 0..WIN {
            self.scratch[i] = Complex32::new(self.pending[off + i] * self.window[i], 0.0);
        }
        self.fft.process_with_scratch(&mut self.scratch, &mut self.fft_scratch);
        for (first, w) in &self.filters {
            let mut p = 0.0f32;
            for (k, wk) in w.iter().enumerate() {
                p += self.scratch[first + k].norm_sqr() * wk;
            }
            let db = 10.0 * (p + 1e-3).log10();
            let v = ((db - DB_FLOOR) / (DB_CEIL - DB_FLOOR) * 255.0).clamp(0.0, 255.0);
            self.data.push(v as u8);
        }
    }
}

/// Build a whole spectrogram at once.
pub fn spectrogram(samples: &[i16]) -> Vec<u8> {
    let mut b = SpecBuilder::new();
    for c in samples.chunks(16_000) {
        b.push(c);
    }
    b.finish();
    b.data
}

#[derive(Debug, Clone, Copy)]
pub struct VadOpts {
    /// shortest speech run kept (ms)
    pub min_speech_ms: i64,
    /// pauses shorter than this are bridged (ms)
    pub min_pause_ms: i64,
    /// padding added on both sides (ms)
    pub pad_ms: i64,
    /// threshold sensitivity: 0 = permissive … 1 = strict
    pub strictness: f32,
}

impl Default for VadOpts {
    fn default() -> Self {
        VadOpts { min_speech_ms: 180, min_pause_ms: 220, pad_ms: 40, strictness: 0.5 }
    }
}

/// Voice activity from a spectrogram: energy in the speech band (≈250–3800 Hz) relative to an
/// adaptive noise floor, with hysteresis. Returns sorted, non-overlapping `(start_ms, end_ms)`.
/// Works well for dialogue over quiet backgrounds; loud music is often detected as "speech",
/// which the correlation-based sync tolerates.
pub fn vad(spec: &[u8], opts: VadOpts) -> Vec<(i64, i64)> {
    let cols = spec.len() / ROWS;
    if cols == 0 {
        return Vec::new();
    }
    let rows: Vec<usize> = (0..ROWS).filter(|&r| (250.0..=3800.0).contains(&row_hz(r))).collect();
    let mut e: Vec<f32> = (0..cols)
        .map(|c| {
            let col = &spec[c * ROWS..(c + 1) * ROWS];
            rows.iter().map(|&r| col[r] as f32).sum::<f32>() / rows.len() as f32
        })
        .collect();
    // smooth over 50 ms
    let raw = e.clone();
    for (i, v) in e.iter_mut().enumerate() {
        let a = i.saturating_sub(2);
        let b = (i + 3).min(cols);
        *v = raw[a..b].iter().sum::<f32>() / (b - a) as f32;
    }
    // adaptive floor: local 10th percentile over ±15 s, global percentiles for the range
    let mut sorted = e.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f32| sorted[((sorted.len() - 1) as f32 * p) as usize];
    let (g10, g95) = (pct(0.10), pct(0.95));
    let span = (g95 - g10).max(8.0);
    let block = 300; // 3 s blocks
    let nblocks = cols.div_ceil(block);
    let block_floor: Vec<f32> = (0..nblocks)
        .map(|b| {
            let a = (b * block).saturating_sub(1500);
            let z = ((b + 1) * block + 1500).min(cols);
            let mut v: Vec<f32> = e[a..z].to_vec();
            v.sort_by(|x, y| x.partial_cmp(y).unwrap());
            v[(v.len() - 1) / 10]
        })
        .collect();
    let k_on = 0.22 + 0.25 * opts.strictness.clamp(0.0, 1.0);
    let k_off = k_on * 0.6;
    let mut out: Vec<(i64, i64)> = Vec::new();
    let mut on: Option<usize> = None;
    for (i, &v) in e.iter().enumerate() {
        let floor = block_floor[i / block].max(g10);
        let hi = floor + span * k_on;
        let lo = floor + span * k_off;
        match on {
            None if v >= hi => on = Some(i),
            Some(s) if v < lo => {
                out.push((s as i64 * COL_MS, i as i64 * COL_MS));
                on = None;
            }
            _ => {}
        }
    }
    if let Some(s) = on {
        out.push((s as i64 * COL_MS, cols as i64 * COL_MS));
    }
    // bridge short pauses, drop short blips, pad
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (a, b) in out {
        if let Some(last) = merged.last_mut() {
            if a - last.1 < opts.min_pause_ms {
                last.1 = b;
                continue;
            }
        }
        merged.push((a, b));
    }
    merged.retain(|(a, b)| b - a >= opts.min_speech_ms);
    let end = cols as i64 * COL_MS;
    for m in merged.iter_mut() {
        m.0 = (m.0 - opts.pad_ms).max(0);
        m.1 = (m.1 + opts.pad_ms).min(end);
    }
    merged
}

/// Decode a file's audio to 16 kHz mono s16 with ffmpeg, streaming chunks to `sink`.
/// `sink` returns `false` to cancel. `track` selects the audio stream (`0:a:N`).
pub fn decode_16k(ffmpeg: &std::path::Path, input: &std::path::Path, track: usize, mut sink: impl FnMut(&[i16]) -> bool) -> Result<(), String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut c = crate::extract::command(ffmpeg);
    c.args(["-hide_banner", "-loglevel", "error", "-nostdin"]).arg("-i").arg(input);
    c.args(["-map", &format!("0:a:{track}"), "-vn", "-sn", "-dn", "-ac", "1", "-ar", "16000", "-f", "s16le", "pipe:1"]);
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let errt = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let mut buf = vec![0u8; 64 * 1024];
    let mut carry: Option<u8> = None;
    let mut samples: Vec<i16> = Vec::with_capacity(32 * 1024);
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(e.to_string()),
        };
        samples.clear();
        let mut bytes = &buf[..n];
        if let Some(lo) = carry.take() {
            samples.push(i16::from_le_bytes([lo, bytes[0]]));
            bytes = &bytes[1..];
        }
        let whole = bytes.len() / 2 * 2;
        samples.extend(bytes[..whole].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
        if whole < bytes.len() {
            carry = Some(bytes[whole]);
        }
        if !sink(&samples) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".into());
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let msg = errt.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("ffmpeg: {}", msg.trim()));
    }
    Ok(())
}

/// Write 16 kHz mono s16 samples as a WAV file.
pub fn write_wav(path: &std::path::Path, samples: &[i16]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = (samples.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&(SAMPLE_RATE as u32).to_le_bytes())?;
    f.write_all(&(SAMPLE_RATE as u32 * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tone bursts with a little noise → spectrogram peaks at the right band, VAD finds bursts
    #[test]
    fn spectrogram_and_vad() {
        let mut s = vec![0i16; SAMPLE_RATE * 6];
        let mut seed = 12345u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            ((seed >> 16) as i32 % 200 - 100) as i16
        };
        // bursts: 1.0–2.0 s and 3.5–4.2 s, 1 kHz "voice" + harmonics
        for (i, v) in s.iter_mut().enumerate() {
            let t = i as f32 / SAMPLE_RATE as f32;
            let on = (1.0..2.0).contains(&t) || (3.5..4.2).contains(&t);
            let mut x = rnd() as f32;
            if on {
                for h in 1..4 {
                    x += 6000.0 / h as f32 * (2.0 * std::f32::consts::PI * 330.0 * h as f32 * t).sin();
                }
            }
            *v = x as i16;
        }
        let spec = spectrogram(&s);
        assert_eq!(spec.len() / ROWS, 600);
        // the row nearest 330 Hz is loud inside the burst, quiet outside
        let r = (0..ROWS).min_by(|&a, &b| (row_hz(a) - 330.0).abs().partial_cmp(&(row_hz(b) - 330.0).abs()).unwrap()).unwrap();
        assert!(spec[150 * ROWS + r] > spec[50 * ROWS + r] + 60, "{} vs {}", spec[150 * ROWS + r], spec[50 * ROWS + r]);
        let v = vad(&spec, VadOpts::default());
        assert_eq!(v.len(), 2, "{v:?}");
        assert!((v[0].0 - 1000).abs() <= 80 && (v[0].1 - 2000).abs() <= 80, "{v:?}");
        assert!((v[1].0 - 3500).abs() <= 80 && (v[1].1 - 4200).abs() <= 80, "{v:?}");
    }
}
