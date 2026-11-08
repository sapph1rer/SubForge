//! Image-based subtitles (Blu-ray PGS, DVD VobSub) → text with Tesseract OCR.
//!
//! Packets are read with `ffprobe -show_packets -show_data`, so any container ffmpeg understands
//! works (MKV, M2TS, MP4, `.sup`, `.idx/.sub`). Each bitmap is binarised to black text on white,
//! many bitmaps are stacked into one image per Tesseract run (process start-up dominates
//! otherwise) and words are mapped back by position. Spacing is rebuilt from word boxes, which
//! matters for Thai where Tesseract's own spaces are unreliable.

use crate::{Event, Subtitle};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// One subtitle picture (palette-indexed).
#[derive(Debug, Clone)]
pub struct Bitmap {
    pub start: i64,
    pub end: i64,
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
    /// palette index per pixel
    pub px: Vec<u8>,
    /// RGBA per palette entry
    pub pal: Vec<[u8; 4]>,
    pub forced: bool,
}

impl Bitmap {
    pub fn rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for &i in &self.px {
            out.extend_from_slice(&self.pal.get(i as usize).copied().unwrap_or([0, 0, 0, 0]));
        }
        out
    }
}

#[derive(Debug, Clone, Default)]
pub struct BitmapTrack {
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub items: Vec<Bitmap>,
}

pub fn is_bitmap_codec(c: &str) -> bool {
    matches!(c, "hdmv_pgs_subtitle" | "pgssub" | "dvd_subtitle" | "dvdsub")
}

// ------------------------------------------------------------------ reading packets

/// Parse an ffprobe hexdump ("00000000: 7369 7a65 ...  ascii") into bytes.
pub fn parse_hexdump(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for line in s.lines() {
        let Some(pos) = line.find(": ") else { continue };
        let rest = &line[pos + 2..];
        let hex: String = rest.chars().take(39).filter(|c| !c.is_whitespace()).collect();
        let b = hex.as_bytes();
        let mut i = 0;
        while i + 1 < b.len() {
            let h = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push(h(b[i]) << 4 | h(b[i + 1]));
            i += 2;
        }
    }
    out
}

/// Read a bitmap subtitle stream (`sub_index` among subtitle streams) and decode all pictures.
pub fn read_track(ffprobe: &Path, file: &Path, sub_index: usize) -> Result<BitmapTrack, String> {
    let out = crate::extract::command(ffprobe)
        .args(["-v", "error", "-select_streams", &format!("s:{sub_index}"), "-show_streams", "-show_packets", "-show_data", "-of", "json"])
        .arg(file)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffprobe: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let st = &v["streams"][0];
    let codec = st["codec_name"].as_str().unwrap_or("").to_string();
    if !is_bitmap_codec(&codec) {
        return Err(format!("stream s:{sub_index} is {codec}, not PGS/VobSub"));
    }
    let mut width = st["width"].as_u64().unwrap_or(0) as u32;
    let mut height = st["height"].as_u64().unwrap_or(0) as u32;
    let extradata = parse_hexdump(st["extradata"].as_str().unwrap_or(""));
    let mut packets: Vec<(i64, Option<i64>, Vec<u8>)> = Vec::new();
    for p in v["packets"].as_array().into_iter().flatten() {
        let Some(pts) = p["pts_time"].as_str().and_then(|t| t.parse::<f64>().ok()) else { continue };
        let dur = p["duration_time"].as_str().and_then(|t| t.parse::<f64>().ok()).filter(|d| *d > 0.0 && *d < 3600.0).map(|d| (d * 1000.0).round() as i64);
        packets.push(((pts * 1000.0).round() as i64, dur, parse_hexdump(p["data"].as_str().unwrap_or(""))));
    }
    packets.sort_by_key(|p| p.0);
    let items = if codec.contains("pgs") {
        let (items, w, h) = decode_pgs(&packets);
        if width == 0 {
            (width, height) = (w, h);
        }
        items
    } else {
        let idx = String::from_utf8_lossy(&extradata).to_string();
        let palette = parse_idx_palette(&idx);
        if let Some((w, h)) = parse_idx_size(&idx) {
            if width == 0 {
                (width, height) = (w, h);
            }
        }
        decode_vobsub(&packets, &palette)
    };
    if width == 0 {
        width = items.iter().map(|b| b.x as u32 + b.w as u32).max().unwrap_or(720);
        height = items.iter().map(|b| b.y as u32 + b.h as u32).max().unwrap_or(480);
    }
    Ok(BitmapTrack { width, height, codec, items })
}

// ------------------------------------------------------------------ PGS

