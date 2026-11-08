//! Batch editing tools: durations, gap bridging, hearing-impaired text removal, casing,
//! merging duplicates and script resolution resampling.

use crate::model::Subtitle;
use crate::tags::{tag_spans, TagSpan};
use crate::text::{count_chars, tokenize, CountOpts, Token};
use crate::timing::Scope;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DurationMode {
    /// Add (or subtract) milliseconds
    AddMs(i64),
    /// Scale duration by a percentage
    Percent(f64),
    /// Set a fixed duration
    Fixed(i64),
    /// Duration needed to reach this characters-per-second (only extends)
    TargetCps(f64),
}

/// Adjust durations; ends never pass the next line's start minus `min_gap` when `keep_gap`.
pub fn adjust_durations(sub: &mut Subtitle, mode: DurationMode, scope: &Scope, keep_gap: Option<i64>, min_ms: i64, max_ms: i64) -> usize {
    let mut order: Vec<usize> = (0..sub.events.len()).collect();
    order.sort_by_key(|&i| sub.events[i].start);
    let mut changed = 0;
    for (k, &i) in order.iter().enumerate() {
        let e = &sub.events[i];
        if e.comment || !scope.contains(i, e.start) {
            continue;
        }
        let dur = e.duration();
        let mut nd = match mode {
            DurationMode::AddMs(ms) => dur + ms,
            DurationMode::Percent(p) => (dur as f64 * p / 100.0).round() as i64,
            DurationMode::Fixed(ms) => ms,
            DurationMode::TargetCps(cps) => {
                let need = (count_chars(&e.plain(), CountOpts::default()) as f64 / cps.max(0.1) * 1000.0).ceil() as i64;
                dur.max(need)
            }
        };
        if min_ms > 0 {
            nd = nd.max(min_ms);
        }
        if max_ms > 0 {
            nd = nd.min(max_ms);
        }
        nd = nd.max(1);
        let mut end = e.start + nd;
        if let Some(gap) = keep_gap {
            if let Some(&j) = order[k + 1..].iter().find(|&&j| !sub.events[j].comment && sub.events[j].start >= e.start) {
                let limit = sub.events[j].start - gap;
                if end > limit && e.end <= limit {
                    end = limit.max(e.start + 1);
                } else if end > limit {
                    end = e.end.min(end);
                }
            }
        }
        if end != e.end {
            sub.events[i].end = end;
            changed += 1;
        }
    }
    changed
}

/// Close small gaps: if the gap to the next line is below `max_gap`, extend the end so exactly
/// `min_gap` remains (Netflix-style "chaining").
pub fn bridge_gaps(sub: &mut Subtitle, max_gap: i64, min_gap: i64, scope: &Scope) -> usize {
    let mut order: Vec<usize> = (0..sub.events.len()).filter(|&i| !sub.events[i].comment).collect();
    order.sort_by_key(|&i| sub.events[i].start);
    let mut n = 0;
    for w in order.windows(2) {
        let (i, j) = (w[0], w[1]);
        if !scope.contains(i, sub.events[i].start) {
            continue;
        }
        let gap = sub.events[j].start - sub.events[i].end;
        if gap > min_gap && gap < max_gap {
            sub.events[i].end = sub.events[j].start - min_gap;
            n += 1;
        }
    }
    n
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiOpts {
    /// [sound], (sound), {sound} in text
    pub brackets: bool,
    pub parens: bool,
    /// "JOHN:" / "Narrator:" speaker labels at line start
    pub speakers: bool,
    /// lines made only of ♪ / music symbols
    pub music: bool,
    /// only remove speaker labels written in UPPER CASE
    pub speakers_uppercase_only: bool,
}

impl Default for HiOpts {
    fn default() -> Self {
        HiOpts { brackets: true, parens: true, speakers: true, music: true, speakers_uppercase_only: true }
    }
}

/// Trim whitespace at the visible start/end of a line, looking through leading/trailing tag blocks.
fn trim_visible(s: &str) -> String {
    let toks = tokenize(s);
    let mut parts: Vec<String> = toks
        .iter()
        .map(|t| match t {
            Token::Block(b) => format!("{{{b}}}"),
            Token::Text(t) => t.to_string(),
        })
        .collect();
    if let Some(i) = toks.iter().position(|t| matches!(t, Token::Text(_))) {
        parts[i] = parts[i].trim_start().to_string();
    }
    if let Some(i) = toks.iter().rposition(|t| matches!(t, Token::Text(_))) {
        parts[i] = parts[i].trim_end().to_string();
    }
    parts.concat()
}

fn strip_between(s: &str, open: char, close: char) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        if c == open {
            depth += 1;
        } else if c == close && depth > 0 {
            depth -= 1;
        } else if depth == 0 {
            out.push(c);
        }
    }
    out
}

