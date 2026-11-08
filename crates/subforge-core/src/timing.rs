//! Timing operations: shift, frame-rate conversion, linear synchronisation, snapping.

use crate::model::Subtitle;

/// Which events an operation touches.
#[derive(Debug, Clone, Default)]
pub enum Scope {
    #[default]
    All,
    /// Only these indices.
    Indices(Vec<usize>),
    /// Events whose start is >= this time.
    From(i64),
}

impl Scope {
    pub fn contains(&self, idx: usize, start: i64) -> bool {
        match self {
            Scope::All => true,
            Scope::Indices(v) => v.contains(&idx),
            Scope::From(t) => start >= *t,
        }
    }
}

/// Which time fields to modify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Both,
    Start,
    End,
}

pub fn shift(sub: &mut Subtitle, offset_ms: i64, scope: &Scope, field: Field) {
    for (i, e) in sub.events.iter_mut().enumerate() {
        if !scope.contains(i, e.start) {
            continue;
        }
        if field != Field::End {
            e.start = (e.start + offset_ms).max(0);
        }
        if field != Field::Start {
            e.end = (e.end + offset_ms).max(0);
        }
        if e.end < e.start {
            e.end = e.start;
        }
    }
}

/// Map every time `t` to `t * factor + offset`.
pub fn linear(sub: &mut Subtitle, factor: f64, offset_ms: f64, scope: &Scope) {
    for (i, e) in sub.events.iter_mut().enumerate() {
        if !scope.contains(i, e.start) {
            continue;
        }
        e.start = ((e.start as f64 * factor + offset_ms).round() as i64).max(0);
        e.end = ((e.end as f64 * factor + offset_ms).round() as i64).max(e.start);
    }
}

/// Convert timing made for `from_fps` video to `to_fps` (e.g. 23.976 -> 25 PAL speed-up).
pub fn change_fps(sub: &mut Subtitle, from_fps: f64, to_fps: f64, scope: &Scope) {
    linear(sub, from_fps / to_fps, 0.0, scope);
}

/// Two-point synchronisation: `old_a` should become `new_a` and `old_b` should become `new_b`.
/// With identical points it degenerates to a shift.
pub fn sync_two_points(sub: &mut Subtitle, old_a: i64, new_a: i64, old_b: i64, new_b: i64, scope: &Scope) -> Result<(f64, f64), String> {
    if old_a == old_b {
        if new_a != new_b {
            return Err("the two reference points must have different original times".into());
        }
        shift(sub, new_a - old_a, scope, Field::Both);
        return Ok((1.0, (new_a - old_a) as f64));
    }
    let factor = (new_b - new_a) as f64 / (old_b - old_a) as f64;
    if !(factor.is_finite() && factor > 0.0) {
        return Err("invalid sync points (would reverse time)".into());
    }
    let offset = new_a as f64 - old_a as f64 * factor;
    linear(sub, factor, offset, scope);
    Ok((factor, offset))
}

/// Multi-point synchronisation using piecewise-linear interpolation between sorted points.
pub fn sync_points(sub: &mut Subtitle, points: &[(i64, i64)], scope: &Scope) -> Result<(), String> {
    match points.len() {
        0 => return Err("no sync points".into()),
        1 => {
            shift(sub, points[0].1 - points[0].0, scope, Field::Both);
            return Ok(());
        }
        2 => {
            sync_two_points(sub, points[0].0, points[0].1, points[1].0, points[1].1, scope)?;
            return Ok(());
        }
        _ => {}
    }
    let mut pts = points.to_vec();
    pts.sort();
    let map = |t: i64| -> i64 {
        let k = pts.iter().position(|p| p.0 > t).unwrap_or(pts.len());
        let (a, b) = if k == 0 {
            (pts[0], pts[1])
        } else if k >= pts.len() {
            (pts[pts.len() - 2], pts[pts.len() - 1])
        } else {
            (pts[k - 1], pts[k])
        };
        if a.0 == b.0 {
            return t + (a.1 - a.0);
        }
        let f = (b.1 - a.1) as f64 / (b.0 - a.0) as f64;
        (a.1 as f64 + (t - a.0) as f64 * f).round() as i64
    };
    for (i, e) in sub.events.iter_mut().enumerate() {
        if !scope.contains(i, e.start) {
            continue;
        }
        e.start = map(e.start).max(0);
        e.end = map(e.end).max(e.start);
    }
    Ok(())
}

/// Round all times to the nearest video frame boundary.
pub fn snap_to_frames(sub: &mut Subtitle, fps: f64, scope: &Scope) {
    let fd = 1000.0 / fps;
    for (i, e) in sub.events.iter_mut().enumerate() {
        if !scope.contains(i, e.start) {
            continue;
        }
        e.start = ((e.start as f64 / fd).round() * fd).round() as i64;
        e.end = ((e.end as f64 / fd).round() * fd).round() as i64;
    }
}

// ------------------------------------------------------------------ keyframes