fn ycbcr_to_rgba(y: u8, cr: u8, cb: u8, a: u8) -> [u8; 4] {
    // BT.709 limited range (Blu-ray)
    let y = (y as f32 - 16.0) * 1.164;
    let cb = cb as f32 - 128.0;
    let cr = cr as f32 - 128.0;
    let r = y + 1.793 * cr;
    let g = y - 0.213 * cb - 0.533 * cr;
    let b = y + 2.112 * cb;
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    [c(r), c(g), c(b), a]
}

fn pgs_rle(data: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut px = vec![0u8; w * h];
    let (mut x, mut y, mut i) = (0usize, 0usize, 0usize);
    while i < data.len() && y < h {
        let b = data[i];
        i += 1;
        let (len, color) = if b != 0 {
            (1, b)
        } else {
            let Some(&f) = data.get(i) else { break };
            i += 1;
            if f == 0 {
                x = 0;
                y += 1;
                continue;
            }
            let long = f & 0x40 != 0;
            let mut len = (f & 0x3F) as usize;
            if long {
                len = len << 8 | *data.get(i).unwrap_or(&0) as usize;
                i += 1;
            }
            let color = if f & 0x80 != 0 {
                let c = *data.get(i).unwrap_or(&0);
                i += 1;
                c
            } else {
                0
            };
            (len, color)
        };
        for _ in 0..len {
            if x < w {
                px[y * w + x] = color;
            }
            x += 1;
        }
    }
    px
}

