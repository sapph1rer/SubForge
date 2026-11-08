//! Motion tracking: follow one or two points through video frames and attach subtitle lines to
//! them (frame-by-frame `\pos`, plus `\fscx`/`\fscy`/`\frz` when two points are tracked).
//!
//! The tracker uses zero-mean normalised cross-correlation between a patch around the point and
//! a search window in the next frame, with velocity prediction (fast pans), bilinear sampling
//! (no rounding drift) and parabolic sub-pixel refinement of the correlation peak.

use crate::anim::{Applied, EFFECT_GEN, EFFECT_SRC};
use crate::model::{Event, Subtitle};
use crate::tags::{self, remove_tags};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::Stdio;

/// An 8-bit grayscale frame.
#[derive(Clone)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Gray {
    #[inline]
    fn px(&self, x: i64, y: i64) -> f32 {
        let x = x.clamp(0, self.w as i64 - 1) as usize;
        let y = y.clamp(0, self.h as i64 - 1) as usize;
        self.data[y * self.w + x] as f32
    }
    /// Bilinear sample of a `w`×`h` block whose top-left is at (x0, y0).
    fn sample(&self, x0: f32, y0: f32, w: usize, h: usize, out: &mut Vec<f32>) {
        out.clear();
        out.reserve(w * h);
        let (ix, iy) = (x0.floor(), y0.floor());
        let (fx, fy) = (x0 - ix, y0 - iy);
        let (ix, iy) = (ix as i64, iy as i64);
        let (w00, w10, w01, w11) = ((1.0 - fx) * (1.0 - fy), fx * (1.0 - fy), (1.0 - fx) * fy, fx * fy);
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let (sx, sy) = (ix + x, iy + y);
                out.push(self.px(sx, sy) * w00 + self.px(sx + 1, sy) * w10 + self.px(sx, sy + 1) * w01 + self.px(sx + 1, sy + 1) * w11);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TrackParams {
    /// Patch size in pixels (odd)
    pub patch: usize,
    /// Search radius in pixels around the predicted position
    pub search: usize,
    /// Correlation below this marks the frame as unreliable
    pub min_score: f32,
}

impl Default for TrackParams {
    fn default() -> Self {
        TrackParams { patch: 31, search: 32, min_score: 0.6 }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    pub x: f32,
    pub y: f32,
    /// Normalised correlation of the match (1 = identical)
    pub score: f32,
}

/// Incremental single-point tracker.
pub struct PointTracker {
    params: TrackParams,
    pos: (f32, f32),
    vel: (f32, f32),
    tpl: Vec<f32>,
    tpl_mean: f32,
    tpl_norm: f32,
    /// template from the reference frame (used to cancel drift of the adaptive template)
    ref_tpl: Vec<f32>,
    ref_mean: f32,
    ref_norm: f32,
    region: Vec<f32>,
    buf: Vec<f32>,
}

fn stats(v: &[f32]) -> (f32, f32) {
    let n = v.len() as f32;
    let mean = v.iter().sum::<f32>() / n;
    let var: f32 = v.iter().map(|x| (x - mean) * (x - mean)).sum();
    (mean, var.sqrt())
}

impl PointTracker {
    /// Start tracking `pos` (frame pixel coordinates) in `frame`.
    pub fn new(frame: &Gray, pos: (f32, f32), params: TrackParams) -> PointTracker {
        let mut t = PointTracker {
            params: TrackParams { patch: params.patch | 1, ..params },
            pos,
            vel: (0.0, 0.0),
            tpl: Vec::new(),
            tpl_mean: 0.0,
            tpl_norm: 0.0,
            ref_tpl: Vec::new(),
            ref_mean: 0.0,
            ref_norm: 0.0,
            region: Vec::new(),
            buf: Vec::new(),
        };
        t.grab_template(frame);
        t.ref_tpl = t.tpl.clone();
        t.ref_mean = t.tpl_mean;
        t.ref_norm = t.tpl_norm;
        t
    }

    fn grab_template(&mut self, frame: &Gray) {
        let n = self.params.patch;
        let half = (n / 2) as f32;
        frame.sample(self.pos.0 - half, self.pos.1 - half, n, n, &mut self.buf);
        let (m, s) = stats(&self.buf);
        self.tpl = self.buf.clone();
        self.tpl_mean = m;
        self.tpl_norm = s.max(1e-3);
    }

    /// Find the point in the next frame.
    pub fn step(&mut self, frame: &Gray) -> Sample {
        let pred = (self.pos.0 + self.vel.0, self.pos.1 + self.vel.1);
        let tpl = std::mem::take(&mut self.tpl);
        let (new, bs) = self.search(frame, pred, self.params.search, &tpl, self.tpl_mean, self.tpl_norm);
        self.tpl = tpl;
        // drift correction: re-match the original template close to the result
        let rtpl = std::mem::take(&mut self.ref_tpl);
        let (fixed, rs) = self.search(frame, new, 3, &rtpl, self.ref_mean, self.ref_norm);
        self.ref_tpl = rtpl;
        let (new, bs) = if rs >= 0.8 && rs >= bs - 0.1 { (fixed, bs.max(rs)) } else { (new, bs) };
        let reliable = bs >= self.params.min_score;
        if reliable {
            // smoothed velocity for the next prediction
            let v = (new.0 - self.pos.0, new.1 - self.pos.1);
            self.vel = (self.vel.0 * 0.3 + v.0 * 0.7, self.vel.1 * 0.3 + v.1 * 0.7);
            self.pos = new;
            // adapt the template to gradual appearance changes
            self.grab_template(frame);
        } else {
            // keep coasting on the last velocity; keep the last good template
            self.pos = pred;
        }
        Sample { x: self.pos.0, y: self.pos.1, score: bs }
    }

    /// NCC search of `tpl` around `center` (±`r` px); returns the sub-pixel best position and score.
    fn search(&mut self, frame: &Gray, center: (f32, f32), r: usize, tpl: &[f32], tpl_mean: f32, tpl_norm: f32) -> ((f32, f32), f32) {
        let n = self.params.patch;
        let half = (n / 2) as f32;
        let pred = center;
        let rw = n + 2 * r;
        // the region keeps the predicted position's fractional offset, so candidates sit on an integer grid
        let mut region = std::mem::take(&mut self.region);
        frame.sample(pred.0 - half - r as f32, pred.1 - half - r as f32, rw, rw, &mut region);
        let side = 2 * r + 1;
        let mut scores = vec![-1.0f32; side * side];
        // integral images for fast window mean/variance
        let mut sum = vec![0f64; (rw + 1) * (rw + 1)];
        let mut sq = vec![0f64; (rw + 1) * (rw + 1)];
        for y in 0..rw {
            let mut rs = 0f64;
            let mut rq = 0f64;
            for x in 0..rw {
                let v = region[y * rw + x] as f64;
                rs += v;
                rq += v * v;
                sum[(y + 1) * (rw + 1) + x + 1] = sum[y * (rw + 1) + x + 1] + rs;
                sq[(y + 1) * (rw + 1) + x + 1] = sq[y * (rw + 1) + x + 1] + rq;
            }
        }
        let rect = |t: &[f64], x: usize, y: usize| t[(y + n) * (rw + 1) + x + n] - t[y * (rw + 1) + x + n] - t[(y + n) * (rw + 1) + x] + t[y * (rw + 1) + x];
        let nn = (n * n) as f64;
        let tz: Vec<f32> = tpl.iter().map(|v| v - tpl_mean).collect();
        let mut best = (r, r, -2.0f32);
        for dy in 0..side {
            for dx in 0..side {
                let s = rect(&sum, dx, dy);
                let q = rect(&sq, dx, dy);
                let var = (q - s * s / nn).max(0.0);
                if var < 1e-6 {
                    continue;
                }
                let mut acc = 0f32;
                for py in 0..n {
                    let row = &region[(dy + py) * rw + dx..(dy + py) * rw + dx + n];
                    let trow = &tz[py * n..py * n + n];
                    for k in 0..n {
                        acc += row[k] * trow[k];
                    }
                }
                let score = acc / (tpl_norm * var.sqrt() as f32);
                scores[dy * side + dx] = score;
                if score > best.2 {
                    best = (dx, dy, score);
                }
            }
        }
        self.region = region;
        let (bx, by, bs) = best;
        // parabolic sub-pixel refinement
        let sub = |a: f32, b: f32, c: f32| {
            let d = a - 2.0 * b + c;
            if d.abs() < 1e-6 {
                0.0
            } else {
                (0.5 * (a - c) / d).clamp(-0.5, 0.5)
            }
        };
        let at = |x: usize, y: usize| scores[y * side + x];
        let ox = if bx > 0 && bx + 1 < side { sub(at(bx - 1, by), bs, at(bx + 1, by)) } else { 0.0 };
        let oy = if by > 0 && by + 1 < side { sub(at(bx, by - 1), bs, at(bx, by + 1)) } else { 0.0 };
        ((pred.0 + bx as f32 - r as f32 + ox, pred.1 + by as f32 - r as f32 + oy), bs)
    }
}

/// Decode `count` frames starting at frame `first` as grayscale `w`×`h` and hand each to `f`
/// (stop early by returning false). Frame-accurate (seeks to the frame's centre time).
pub fn decode_gray(ffmpeg: &Path, video: &Path, fps: f64, first: i64, count: usize, w: usize, h: usize, mut f: impl FnMut(i64, Gray) -> bool) -> Result<(), String> {
    let fm = 1000.0 / fps;
    let ss = ((first as f64 - 0.5) * fm).max(0.0) / 1000.0;
    let mut c = crate::extract::command(ffmpeg);
    c.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss", &format!("{ss:.4}")]);
    c.arg("-i").arg(video);
    c.args(["-map", "0:v:0", "-an", "-sn", "-dn", "-frames:v", &count.to_string()]);
    c.args(["-vf", &format!("scale={w}:{h}:flags=bicubic"), "-f", "rawvideo", "-pix_fmt", "gray", "pipe:1"]);
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    let mut out = child.stdout.take().unwrap();
    let mut k = first;
    loop {
        let mut data = vec![0u8; w * h];
        if out.read_exact(&mut data).is_err() {
            break;
        }
        if !f(k, Gray { w, h, data }) {
            let _ = child.kill();
            break;
        }
        k += 1;
    }
    let _ = child.wait();
    Ok(())
}

/// Result of tracking: positions per frame in *script* coordinates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub fps: f64,
    /// Frame number of `a[0]`
    pub first: i64,
    /// Frame where the points were placed (the reference)
    pub reference: i64,
    pub a: Vec<Option<Sample>>,
    pub b: Option<Vec<Option<Sample>>>,
}

impl Track {
    pub fn last(&self) -> i64 {
        self.first + self.a.len() as i64 - 1
    }
    fn at(v: &[Option<Sample>], first: i64, k: i64) -> Option<Sample> {
        if v.is_empty() {
            return None;
        }
        let i = (k - first).clamp(0, v.len() as i64 - 1) as usize;
        // nearest tracked sample (hold through gaps)
        (0..v.len()).map(|d| [i.checked_sub(d), Some(i + d)]).flat_map(|x| x.into_iter().flatten()).filter(|&j| j < v.len()).find_map(|j| v[j])
    }
    pub fn a_at(&self, k: i64) -> Option<Sample> {
        Self::at(&self.a, self.first, k)
    }
    pub fn b_at(&self, k: i64) -> Option<Sample> {
        self.b.as_ref().and_then(|b| Self::at(b, self.first, k))
    }
    /// Frames whose match score is below `min`.
    pub fn weak_frames(&self, min: f32) -> usize {
        let w = |v: &Vec<Option<Sample>>| v.iter().filter(|s| s.map(|s| s.score < min).unwrap_or(true)).count();
        w(&self.a) + self.b.as_ref().map(w).unwrap_or(0)
    }
}

/// Track one or two points over frames `first..=last`, starting from `reference` where the
/// points are given (frame pixel coordinates). Calls `progress(done, total)`; return false to cancel.
#[allow(clippy::too_many_arguments)]
pub fn track_video(
    ffmpeg: &Path,
    video: &Path,
    fps: f64,
    size: (usize, usize),
    first: i64,
    last: i64,
    reference: i64,
    points: &[(f32, f32)],
    params: TrackParams,
    mut progress: impl FnMut(usize, usize) -> bool,
) -> Result<Vec<Vec<Option<Sample>>>, String> {
    if points.is_empty() || points.len() > 2 {
        return Err("place one or two points".into());
    }
    if !(first <= reference && reference <= last) {
        return Err("the reference frame must be inside the tracked range".into());
    }
    let total = (last - first + 1) as usize;
    let (w, h) = size;
    let mut out: Vec<Vec<Option<Sample>>> = vec![vec![None; total]; points.len()];
    let mut done = 0usize;
    let mut cancelled = false;
    // backward part: keep frames first..=reference in memory (usually short), then walk back
    let mut back: Vec<Gray> = Vec::new();
    decode_gray(ffmpeg, video, fps, first, (reference - first + 1) as usize, w, h, |_, g| {
        back.push(g);
        true
    })?;
    if back.is_empty() {
        return Err("could not decode the reference frame".into());
    }
    let ref_frame = back.last().unwrap().clone();
    for (pi, p) in points.iter().enumerate() {
        out[pi][(reference - first) as usize] = Some(Sample { x: p.0, y: p.1, score: 1.0 });
    }
    done += 1;
    if back.len() > 1 {
        let mut trackers: Vec<PointTracker> = points.iter().map(|p| PointTracker::new(&ref_frame, *p, params)).collect();
        for (i, g) in back.iter().enumerate().rev().skip(1) {
            for (pi, t) in trackers.iter_mut().enumerate() {
                out[pi][i] = Some(t.step(g));
            }
            done += 1;
            if !progress(done, total) {
                cancelled = true;
                break;
            }
        }
    }
    drop(back);
    // forward part: stream frames after the reference
    if !cancelled && last > reference {
        let mut trackers: Vec<PointTracker> = points.iter().map(|p| PointTracker::new(&ref_frame, *p, params)).collect();
        decode_gray(ffmpeg, video, fps, reference + 1, (last - reference) as usize, w, h, |k, g| {
            let i = (k - first) as usize;
            for (pi, t) in trackers.iter_mut().enumerate() {
                if i < total {
                    out[pi][i] = Some(t.step(&g));
                }
            }
            done += 1;
            if !progress(done, total) {
                cancelled = true;
                return false;
            }
            true
        })?;
    }
    if cancelled {
        return Err("cancelled".into());
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyOpts {
    pub position: bool,
    /// needs two points
    pub scale: bool,
    /// needs two points
    pub rotation: bool,
}

impl Default for ApplyOpts {
    fn default() -> Self {
        ApplyOpts { position: true, scale: true, rotation: true }
    }
}

fn num(v: f32) -> String {
    crate::ass::fmt_num((v * 100.0).round() / 100.0)
}

/// Attach line `idx` to a track: one event per frame with `\pos` (and `\fscx/\fscy/\frz` from the
/// second point). The original line is kept as a comment, like frame-by-frame animations, so it can
/// be removed with [`crate::anim::remove_tween`].
pub fn apply_track(sub: &mut Subtitle, idx: usize, track: &Track, opts: ApplyOpts) -> Result<Applied, String> {
    if idx >= sub.events.len() {
        return Err("no such line".into());
    }
    let idx = crate::anim::remove_tween(sub, idx).unwrap_or(idx);
    let orig = sub.events[idx].clone();
    let fm = 1000.0 / track.fps;
    let k0 = (orig.start as f64 / fm - 1e-6).ceil() as i64;
    let k1 = (orig.end as f64 / fm - 1e-6).ceil() as i64 - 1;
    if k1 < k0 {
        return Err("line is shorter than one frame".into());
    }
    if k1 < track.first || k0 > track.last() {
        return Err(format!("line {} is outside the tracked frames", idx + 1));
    }
    let mut warnings = Vec::new();
    if k0 < track.first || k1 > track.last() {
        warnings.push(format!("line {} extends beyond the tracked range — edge frames hold the last position", idx + 1));
    }
    let two = track.b.is_some();
    let use_scale = opts.scale && two;
    let use_rot = opts.rotation && two;
    let ra = track.a_at(track.reference).ok_or("no reference point")?;
    let rb = track.b_at(track.reference);
    let ref_time = (track.reference as f64 * fm).round() as i64;
    let p0 = tags::position_at(sub, &orig, ref_time.clamp(orig.start, orig.end - 1));
    let st = sub.style(&orig.style).cloned().unwrap_or_default();
    let tf = |n: &str| tags::get_tag(&orig.text, n).and_then(|a| a.trim().parse::<f32>().ok());
    let (sx0, sy0, rz0) = (tf("fscx").unwrap_or(st.scale_x), tf("fscy").unwrap_or(st.scale_y), tf("frz").or_else(|| tf("fr")).unwrap_or(st.angle));
    let fade = tags::parse_fad(&orig.text);
    let mut drop_tags: Vec<&str> = vec!["pos", "move"];
    if use_scale {
        drop_tags.extend(["fscx", "fscy"]);
    }
    if use_rot {
        drop_tags.push("frz");
    }
    if fade.is_some() {
        drop_tags.extend(["fad", "fade"]);
    }
    if tags::get_tag(&orig.text, "t").is_some() {
        warnings.push(format!("line {}: \\t transforms restart on every frame line — consider removing them", idx + 1));
    }
    let base = remove_tags(&orig.text, &drop_tags);
    let dur = orig.duration();
    let id = format!("{:x}", {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in format!("trk{:?}{}{}", std::time::SystemTime::now(), orig.start, orig.text).bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h & 0xffff_ffff
    });
    let mut out: Vec<Event> = Vec::new();
    let mut last_tags = String::new();
    for k in k0..=k1 {
        let a = track.a_at(k).unwrap_or(ra);
        let (mut scale, mut rot) = (1.0f32, 0.0f32);
        if let (Some(rb), Some(b)) = (rb, track.b_at(k)) {
            let (v0x, v0y, v1x, v1y) = (rb.x - ra.x, rb.y - ra.y, b.x - a.x, b.y - a.y);
            let (l0, l1) = ((v0x * v0x + v0y * v0y).sqrt(), (v1x * v1x + v1y * v1y).sqrt());
            if l0 > 1e-3 {
                scale = l1 / l0;
            }
            rot = v1y.atan2(v1x) - v0y.atan2(v0x);
        }
        let (s, r) = (if use_scale { scale } else { 1.0 }, if use_rot { rot } else { 0.0 });
        let (dx, dy) = (p0.0 - ra.x, p0.1 - ra.y);
        let (cs, sn) = (r.cos(), r.sin());
        let pos = if opts.position || use_scale || use_rot {
            (a.x + s * (dx * cs - dy * sn), a.y + s * (dx * sn + dy * cs))
        } else {
            p0
        };
        let mut t = format!("\\pos({},{})", num(pos.0), num(pos.1));
        if use_scale {
            t += &format!("\\fscx{}\\fscy{}", num(sx0 * s), num(sy0 * s));
        }
        if use_rot {
            t += &format!("\\frz{}", num(rz0 - r.to_degrees()));
        }
        if let Some((fi, fo)) = fade {
            let local = ((k as f64 * fm) as i64 - orig.start).max(0);
            let mut vis = 1.0f32;
            if fi > 0 && local < fi {
                vis = vis.min(local as f32 / fi as f32);
            }
            if fo > 0 && local > dur - fo {
                vis = vis.min((dur - local) as f32 / fo as f32);
            }
            t += &format!("\\alpha&H{:02X}&", (255.0 * (1.0 - vis.clamp(0.0, 1.0))).round() as u8);
        }
        let start = if k == k0 { orig.start } else { (k as f64 * fm).round() as i64 };
        let end = if k == k1 { orig.end } else { ((k + 1) as f64 * fm).round() as i64 };
        if t == last_tags {
            if let Some(p) = out.last_mut() {
                p.end = end;
                continue;
            }
        }
        last_tags = t.clone();
        let mut e = orig.clone();
        e.start = start;
        e.end = end;
        e.comment = false;
        e.effect = format!("{EFFECT_GEN}{id}");
        e.text = format!("{{{t}}}{base}");
        out.push(e);
    }
    let n = out.len();
    let src = &mut sub.events[idx];
    src.comment = true;
    src.effect = format!("{EFFECT_SRC}{id}");
    for (k, e) in out.into_iter().enumerate() {
        sub.events.insert(idx + 1 + k, e);
    }
    Ok(Applied { index: idx + 1, events: n, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Textured test image: smooth noise so correlation has a clear peak.
    fn texture(w: usize, h: usize, ox: f32, oy: f32, angle: f32, scale: f32) -> Gray {
        let mut data = vec![0u8; w * h];
        let (c, s) = (angle.cos(), angle.sin());
        for y in 0..h {
            for x in 0..w {
                // inverse-transform the pixel into texture space
                let (px, py) = (x as f32 - ox, y as f32 - oy);
                let (u, v) = ((px * c + py * s) / scale, (-px * s + py * c) / scale);
                let f = (u * 0.21).sin() * (v * 0.17).cos() + (u * 0.05 + v * 0.07).sin() * 0.8 + ((u * 0.37).sin() * (v * 0.29).sin()) * 0.5;
                data[y * w + x] = (128.0 + f * 55.0).clamp(0.0, 255.0) as u8;
            }
        }
        Gray { w, h, data }
    }

    #[test]
    fn follows_translation_subpixel() {
        let (w, h) = (320, 240);
        let start = (160.0f32, 120.0f32);
        let f0 = texture(w, h, 0.0, 0.0, 0.0, 1.0);
        let mut t = PointTracker::new(&f0, start, TrackParams::default());
        // move 3.4 px right, 1.7 px down per frame, accelerating a bit
        let (mut ox, mut oy) = (0.0f32, 0.0f32);
        for k in 1..30 {
            ox += 3.4 + k as f32 * 0.05;
            oy += 1.7;
            let s = t.step(&texture(w, h, ox, oy, 0.0, 1.0));
            assert!(s.score > 0.9, "frame {k}: score {}", s.score);
            let (ex, ey) = (start.0 + ox, start.1 + oy);
            assert!((s.x - ex).abs() < 0.35 && (s.y - ey).abs() < 0.35, "frame {k}: got ({}, {}), want ({ex}, {ey})", s.x, s.y);
        }
    }

    #[test]
    fn two_points_give_scale_and_rotation() {
        let mut sub = Subtitle::new();
        sub.events.push(Event::new(0, 1000, "{\\pos(150,100)}Sign"));
        // fake a track: A fixed at (100,100), B rotates 90° about A and doubles its distance
        let fps = 10.0;
        let a: Vec<Option<Sample>> = (0..10).map(|_| Some(Sample { x: 100.0, y: 100.0, score: 1.0 })).collect();
        let b: Vec<Option<Sample>> = (0..10)
            .map(|k| {
                let t = k as f32 / 9.0;
                let ang = t * std::f32::consts::FRAC_PI_2;
                let len = 50.0 * (1.0 + t);
                Some(Sample { x: 100.0 + len * ang.cos(), y: 100.0 + len * ang.sin(), score: 1.0 })
            })
            .collect();
        let tr = Track { fps, first: 0, reference: 0, a, b: Some(b) };
        let r = apply_track(&mut sub, 0, &tr, ApplyOpts::default()).unwrap();
        assert_eq!(r.events, 10);
        let last = sub.events.last().unwrap();
        // the line sat 50 px right of A; after +90° (screen) and ×2 it sits 100 px below A
        assert!(last.text.starts_with("{\\pos(100,200)\\fscx200\\fscy200\\frz-90}"), "{}", last.text);
        assert!(sub.events[0].comment && sub.events[0].effect.starts_with(EFFECT_SRC));
        // removable like animations
        crate::anim::remove_tween(&mut sub, 3).unwrap();
        assert_eq!(sub.events.len(), 1);
        assert_eq!(sub.events[0].text, "{\\pos(150,100)}Sign");
    }
}