/// Remove text for the hearing impaired. Works line by line on the `\N`-separated parts and keeps
/// override tags. Returns (changed, removed-empty) counts.
pub fn remove_hi(sub: &mut Subtitle, opts: HiOpts, scope: &Scope) -> (usize, usize) {
    let mut changed = 0;
    for (i, e) in sub.events.iter_mut().enumerate() {
        if e.comment || !scope.contains(i, e.start) {
            continue;
        }
        // operate on text runs only (keep tags)
        let mut parts: Vec<String> = Vec::new();
        for line in e.text.split("\\N") {
            let mut out = String::new();
            for tok in tokenize(line) {
                match tok {
                    Token::Block(b) => {
                        out.push('{');
                        out.push_str(b);
                        out.push('}');
                    }
                    Token::Text(t) => {
                        let mut s = t.to_string();
                        if opts.brackets {
                            s = strip_between(&s, '[', ']');
                        }
                        if opts.parens {
                            s = strip_between(&s, '(', ')');
                        }
                        out.push_str(&s);
                    }
                }
            }
            let mut plain_line = crate::text::plain_text(&out);
            if opts.speakers {
                if let Some((label, _)) = plain_line.split_once(':') {
                    let l = label.trim().trim_start_matches('-').trim();
                    let looks = !l.is_empty()
                        && l.chars().count() <= 25
                        && !l.chars().any(|c| c.is_ascii_digit())
                        && (!opts.speakers_uppercase_only || l.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase()));
                    if looks {
                        // remove the label from the text run that contains it
                        if let Some(p) = out.find(&format!("{}:", label)) {
                            let after = &out[p + label.len() + 1..];
                            out = format!("{}{}", &out[..p], after.trim_start());
                            plain_line = crate::text::plain_text(&out);
                        }
                    }
                }
            }
            if opts.music && !plain_line.trim().is_empty() && plain_line.chars().all(|c| matches!(c, '♪' | '♫' | '#' | '*' | ' ' | '~' | '…' | '.')) {
                continue;
            }
            let trimmed_plain = crate::text::plain_text(&out);
            if trimmed_plain.trim().is_empty() && !out.contains("\\p") {
                // drop the empty line but keep its leading tags on the next part
                let tags: String = tokenize(&out).into_iter().filter_map(|t| if let Token::Block(b) = t { Some(format!("{{{b}}}")) } else { None }).collect();
                if !tags.is_empty() {
                    parts.push(tags);
                }
                continue;
            }
            parts.push(trim_visible(&out));
        }
        // merge tag-only fragments into the following part
        let mut merged: Vec<String> = Vec::new();
        let mut carry = String::new();
        for p in parts {
            if crate::text::plain_text(&p).trim().is_empty() {
                carry.push_str(&p);
            } else {
                merged.push(format!("{carry}{p}"));
                carry.clear();
            }
        }
        let new = if merged.is_empty() { carry } else { merged.join("\\N") };
        let new = new.replace("  ", " ");
        if new != e.text {
            e.text = new;
            changed += 1;
        }
    }
    let before = sub.events.len();
    sub.events.retain(|e| e.comment || !e.plain().trim().is_empty() || e.text.contains("\\p"));
    (changed, before - sub.events.len())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Case {
    Upper,
    Lower,
    /// First letter of each sentence upper, rest lower
    Sentence,
    /// First letter of each word upper
    Title,
}