/// Decode PGS display sets. Packets contain raw segments (type, u16 size, payload).
pub fn decode_pgs(packets: &[(i64, Option<i64>, Vec<u8>)]) -> (Vec<Bitmap>, u32, u32) {
    struct Obj {
        w: usize,
        h: usize,
        data: Vec<u8>,
    }
    let mut palettes: std::collections::HashMap<u8, Vec<[u8; 4]>> = Default::default();
    let mut objects: std::collections::HashMap<u16, Obj> = Default::default();
    let mut pcs: Option<(u8, Vec<(u16, i32, i32, Option<(i32, i32, usize, usize)>)>, bool)> = None; // palette id, objects, forced
    let (mut vw, mut vh) = (0u32, 0u32);
    let mut out: Vec<Bitmap> = Vec::new();
    let mut open: Option<usize> = None;
    for (pts, _dur, data) in packets {
        let mut i = 0;
        while i + 3 <= data.len() {
            let typ = data[i];
            let size = u16::from_be_bytes([data[i + 1], data[i + 2]]) as usize;
            let p = &data[i + 3..(i + 3 + size).min(data.len())];
            i += 3 + size;
            match typ {
                0x16 if p.len() >= 11 => {
                    vw = u16::from_be_bytes([p[0], p[1]]) as u32;
                    vh = u16::from_be_bytes([p[2], p[3]]) as u32;
                    let state = p[7];
                    if state & 0xC0 != 0 {
                        // epoch start / acquisition point: forget old objects
                        objects.clear();
                    }
                    let pal_id = p[9];
                    let n = p[10] as usize;
                    let mut objs = Vec::new();
                    let mut k = 11;
                    let mut forced = false;
                    for _ in 0..n {
                        if k + 8 > p.len() {
                            break;
                        }
                        let id = u16::from_be_bytes([p[k], p[k + 1]]);
                        let flags = p[k + 3];
                        forced |= flags & 0x40 != 0;
                        let x = u16::from_be_bytes([p[k + 4], p[k + 5]]) as i32;
                        let y = u16::from_be_bytes([p[k + 6], p[k + 7]]) as i32;
                        k += 8;
                        let crop = if flags & 0x80 != 0 && k + 8 <= p.len() {
                            let c = (
                                u16::from_be_bytes([p[k], p[k + 1]]) as i32,
                                u16::from_be_bytes([p[k + 2], p[k + 3]]) as i32,
                                u16::from_be_bytes([p[k + 4], p[k + 5]]) as usize,
                                u16::from_be_bytes([p[k + 6], p[k + 7]]) as usize,
                            );
                            k += 8;
                            Some(c)
                        } else {
                            None
                        };
                        objs.push((id, x, y, crop));
                    }
                    pcs = Some((pal_id, objs, forced));
                }
                0x14 if p.len() >= 2 => {
                    let id = p[0];
                    let pal = palettes.entry(id).or_insert_with(|| vec![[0, 0, 0, 0]; 256]);
                    for e in p[2..].chunks_exact(5) {
                        pal[e[0] as usize] = ycbcr_to_rgba(e[1], e[2], e[3], e[4]);
                    }
                }
                0x15 if p.len() >= 4 => {
                    let id = u16::from_be_bytes([p[0], p[1]]);
                    let seq = p[3];
                    if seq & 0x80 != 0 && p.len() >= 11 {
                        let w = u16::from_be_bytes([p[7], p[8]]) as usize;
                        let h = u16::from_be_bytes([p[9], p[10]]) as usize;
                        objects.insert(id, Obj { w, h, data: p[11..].to_vec() });
                    } else if let Some(o) = objects.get_mut(&id) {
                        o.data.extend_from_slice(&p[4..]);
                    }
                }
                0x80 => {
                    // end of display set: close whatever is on screen, open the new picture
                    let Some((pal_id, objs, forced)) = pcs.take() else { continue };
                    if let Some(o) = open.take() {
                        if out[o].end <= out[o].start {
                            out[o].end = *pts;
                        }
                    }
                    if objs.is_empty() {
                        continue;
                    }
                    let pal = palettes.get(&pal_id).cloned().unwrap_or_else(|| vec![[0, 0, 0, 0]; 256]);
                    // compose all objects into one picture (union box)
                    let mut parts = Vec::new();
                    for (id, x, y, crop) in &objs {
                        let Some(o) = objects.get(id) else { continue };
                        let full = pgs_rle(&o.data, o.w, o.h);
                        let (cx, cy, cw, ch) = crop.unwrap_or((0, 0, o.w, o.h));
                        let (cx, cy) = (cx.max(0) as usize, cy.max(0) as usize);
                        let (cw, ch) = (cw.min(o.w.saturating_sub(cx)), ch.min(o.h.saturating_sub(cy)));
                        let mut px = vec![0u8; cw * ch];
                        for r in 0..ch {
                            px[r * cw..(r + 1) * cw].copy_from_slice(&full[(cy + r) * o.w + cx..(cy + r) * o.w + cx + cw]);
                        }
                        parts.push((*x, *y, cw, ch, px));
                    }
                    if parts.is_empty() {
                        continue;
                    }
                    let x0 = parts.iter().map(|p| p.0).min().unwrap();
                    let y0 = parts.iter().map(|p| p.1).min().unwrap();
                    let x1 = parts.iter().map(|p| p.0 + p.2 as i32).max().unwrap();
                    let y1 = parts.iter().map(|p| p.1 + p.3 as i32).max().unwrap();
                    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
                    let mut px = vec![0u8; w * h];
                    for (x, y, pw, ph, data) in parts {
                        for r in 0..ph {
                            let dst = (y - y0) as usize + r;
                            let dx = (x - x0) as usize;
                            px[dst * w + dx..dst * w + dx + pw].copy_from_slice(&data[r * pw..(r + 1) * pw]);
                        }
                    }
                    out.push(Bitmap { start: *pts, end: 0, x: x0, y: y0, w, h, px, pal, forced });
                    open = Some(out.len() - 1);
                }
                _ => {}
            }
        }
    }
    // a picture never cleared: give it a sensible duration
    for b in out.iter_mut() {
        if b.end <= b.start {
            b.end = b.start + 4000;
        }
    }
    // drop duplicates produced by palette-only updates (same picture shown again back-to-back)
    let mut merged: Vec<Bitmap> = Vec::new();
    for b in out {
        if let Some(l) = merged.last_mut() {
            if l.end == b.start && l.px == b.px && l.x == b.x && l.y == b.y {
                l.end = b.end;
                continue;
            }
        }
        merged.push(b);
    }
    (merged, vw, vh)
}

// ------------------------------------------------------------------ VobSub

pub fn parse_idx_palette(idx: &str) -> [[u8; 3]; 16] {
    let mut pal = [[0u8; 3]; 16];
    for l in idx.lines() {
        if let Some(rest) = l.trim().strip_prefix("palette:") {
            for (i, c) in rest.split(',').take(16).enumerate() {
                let v = u32::from_str_radix(c.trim(), 16).unwrap_or(0);
                pal[i] = [(v >> 16) as u8, (v >> 8) as u8, v as u8];
            }
        }
    }
    pal
}

fn parse_idx_size(idx: &str) -> Option<(u32, u32)> {
    let l = idx.lines().find_map(|l| l.trim().strip_prefix("size:"))?;
    let (w, h) = l.trim().split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

struct Nibbles<'a> {
    d: &'a [u8],
    pos: usize, // in nibbles
}