/// Nearest keyframe (ms) to `t` within `[t - before, t + after]`. `kfs` must be sorted.
pub fn nearest_keyframe(kfs: &[i64], t: i64, before: i64, after: i64) -> Option<i64> {
    let i = kfs.partition_point(|&k| k < t - before);
    kfs[i..].iter().copied().take_while(|&k| k <= t + after).min_by_key(|&k| (k - t).abs())
}

/// Previous / next keyframe strictly before / after `t`.
pub fn prev_keyframe(kfs: &[i64], t: i64) -> Option<i64> {
    let i = kfs.partition_point(|&k| k < t);
    i.checked_sub(1).map(|j| kfs[j])
}
pub fn next_keyframe(kfs: &[i64], t: i64) -> Option<i64> {
    let i = kfs.partition_point(|&k| k <= t);
    kfs.get(i).copied()
}

/// Snap start and end times to keyframes (scene changes) that are within the given windows.
/// Ends snap to the keyframe itself, so the subtitle disappears exactly on the cut.
/// Returns the number of events changed.
pub fn snap_to_keyframes(sub: &mut Subtitle, kfs: &[i64], before_ms: i64, after_ms: i64, scope: &Scope) -> usize {
    let mut n = 0;
    for (i, e) in sub.events.iter_mut().enumerate() {
        if e.comment || !scope.contains(i, e.start) {
            continue;
        }
        let (s0, e0) = (e.start, e.end);
        if let Some(k) = nearest_keyframe(kfs, e.start, before_ms, after_ms) {
            if k < e.end {
                e.start = k;
            }
        }
        if let Some(k) = nearest_keyframe(kfs, e.end, before_ms, after_ms) {
            if k > e.start {
                e.end = k;
            }
        }
        if (e.start, e.end) != (s0, e0) {
            n += 1;
        }
    }
    n
}

/// Parse a keyframe file.
///
/// Supported: Aegisub `# keyframe format v1` (frame numbers, optional `fps` line), and plain
/// lists where each line is a frame number (needs `fps`), seconds (`12.345`), milliseconds
/// (`12345ms`) or a clock time (`0:00:12.345`). Returns sorted, de-duplicated milliseconds.
pub fn parse_keyframes(content: &str, fps: Option<f64>) -> Result<Vec<i64>, String> {
    let mut fps = fps;
    let mut out = Vec::new();
    for line in content.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some(f) = l.strip_prefix("fps") {
            if let Some(v) = crate::time::parse_fps(f.trim()).filter(|v| *v > 0.0) {
                if fps.is_none() {
                    fps = Some(v);
                }
            }
            continue;
        }
        let ms = if l.contains(':') {
            crate::time::parse_clock(l).ok_or_else(|| format!("bad time: {l}"))?
        } else if let Some(m) = l.strip_suffix("ms") {
            m.trim().parse::<i64>().map_err(|_| format!("bad ms: {l}"))?
        } else if l.contains('.') {
            (l.parse::<f64>().map_err(|_| format!("bad seconds: {l}"))? * 1000.0).round() as i64
        } else {
            let f: i64 = l.parse().map_err(|_| format!("bad frame number: {l}"))?;
            let fps = fps.ok_or("frame-number keyframes need a frame rate (--fps)")?;
            (f as f64 * 1000.0 / fps).round() as i64
        };
        out.push(ms);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Write keyframes in Aegisub v1 format (frame numbers).
pub fn keyframes_to_aegisub(kfs: &[i64], fps: f64) -> String {
    let mut s = String::from("# keyframe format v1\nfps 0\n");
    for &k in kfs {
        s.push_str(&format!("{}\n", (k as f64 * fps / 1000.0).round() as i64));
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn keyframes() {
        let kfs = parse_keyframes("# keyframe format v1\nfps 0\n0\n24\n240\n", Some(24.0)).unwrap();
        assert_eq!(kfs, vec![0, 1000, 10000]);
        assert_eq!(parse_keyframes("1.5\n0:00:03.000\n250ms", None).unwrap(), vec![250, 1500, 3000]);
        let mut s = Subtitle::new();
        s.events.push(Event::new(1100, 9800, "a"));
        let n = snap_to_keyframes(&mut s, &kfs, 250, 250, &Scope::All);
        assert_eq!(n, 1);
        assert_eq!((s.events[0].start, s.events[0].end), (1000, 10000));
        assert_eq!(prev_keyframe(&kfs, 1000), Some(0));
        assert_eq!(next_keyframe(&kfs, 1000), Some(10000));
        assert_eq!(keyframes_to_aegisub(&kfs, 24.0), "# keyframe format v1\nfps 0\n0\n24\n240\n");
    }
    use super::*;
    use crate::model::Event;
    #[test]
    fn sync() {
        let mut s = Subtitle::new();
        s.events.push(Event::new(1000, 2000, "a"));
        s.events.push(Event::new(10000, 11000, "b"));
        sync_two_points(&mut s, 1000, 2000, 10000, 20000, &Scope::All).unwrap();
        assert_eq!(s.events[0].start, 2000);
        assert_eq!(s.events[1].start, 20000);
        shift(&mut s, -500, &Scope::Indices(vec![0]), Field::Both);
        assert_eq!(s.events[0].start, 1500);
    }
}