/// Change letter case of text runs (tags and drawings untouched).
pub fn change_case(sub: &mut Subtitle, case: Case, scope: &Scope) -> usize {
    let mut n = 0;
    for (i, e) in sub.events.iter_mut().enumerate() {
        if e.comment || !scope.contains(i, e.start) {
            continue;
        }
        let mut out = String::with_capacity(e.text.len());
        let mut sentence_start = true;
        let mut word_start = true;
        let mut drawing = false;
        for tok in tokenize(&e.text) {
            match tok {
                Token::Block(b) => {
                    for t in tag_spans(b) {
                        if t.name == "p" {
                            drawing = t.arg.trim().parse::<i32>().map(|v| v > 0).unwrap_or(false);
                        }
                    }
                    out.push('{');
                    out.push_str(b);
                    out.push('}');
                }
                Token::Text(t) if drawing => out.push_str(t),
                Token::Text(t) => {
                    let mut chars = t.chars().peekable();
                    while let Some(c) = chars.next() {
                        // keep \N \n \h escapes intact
                        if c == '\\' && matches!(chars.peek(), Some('N') | Some('n') | Some('h')) {
                            out.push(c);
                            out.push(chars.next().unwrap());
                            word_start = true;
                            continue;
                        }
                        let mapped: String = match case {
                            Case::Upper => c.to_uppercase().collect(),
                            Case::Lower => c.to_lowercase().collect(),
                            Case::Sentence => {
                                if c.is_alphabetic() {
                                    let r = if sentence_start { c.to_uppercase().collect() } else { c.to_lowercase().collect() };
                                    sentence_start = false;
                                    r
                                } else {
                                    c.to_string()
                                }
                            }
                            Case::Title => {
                                if c.is_alphabetic() {
                                    let r = if word_start { c.to_uppercase().collect() } else { c.to_lowercase().collect() };
                                    word_start = false;
                                    r
                                } else {
                                    c.to_string()
                                }
                            }
                        };
                        if matches!(c, '.' | '!' | '?' | '。' | '！' | '？') {
                            sentence_start = true;
                        }
                        if !c.is_alphanumeric() && c != '\'' {
                            word_start = true;
                        }
                        out.push_str(&mapped);
                    }
                }
            }
        }
        if out != e.text {
            e.text = out;
            n += 1;
        }
    }
    n
}

/// Merge consecutive lines that have identical text and touch/overlap (gap ≤ `max_gap`).
pub fn merge_same_text(sub: &mut Subtitle, max_gap: i64) -> usize {
    sub.sort_by_time();
    let mut n = 0;
    let mut i = 0;
    while i + 1 < sub.events.len() {
        let (a, b) = (&sub.events[i], &sub.events[i + 1]);
        if !a.comment && !b.comment && a.text == b.text && a.style == b.style && b.start - a.end <= max_gap {
            let end = a.end.max(b.end);
            sub.events[i].end = end;
            sub.events.remove(i + 1);
            n += 1;
        } else {
            i += 1;
        }
    }
    n
}

fn scale_args(arg: &str, sx: f64, sy: f64, xy_pairs: bool) -> String {
    let inner = arg.trim().trim_start_matches('(').trim_end_matches(')');
    let vals: Vec<&str> = inner.split(',').collect();
    let scaled: Vec<String> = vals
        .iter()
        .enumerate()
        .map(|(k, v)| match v.trim().parse::<f64>() {
            Ok(n) if xy_pairs => {
                let f = if k % 2 == 0 { sx } else { sy };
                crate::ass::fmt_num((n * f * 100.0).round() as f32 / 100.0)
            }
            _ => v.trim().to_string(),
        })
        .collect();
    format!("({})", scaled.join(","))
}