impl Nibbles<'_> {
    fn next(&mut self) -> u16 {
        let b = *self.d.get(self.pos / 2).unwrap_or(&0);
        let v = if self.pos % 2 == 0 { b >> 4 } else { b & 0xF };
        self.pos += 1;
        v as u16
    }
    fn align(&mut self) {
        self.pos = self.pos.div_ceil(2) * 2;
    }
}

fn spu_field(data: &[u8], start: usize, w: usize, rows: impl Iterator<Item = usize>, px: &mut [u8]) {
    let mut n = Nibbles { d: data, pos: start * 2 };
    for y in rows {
        let mut x = 0;
        while x < w {
            let mut v = n.next();
            if v < 0x4 {
                v = v << 4 | n.next();
                if v < 0x10 {
                    v = v << 4 | n.next();
                    if v < 0x40 {
                        v = v << 4 | n.next();
                    }
                }
            }
            let color = (v & 3) as u8;
            let mut run = (v >> 2) as usize;
            if run == 0 {
                run = w - x;
            }
            for _ in 0..run.min(w - x) {
                px[y * w + x] = color;
                x += 1;
            }
        }
        n.align();
    }
}

/// Decode DVD subpicture units (one per packet).
pub fn decode_vobsub(packets: &[(i64, Option<i64>, Vec<u8>)], palette: &[[u8; 3]; 16]) -> Vec<Bitmap> {
    let mut out: Vec<Bitmap> = Vec::new();
    for (pts, dur, d) in packets {
        if d.len() < 4 {
            continue;
        }
        let ctrl = u16::from_be_bytes([d[2], d[3]]) as usize;
        let (mut colors, mut alpha) = ([0u8; 4], [0u8; 4]);
        let (mut x1, mut x2, mut y1, mut y2) = (0usize, 0usize, 0usize, 0usize);
        let (mut off_top, mut off_bot) = (0usize, 0usize);
        let (mut t_start, mut t_stop): (Option<i64>, Option<i64>) = (None, None);
        let mut forced = false;
        let mut seq = ctrl;
        for _ in 0..32 {
            if seq + 4 > d.len() {
                break;
            }
            let delay = u16::from_be_bytes([d[seq], d[seq + 1]]) as i64 * 1024 * 1000 / 90_000;
            let next = u16::from_be_bytes([d[seq + 2], d[seq + 3]]) as usize;
            let mut i = seq + 4;
            while i < d.len() {
                let cmd = d[i];
                i += 1;
                match cmd {
                    0x00 => {
                        forced = true;
                        t_start.get_or_insert(delay);
                    }
                    0x01 => {
                        t_start.get_or_insert(delay);
                    }
                    0x02 => t_stop = Some(delay),
                    0x03 if i + 2 <= d.len() => {
                        colors = [d[i + 1] & 0xF, d[i + 1] >> 4, d[i] & 0xF, d[i] >> 4];
                        i += 2;
                    }
                    0x04 if i + 2 <= d.len() => {
                        alpha = [d[i + 1] & 0xF, d[i + 1] >> 4, d[i] & 0xF, d[i] >> 4];
                        i += 2;
                    }
                    0x05 if i + 6 <= d.len() => {
                        x1 = ((d[i] as usize) << 4) | (d[i + 1] as usize >> 4);
                        x2 = ((d[i + 1] as usize & 0xF) << 8) | d[i + 2] as usize;
                        y1 = ((d[i + 3] as usize) << 4) | (d[i + 4] as usize >> 4);
                        y2 = ((d[i + 4] as usize & 0xF) << 8) | d[i + 5] as usize;
                        i += 6;
                    }
                    0x06 if i + 4 <= d.len() => {
                        off_top = u16::from_be_bytes([d[i], d[i + 1]]) as usize;
                        off_bot = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
                        i += 4;
                    }
                    0xFF => break,
                    _ => break, // unknown command: stop this sequence
                }
            }
            if next == seq || next >= d.len() {
                break;
            }
            seq = next;
        }
        if x2 < x1 || y2 < y1 || off_top == 0 {
            continue;
        }
        let (w, h) = (x2 - x1 + 1, y2 - y1 + 1);
        let mut px = vec![0u8; w * h];
        spu_field(d, off_top, w, (0..h).step_by(2), &mut px);
        spu_field(d, off_bot, w, (1..h).step_by(2), &mut px);
        let pal: Vec<[u8; 4]> = (0..4)
            .map(|k| {
                let c = palette[colors[k] as usize & 15];
                [c[0], c[1], c[2], alpha[k] * 17]
            })
            .collect();
        let start = pts + t_start.unwrap_or(0);
        let end = match (t_stop, dur) {
            (Some(s), _) => pts + s,
            (None, Some(d)) => pts + d,
            _ => start + 4000,
        };
        out.push(Bitmap { start, end: end.max(start + 1), x: x1 as i32, y: y1 as i32, w, h, px, pal, forced });
    }
    // pictures replace each other: never overlap the next one; absurd stop delays
    // (some encoders write ~0xFFFF) fall back to the next picture or 4 s
    for i in 0..out.len() {
        let next = out.get(i + 1).map(|b| b.start);
        let b = &mut out[i];
        if let Some(ns) = next {
            if b.end > ns {
                b.end = ns;
            }
        }
        if b.end - b.start > 30_000 {
            b.end = next.map(|ns| ns.min(b.start + 8000)).unwrap_or(b.start + 4000);
        }
    }
    out
}

