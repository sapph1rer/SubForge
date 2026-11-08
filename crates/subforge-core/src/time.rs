//! Time helpers. All times inside SubForge are integer milliseconds (`i64`).

/// Format as SRT time: `HH:MM:SS,mmm`
pub fn fmt_srt(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

/// Format as WebVTT time: `HH:MM:SS.mmm`
pub fn fmt_vtt(ms: i64) -> String {
    fmt_srt(ms).replace(',', ".")
}

/// Format as ASS time: `H:MM:SS.cc` (rounded to nearest centisecond)
pub fn fmt_ass(ms: i64) -> String {
    let cs = (ms.max(0) + 5) / 10;
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360_000,
        (cs / 6000) % 60,
        (cs / 100) % 60,
        cs % 100
    )
}

/// Human / editor display: `H:MM:SS.mmm`
pub fn fmt_display(ms: i64) -> String {
    let neg = ms < 0;
    let a = ms.abs();
    format!(
        "{}{}:{:02}:{:02}.{:03}",
        if neg { "-" } else { "" },
        a / 3_600_000,
        (a / 60_000) % 60,
        (a / 1000) % 60,
        a % 1000
    )
}

/// Parse a clock-style timestamp. Accepts:
/// `H:MM:SS.cc`, `HH:MM:SS,mmm`, `MM:SS.mmm`, `SS.mmm`, `SS`
/// The fractional part is interpreted by its digit count (1 => tenths, 2 => centis, 3 => millis).
pub fn parse_clock(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (main, frac) = match s.rfind(|c| c == ',' || c == '.') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let mut total: i64 = 0;
    let parts: Vec<&str> = main.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    for p in &parts {
        let p = p.trim();
        let v: i64 = if p.is_empty() { 0 } else { p.parse().ok()? };
        total = total * 60 + v;
    }
    let mut ms = total * 1000;
    if !frac.is_empty() {
        let frac = frac.trim();
        if !frac.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let digits = &frac[..frac.len().min(3)];
        let mut v: i64 = digits.parse().ok()?;
        for _ in digits.len()..3 {
            v *= 10;
        }
        ms += v;
    }
    Some(if neg { -ms } else { ms })
}

/// Parse an offset / duration given by a user. Accepts clock formats (see [`parse_clock`])
/// and unit suffixes: `1500ms`, `1.5s`, `2m`, `-250`, `+1.2s`, `1h`. Bare numbers are milliseconds.
pub fn parse_offset(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.contains(':') {
        return parse_clock(t);
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(r) => (true, r.trim()),
        None => (false, t.strip_prefix('+').unwrap_or(t).trim()),
    };
    let lower = body.to_ascii_lowercase();
    let (num, mult) = if let Some(n) = lower.strip_suffix("ms") {
        (n.to_string(), 1.0)
    } else if let Some(n) = lower.strip_suffix('s') {
        (n.to_string(), 1000.0)
    } else if let Some(n) = lower.strip_suffix('m') {
        (n.to_string(), 60_000.0)
    } else if let Some(n) = lower.strip_suffix('h') {
        (n.to_string(), 3_600_000.0)
    } else {
        (lower.clone(), 1.0)
    };
    let v: f64 = num.trim().replace(',', ".").parse().ok()?;
    let ms = (v * mult).round() as i64;
    Some(if neg { -ms } else { ms })
}

/// Parse a frame rate: `23.976`, `24000/1001`, `ntsc` aliases.
pub fn parse_fps(s: &str) -> Option<f64> {
    let t = s.trim().to_ascii_lowercase();
    let v = match t.as_str() {
        "23.976" | "23.98" | "ntsc-film" => 24000.0 / 1001.0,
        "29.97" | "ntsc" => 30000.0 / 1001.0,
        "59.94" => 60000.0 / 1001.0,
        _ => {
            if let Some((a, b)) = t.split_once('/') {
                let a: f64 = a.trim().parse().ok()?;
                let b: f64 = b.trim().parse().ok()?;
                if b == 0.0 {
                    return None;
                }
                a / b
            } else {
                t.parse().ok()?
            }
        }
    };
    if v > 0.0 && v.is_finite() {
        Some(v)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock() {
        assert_eq!(parse_clock("00:01:02,345"), Some(62_345));
        assert_eq!(parse_clock("0:01:02.34"), Some(62_340));
        assert_eq!(parse_clock("01:02.5"), Some(62_500));
        assert_eq!(parse_clock("-1.5"), Some(-1500));
        assert_eq!(fmt_ass(62_345), "0:01:02.35");
        assert_eq!(fmt_srt(3_723_004), "01:02:03,004");
    }
    #[test]
    fn offset() {
        assert_eq!(parse_offset("1.5s"), Some(1500));
        assert_eq!(parse_offset("-250ms"), Some(-250));
        assert_eq!(parse_offset("+2m"), Some(120_000));
        assert_eq!(parse_offset("-00:00:01,200"), Some(-1200));
        assert_eq!(parse_offset("300"), Some(300));
    }
    #[test]
    fn fps() {
        assert!((parse_fps("24000/1001").unwrap() - 23.976).abs() < 0.001);
        assert_eq!(parse_fps("25"), Some(25.0));
    }
}