fn scale_drawing(s: &str, sx: f64, sy: f64) -> String {
    let mut out = Vec::new();
    let mut idx = 0;
    for tok in s.split_whitespace() {
        match tok.parse::<f64>() {
            Ok(v) => {
                let f = if idx % 2 == 0 { sx } else { sy };
                idx += 1;
                out.push(crate::ass::fmt_num((v * f * 100.0).round() as f32 / 100.0));
            }
            Err(_) => {
                idx = 0;
                out.push(tok.to_string());
            }
        }
    }
    out.join(" ")
}

/// Rewrite one override block for a resolution change.
fn resample_block(block: &str, sx: f64, sy: f64) -> String {
    let mut out = String::new();
    let mut cur = 0;
    let spans: Vec<TagSpan> = tag_spans(block);
    let size = sy; // sizes follow the vertical ratio, like Aegisub
    for t in spans {
        out.push_str(&block[cur..t.start]);
        let new = match t.name.as_str() {
            "pos" | "org" => format!("\\{}{}", t.name, scale_args(&t.arg, sx, sy, true)),
            "move" => {
                let inner = t.arg.trim().trim_start_matches('(').trim_end_matches(')');
                let v: Vec<&str> = inner.split(',').collect();
                if v.len() >= 4 {
                    let head = scale_args(&v[..4].join(","), sx, sy, true);
                    let tail = if v.len() > 4 { format!(",{}", v[4..].join(",")) } else { String::new() };
                    format!("\\move{}{})", head.trim_end_matches(')'), tail)
                } else {
                    block[t.start..t.end].to_string()
                }
            }
            "clip" | "iclip" => {
                let inner = t.arg.trim().trim_start_matches('(').trim_end_matches(')');
                let v: Vec<&str> = inner.split(',').collect();
                if v.len() == 4 && v.iter().all(|x| x.trim().parse::<f64>().is_ok()) {
                    format!("\\{}{}", t.name, scale_args(&t.arg, sx, sy, true))
                } else {
                    // vector clip: optional scale then drawing
                    let (pre, draw) = match inner.split_once(',') {
                        Some((a, b)) if a.trim().parse::<i32>().is_ok() => (format!("{},", a.trim()), b),
                        _ => (String::new(), inner),
                    };
                    format!("\\{}({}{})", t.name, pre, scale_drawing(draw, sx, sy))
                }
            }
            "fs" | "bord" | "shad" | "be" | "blur" | "ybord" | "yshad" => match t.arg.trim().parse::<f64>() {
                Ok(v) => format!("\\{}{}", t.name, crate::ass::fmt_num((v * size * 100.0).round() as f32 / 100.0)),
                Err(_) => block[t.start..t.end].to_string(),
            },
            "xbord" | "xshad" | "fsp" => match t.arg.trim().parse::<f64>() {
                Ok(v) => format!("\\{}{}", t.name, crate::ass::fmt_num((v * sx * 100.0).round() as f32 / 100.0)),
                Err(_) => block[t.start..t.end].to_string(),
            },
            "t" => {
                // recurse into the transform's tag list
                let inner = t.arg.trim();
                let inner = inner.strip_prefix('(').and_then(|x| x.strip_suffix(')')).unwrap_or(inner);
                match inner.find('\\') {
                    Some(p) => format!("\\t({}{})", &inner[..p], resample_block(&inner[p..], sx, sy)),
                    None => block[t.start..t.end].to_string(),
                }
            }
            _ => block[t.start..t.end].to_string(),
        };
        out.push_str(&new);
        cur = t.end;
    }
    out.push_str(&block[cur..]);
    out
}