// ------------------------------------------------------------------ binarisation

/// 8-bit grey image, black text (0) on white (255).
#[derive(Debug, Clone)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Gray {
    pub fn write_pgm(&self, path: &Path) -> std::io::Result<()> {
        let mut v = format!("P5\n{} {}\n255\n", self.w, self.h).into_bytes();
        v.extend_from_slice(&self.data);
        std::fs::write(path, v)
    }
}

/// Black-on-white text image for OCR. The text colour is told apart from the outline/shadow by
/// brightness (Otsu split) and by which class touches transparency more (the outline does).
pub fn binarize(b: &Bitmap, scale: usize, pad: usize) -> Gray {
    let lum: Vec<f32> = b.pal.iter().map(|c| 0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32).collect();
    let opaque = |i: u8| b.pal.get(i as usize).map(|c| c[3] >= 110).unwrap_or(false);
    // Otsu over opaque pixels
    let mut hist = [0u32; 256];
    for &i in &b.px {
        if opaque(i) {
            hist[lum[i as usize] as usize] += 1;
        }
    }
    let total: u32 = hist.iter().sum();
    let mut thr = 128usize;
    if total > 0 {
        let sum: f64 = hist.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum();
        let (mut wb, mut sb, mut best) = (0f64, 0f64, -1f64);
        for (t, &c) in hist.iter().enumerate() {
            wb += c as f64;
            if wb == 0.0 {
                continue;
            }
            let wf = total as f64 - wb;
            if wf == 0.0 {
                break;
            }
            sb += t as f64 * c as f64;
            let (mb, mf) = (sb / wb, (sum - sb) / wf);
            let between = wb * wf * (mb - mf).powi(2);
            if between > best {
                best = between;
                thr = t;
            }
        }
    }
    let is_bright = |i: u8| opaque(i) && lum[i as usize] as usize > thr;
    let is_dark = |i: u8| opaque(i) && lum[i as usize] as usize <= thr;
    // which class touches transparent pixels more → that one is the outline
    let (mut bt, mut bn, mut dt, mut dn) = (0u32, 0u32, 0u32, 0u32);
    let (w, h) = (b.w, b.h);
    for y in 0..h {
        for x in 0..w {
            let i = b.px[y * w + x];
            if !opaque(i) {
                continue;
            }
            let touches = [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)].iter().any(|&(nx, ny)| nx >= w || ny >= h || !opaque(b.px[ny * w + nx]));
            if is_bright(i) {
                bn += 1;
                bt += touches as u32;
            } else {
                dn += 1;
                dt += touches as u32;
            }
        }
    }
    let frac = |t: u32, n: u32| if n == 0 { 1.0 } else { t as f64 / n as f64 };
    // without an outline (only one class), the opaque pixels are the text
    let text_bright = if dn * 20 < bn || bn * 20 < dn { bn >= dn } else { frac(bt, bn) <= frac(dt, dn) };
    let is_text = |i: u8| if dn * 20 < bn || bn * 20 < dn { opaque(i) } else if text_bright { is_bright(i) } else { is_dark(i) };
    let (ow, oh) = (w * scale + 2 * pad, h * scale + 2 * pad);
    let mut data = vec![255u8; ow * oh];
    for y in 0..h {
        for x in 0..w {
            if is_text(b.px[y * w + x]) {
                for sy in 0..scale {
                    let row = (pad + y * scale + sy) * ow + pad + x * scale;
                    data[row..row + scale].fill(0);
                }
            }
        }
    }
    Gray { w: ow, h: oh, data }
}

// ------------------------------------------------------------------ OCR

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrOpts {
    pub tesseract: PathBuf,
    /// Tesseract languages, e.g. `tha+eng`
    pub lang: String,
    /// page segmentation mode for each text line (7 = single line)
    pub psm: u8,
    /// 0 = automatic (scale small DVD bitmaps up)
    pub scale: usize,
    /// parallel Tesseract processes (0 = CPU count)
    pub threads: usize,
    /// user replacement list applied after OCR (whole-string replace, in order)
    pub replace: Vec<(String, String)>,
    /// extra tessdata directory (`--tessdata-dir`)
    pub tessdata: Option<PathBuf>,
}