/// Change PlayResX/Y and rescale styles, margins, positions, clips, sizes and drawings so the
/// script looks the same at the new resolution.
pub fn resample(sub: &mut Subtitle, to: (i32, i32)) {
    let from = sub.play_res();
    let sx = to.0 as f64 / from.0.max(1) as f64;
    let sy = to.1 as f64 / from.1.max(1) as f64;
    for s in sub.styles.iter_mut() {
        s.fontsize = (s.fontsize as f64 * sy) as f32;
        s.outline_w = (s.outline_w as f64 * sy) as f32;
        s.shadow = (s.shadow as f64 * sy) as f32;
        s.spacing = (s.spacing as f64 * sx) as f32;
        s.margin_l = (s.margin_l as f64 * sx).round() as i32;
        s.margin_r = (s.margin_r as f64 * sx).round() as i32;
        s.margin_v = (s.margin_v as f64 * sy).round() as i32;
    }
    for e in sub.events.iter_mut() {
        e.margin_l = (e.margin_l as f64 * sx).round() as i32;
        e.margin_r = (e.margin_r as f64 * sx).round() as i32;
        e.margin_v = (e.margin_v as f64 * sy).round() as i32;
        let mut out = String::with_capacity(e.text.len());
        let mut drawing = false;
        for tok in tokenize(&e.text) {
            match tok {
                Token::Block(b) => {
                    for t in tag_spans(b) {
                        if t.name == "p" {
                            drawing = t.arg.trim().parse::<i32>().map(|v| v > 0).unwrap_or(false);
                        }
                    }
                    out.push('{');
                    out.push_str(&resample_block(b, sx, sy));
                    out.push('}');
                }
                Token::Text(t) if drawing => out.push_str(&scale_drawing(t, sx, sy)),
                Token::Text(t) => out.push_str(t),
            }
        }
        e.text = out;
    }
    sub.info_set("PlayResX", to.0.to_string());
    sub.info_set("PlayResY", to.1.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Event;
    #[test]
    fn hi() {
        let mut s = Subtitle::new();
        s.events.push(Event::new(0, 1, "[DOOR SLAMS]\\NJOHN: Where are you?"));
        s.events.push(Event::new(2, 3, "♪ ♪"));
        s.events.push(Event::new(4, 5, "{\\i1}(sighs){\\i0} Fine."));
        let (c, r) = remove_hi(&mut s, HiOpts::default(), &Scope::All);
        assert_eq!(r, 1);
        assert!(c >= 2);
        assert_eq!(s.events[0].text, "Where are you?");
        assert_eq!(s.events[1].plain(), "Fine.");
    }
    #[test]
    fn case() {
        let mut s = Subtitle::new();
        s.events.push(Event::new(0, 1, "{\\i1}HELLO THERE.{\\i0} HOW ARE YOU?\\NFINE"));
        change_case(&mut s, Case::Sentence, &Scope::All);
        assert_eq!(s.events[0].text, "{\\i1}Hello there.{\\i0} How are you?\\NFine");
    }
    #[test]
    fn durations_and_gaps() {
        let mut s = Subtitle::new();
        s.events.push(Event::new(0, 1000, "a"));
        s.events.push(Event::new(1200, 2000, "b"));
        assert_eq!(bridge_gaps(&mut s, 500, 83, &Scope::All), 1);
        assert_eq!(s.events[0].end, 1117);
        adjust_durations(&mut s, DurationMode::AddMs(5000), &Scope::All, Some(83), 0, 0);
        assert_eq!(s.events[0].end, 1117); // blocked by next line
        assert_eq!(s.events[1].end, 7000);
    }
    #[test]
    fn resample_script() {
        let mut s = Subtitle::new();
        s.info_set("PlayResX", "640");
        s.info_set("PlayResY", "360");
        s.styles[0].fontsize = 20.0;
        s.events.push(Event::new(0, 1, "{\\pos(320,180)\\fs30\\t(0,100,\\fs40)\\clip(m 0 0 l 10 10)\\p1}m 0 0 l 100 50{\\p0}"));
        resample(&mut s, (1920, 1080));
        assert_eq!(s.styles[0].fontsize, 60.0);
        assert_eq!(s.events[0].text, "{\\pos(960,540)\\fs90\\t(0,100,\\fs120)\\clip(m 0 0 l 30 30)\\p1}m 0 0 l 300 150{\\p0}");
        assert_eq!(s.play_res(), (1920, 1080));
    }
}