impl Default for OcrOpts {
    fn default() -> Self {
        OcrOpts { tesseract: PathBuf::from("tesseract"), lang: "tha+eng".into(), psm: 7, scale: 0, threads: 0, replace: Vec::new(), tessdata: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrLine {
    pub start: i64,
    pub end: i64,
    pub text: String,
    /// lowest word confidence (0..100); low values deserve a look
    pub confidence: f32,
    /// picture is in the upper part of the frame
    pub top: bool,
    pub forced: bool,
    /// index into the track's bitmaps
    pub image: usize,
}

/// Languages installed for a Tesseract executable.
pub fn tesseract_languages(tesseract: &Path, tessdata: Option<&Path>) -> Result<Vec<String>, String> {
    let mut c = crate::extract::command(tesseract);
    if let Some(d) = tessdata {
        c.arg("--tessdata-dir").arg(d);
    }
    let out = c.arg("--list-langs").stdin(Stdio::null()).output().map_err(|e| format!("cannot run {}: {e}", tesseract.display()))?;
    let s = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    Ok(s.lines().skip_while(|l| !l.contains("List of available languages")).skip(1).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

pub fn locate_tesseract(hint_dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = hint_dirs.to_vec();
    if cfg!(windows) {
        for v in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(p) = std::env::var_os(v) {
                dirs.push(PathBuf::from(&p).join("Tesseract-OCR"));
                dirs.push(PathBuf::from(&p).join("Programs").join("Tesseract-OCR"));
            }
        }
    }
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    let name = if cfg!(windows) { "tesseract.exe" } else { "tesseract" };
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Split a binarised picture into text lines (horizontal projection). Thin bands — Thai
/// vowel/tone marks floating above or below the letters — are merged into their line.
pub fn split_lines(g: &Gray, pad: usize) -> Vec<Gray> {
    let rows: Vec<usize> = (0..g.h).map(|y| g.data[y * g.w..(y + 1) * g.w].iter().filter(|&&v| v == 0).count()).collect();
    let mut bands: Vec<(usize, usize)> = Vec::new();
    let mut y = 0;
    while y < g.h {
        if rows[y] > 0 {
            let a = y;
            while y < g.h && rows[y] > 0 {
                y += 1;
            }
            bands.push((a, y));
        } else {
            y += 1;
        }
    }
    if bands.is_empty() {
        return Vec::new();
    }
    loop {
        let maxh = bands.iter().map(|b| b.1 - b.0).max().unwrap_or(1);
        // merge the thinnest band (or one separated by a tiny gap) into its closest neighbour
        let cand = (0..bands.len()).filter(|&i| bands[i].1 - bands[i].0 < maxh * 45 / 100).min_by_key(|&i| bands[i].1 - bands[i].0);
        let tiny_gap = (1..bands.len()).find(|&i| bands[i].0 - bands[i - 1].1 <= (maxh / 12).max(1));
        let (i, j) = if let Some(k) = tiny_gap {
            (k - 1, k)
        } else if let Some(i) = cand {
            if bands.len() == 1 {
                break;
            }
            let up = if i > 0 { Some(bands[i].0 - bands[i - 1].1) } else { None };
            let down = if i + 1 < bands.len() { Some(bands[i + 1].0 - bands[i].1) } else { None };
            match (up, down) {
                (Some(u), Some(d)) if u <= d => (i - 1, i),
                (Some(_), None) => (i - 1, i),
                _ => (i, i + 1),
            }
        } else {
            break;
        };
        bands[i].1 = bands[j].1;
        bands.remove(j);
    }
    bands
        .into_iter()
        .map(|(a, b)| {
            // tight horizontal crop
            let (mut x0, mut x1) = (g.w, 0);
            for y in a..b {
                let r = &g.data[y * g.w..(y + 1) * g.w];
                if let Some(f) = r.iter().position(|&v| v == 0) {
                    x0 = x0.min(f);
                    x1 = x1.max(r.iter().rposition(|&v| v == 0).unwrap() + 1);
                }
            }
            let (w, h) = (x1 - x0 + 2 * pad, b - a + 2 * pad);
            let mut data = vec![255u8; w * h];
            for y in a..b {
                data[(y - a + pad) * w + pad..(y - a + pad) * w + pad + (x1 - x0)].copy_from_slice(&g.data[y * g.w + x0..y * g.w + x1]);
            }
            Gray { w, h, data }
        })
        .collect()
}

/// Run OCR on all bitmaps. `progress(done, total)` returns `false` to cancel.
/// With a spell checker, Thai words Tesseract got slightly wrong (a missing or extra
/// tone mark, า/ำ) are corrected when exactly one small edit gives dictionary words.
pub fn ocr(track: &BitmapTrack, opts: &OcrOpts, work_dir: &Path, speller: Option<&crate::spell::SpellChecker>, progress: &(dyn Fn(usize, usize) -> bool + Sync)) -> Result<Vec<OcrLine>, String> {
    std::fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let n = track.items.len();
    let scale = if opts.scale > 0 { opts.scale } else if track.height < 700 { 3 } else { 1 };
    const BATCH: usize = 24;
    let batches: Vec<(usize, usize)> = (0..n).step_by(BATCH).map(|a| (a, (a + BATCH).min(n))).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let cancelled = AtomicBool::new(false);
    let results: std::sync::Mutex<Vec<Option<(String, f32)>>> = std::sync::Mutex::new(vec![None; n]);
    let first_err: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let threads = if opts.threads > 0 { opts.threads } else { std::thread::available_parallelism().map(|v| v.get()).unwrap_or(2) }.min(batches.len().max(1));
    let fail = |e: String| {
        first_err.lock().unwrap().get_or_insert(e);
        cancelled.store(true, Ordering::SeqCst);
    };
    std::thread::scope(|sc| {
        for t in 0..threads {
            let (next, done, cancelled, results, batches, fail) = (&next, &done, &cancelled, &results, &batches, &fail);
            sc.spawn(move || loop {
                let b = next.fetch_add(1, Ordering::SeqCst);
                if b >= batches.len() || cancelled.load(Ordering::SeqCst) {
                    break;
                }
                let (a, z) = batches[b];
                // one image file per text line; a list file feeds them all to one Tesseract run
                let mut owner: Vec<usize> = Vec::new();
                let mut list = String::new();
                let dir = work_dir.join(format!("t{t}"));
                let _ = std::fs::create_dir_all(&dir);
                for i in a..z {
                    let g = binarize(&track.items[i], scale, 0);
                    for (k, line) in split_lines(&g, 16).into_iter().enumerate() {
                        let f = dir.join(format!("{i}-{k}.pgm"));
                        if let Err(e) = line.write_pgm(&f) {
                            fail(e.to_string());
                            return;
                        }
                        list.push_str(&f.to_string_lossy());
                        list.push('\n');
                        owner.push(i);
                    }
                }
                if owner.is_empty() {
                    done.fetch_add(z - a, Ordering::SeqCst);
                    continue;
                }
                let list_file = dir.join("list.txt");
                let base = dir.join("out");
                if let Err(e) = std::fs::write(&list_file, &list) {
                    fail(e.to_string());
                    return;
                }
                let mut c = crate::extract::command(&opts.tesseract);
                c.arg(&list_file).arg(&base);
                if let Some(d) = &opts.tessdata {
                    c.arg("--tessdata-dir").arg(d);
                }
                c.args(["-l", &opts.lang, "--psm", &opts.psm.to_string(), "-c", "preserve_interword_spaces=1", "txt", "tsv"]);
                c.env("OMP_THREAD_LIMIT", "1");
                match c.stdin(Stdio::null()).stdout(Stdio::null()).output() {
                    Ok(o) if o.status.success() => {}
                    Ok(o) => {
                        fail(format!("tesseract: {}", String::from_utf8_lossy(&o.stderr).trim()));
                        return;
                    }
                    Err(e) => {
                        fail(format!("cannot run {}: {e}", opts.tesseract.display()));
                        return;
                    }
                }
                let txt = std::fs::read_to_string(base.with_extension("txt")).unwrap_or_default();
                let tsv = std::fs::read_to_string(base.with_extension("tsv")).unwrap_or_default();
                // per-page minimum word confidence
                let mut conf = vec![100f32; owner.len()];
                let mut seen = vec![false; owner.len()];
                for l in tsv.lines().skip(1) {
                    let f: Vec<&str> = l.splitn(12, '\t').collect();
                    if f.len() < 12 || f[0] != "5" || f[11].trim().is_empty() {
                        continue;
                    }
                    let page = f[1].trim().parse::<usize>().unwrap_or(1).saturating_sub(1);
                    if let (Some(c), Ok(v)) = (conf.get_mut(page), f[10].trim().parse::<f32>()) {
                        *c = c.min(v);
                        seen[page] = true;
                    }
                }
                let pages: Vec<&str> = txt.split('\u{0C}').collect();
                let mut res = results.lock().unwrap();
                for (p, &i) in owner.iter().enumerate() {
                    let text = pages.get(p).map(|t| t.trim()).unwrap_or("").replace('\n', " ");
                    let c = if seen[p] { conf[p] } else { 0.0 };
                    // stray specks: very short, very unsure
                    if text.is_empty() || (c < 40.0 && text.chars().filter(|c| c.is_alphanumeric()).count() <= 2) {
                        continue;
                    }
                    let entry = res[i].get_or_insert((String::new(), 100.0));
                    if !entry.0.is_empty() {
                        entry.0.push_str("\\N");
                    }
                    entry.0.push_str(&text);
                    entry.1 = entry.1.min(c);
                }
                drop(res);
                let _ = std::fs::remove_dir_all(&dir);
                let d = done.fetch_add(z - a, Ordering::SeqCst) + (z - a);
                if !progress(d, n) {
                    cancelled.store(true, Ordering::SeqCst);
                }
            });
        }
    });
    if let Some(e) = first_err.into_inner().unwrap() {
        return Err(e);
    }
    if cancelled.load(Ordering::SeqCst) && done.load(Ordering::SeqCst) < n {
        return Err("cancelled".into());
    }
    let results = results.into_inner().unwrap();
    let h = track.height.max(1) as i32;
    Ok(track
        .items
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let (text, confidence) = results[i].clone().unwrap_or((String::new(), 0.0));
            let mut text = cleanup(&text, &opts.replace);
            if let Some(sp) = speller {
                text = sp.fix_thai_ocr(&text);
            }
            OcrLine { start: b.start, end: b.end, text, confidence, top: b.y + (b.h as i32) / 2 < h * 2 / 5, forced: b.forced, image: i }
        })
        .collect())
}

/// Typical OCR damage repair + the user's replacement list.
pub fn cleanup(text: &str, replace: &[(String, String)]) -> String {
    let mut s = crate::fixes::normalize_thai(text);
    // common Latin OCR confusions inside Thai text
    for (a, b) in [("|", "I"), ("‘", "'"), ("’", "'"), ("“", "\""), ("”", "\"")] {
        s = s.replace(a, b);
    }
    for (a, b) in replace {
        if !a.is_empty() {
            s = s.replace(a.as_str(), b);
        }
    }
    s.split("\\N").map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\\N")
}

/// Build a subtitle from reviewed OCR lines (top pictures get `{\an8}`).
pub fn to_subtitle(lines: &[OcrLine]) -> Subtitle {
    let mut sub = Subtitle::new();
    sub.ensure_defaults();
    sub.events = lines
        .iter()
        .filter(|l| !l.text.trim().is_empty())
        .map(|l| {
            let t = if l.top { format!("{{\\an8}}{}", l.text) } else { l.text.clone() };
            Event::new(l.start, l.end, t)
        })
        .collect();
    sub
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hexdump() {
        let s = "\n00000000: 7369 7a65 3a20 3139 3230 7831 3038 300a  size: 1920x1080.\n00000010: 2061 6161 0a                             aaa.\n";
        assert_eq!(parse_hexdump(s), b"size: 1920x1080\n aaa\n".to_vec());
    }

    #[test]
    fn pgs_rle_roundtrip() {
        // 4x2: [1,1,1,0] / [0,0,0,0] ; codes: 00 83 01 (3x color1), 00 01 (1x0), 00 00 eol, 00 04, 00 00
        let data = [0, 0x83, 1, 0, 1, 0, 0, 0, 4, 0, 0];
        assert_eq!(pgs_rle(&data, 4, 2), vec![1, 1, 1, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn binarize_picks_fill_not_outline() {
        // white 'I' with black outline on transparent
        let (w, h) = (5, 7);
        let mut px = vec![0u8; w * h];
        for y in 1..6 {
            for x in 1..4 {
                px[y * w + x] = 2; // outline
            }
        }
        for y in 2..5 {
            px[y * w + 2] = 1; // fill
        }
        let b = Bitmap { start: 0, end: 1, x: 0, y: 0, w, h, px, pal: vec![[0, 0, 0, 0], [255, 255, 255, 255], [0, 0, 0, 255]], forced: false };
        let g = binarize(&b, 1, 0);
        let black: Vec<usize> = (0..w * h).filter(|&i| g.data[i] == 0).collect();
        assert_eq!(black, vec![2 * w + 2, 3 * w + 2, 4 * w + 2]);
    }
}
