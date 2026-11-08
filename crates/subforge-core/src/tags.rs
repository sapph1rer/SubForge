//! Editing ASS override tags inside event text, and computing where a line is drawn.
//!
//! All helpers operate on *top-level* tags only: tags nested inside `\t(...)` are left alone,
//! drawing commands and comments inside blocks are preserved byte-for-byte.

use crate::model::{Event, Subtitle};
use crate::text::{legacy_to_numpad, tokenize, Token};

/// Tag names, longest first so `\fscx` is not read as `\fs` + "cx".
const NAMES: &[&str] = &[
    "iclip", "alpha", "xbord", "ybord", "xshad", "yshad", "fscx", "fscy", "blur", "bord", "clip", "fade", "move", "shad", "fad", "fax", "fay", "frx",
    "fry", "frz", "fsp", "org", "pbo", "pos", "an", "be", "fe", "fn", "fr", "fs", "kf", "ko", "1c", "2c", "3c", "4c", "1a", "2a", "3a", "4a", "a", "b", "c",
    "i", "k", "K", "p", "q", "r", "s", "t", "u",
];

/// One top-level tag inside a block: byte range (relative to the block content, including the
/// leading backslash), canonical name and argument text.
#[derive(Debug, Clone, PartialEq)]
pub struct TagSpan {
    pub start: usize,
    pub end: usize,
    pub name: String,
    pub arg: String,
}

/// Canonical name (aliases unified: `\c` == `\1c`, `\fr` == `\frz`).
pub fn canonical(name: &str) -> &str {
    match name {
        "c" => "1c",
        "fr" => "frz",
        n => n,
    }
}

pub fn tag_spans(block: &str) -> Vec<TagSpan> {
    let bytes = block.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        let mut depth = 0i32;
        while j < bytes.len() {
            match bytes[j] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth <= 0 {
                        j += 1;
                        // a tag ends after its closing parenthesis
                        break;
                    }
                }
                b'\\' if depth <= 0 => break,
                _ => {}
            }
            j += 1;
        }
        let raw = &block[start + 1..j];
        let t = raw.trim_start();
        let name = NAMES
            .iter()
            .find(|n| t.starts_with(**n))
            .map(|n| n.to_string())
            .unwrap_or_else(|| t.chars().take_while(|c| c.is_alphanumeric()).collect());
        let arg = t[name.len().min(t.len())..].trim().to_string();
        if !name.is_empty() {
            out.push(TagSpan { start, end: j, name, arg });
        }
        i = j.max(start + 1);
    }
    out
}

/// Byte range (including braces) of every override block.
fn block_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut off = 0;
    for tok in tokenize(text) {
        match tok {
            Token::Block(b) => {
                v.push((off, off + b.len() + 2));
                off += b.len() + 2;
            }
            Token::Text(t) => off += t.len(),
        }
    }
    v
}

/// First argument of the first top-level occurrence of `name` (canonical names compared).
pub fn get_tag(text: &str, name: &str) -> Option<String> {
    let want = canonical(name);
    for (a, b) in block_ranges(text) {
        for t in tag_spans(&text[a + 1..b - 1]) {
            if canonical(&t.name) == want {
                return Some(t.arg);
            }
        }
    }
    None
}

/// Remove all top-level occurrences of the given tags from every block; empty blocks are dropped.
pub fn remove_tags(text: &str, names: &[&str]) -> String {
    let want: Vec<&str> = names.iter().map(|n| canonical(n)).collect();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (a, b) in block_ranges(text) {
        out.push_str(&text[last..a]);
        let inner = &text[a + 1..b - 1];
        let mut kept = String::new();
        let mut cur = 0;
        for t in tag_spans(inner) {
            if want.contains(&canonical(&t.name)) {
                kept.push_str(&inner[cur..t.start]);
                cur = t.end;
            }
        }
        kept.push_str(&inner[cur..]);
        if !kept.is_empty() || inner.is_empty() {
            out.push('{');
            out.push_str(&kept);
            out.push('}');
        }
        last = b;
    }
    out.push_str(&text[last..]);
    out
}

/// Put `\name<arg>` at the end of the leading block (creating one), replacing existing occurrences.
pub fn set_tag(text: &str, name: &str, arg: &str) -> String {
    let cleaned = remove_tags(text, &[name]);
    insert_leading(&cleaned, &format!("\\{name}{arg}"))
}

/// Append raw tag text (e.g. `\pos(1,2)\fad(100,0)`) to the leading override block.
pub fn insert_leading(text: &str, tags: &str) -> String {
    if tags.is_empty() {
        return text.to_string();
    }
    if text.starts_with('{') {
        if let Some(end) = text.find('}') {
            return format!("{}{}{}", &text[..end], tags, &text[end..]);
        }
    }
    format!("{{{tags}}}{text}")
}

fn nums(arg: &str) -> Vec<f32> {
    arg.trim().trim_start_matches('(').trim_end_matches(')').split(',').filter_map(|v| v.trim().parse::<f32>().ok()).collect()
}

pub fn parse_pos(text: &str) -> Option<(f32, f32)> {
    let v = nums(&get_tag(text, "pos")?);
    (v.len() >= 2).then(|| (v[0], v[1]))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Move {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    /// relative to line start (ms); None = whole line
    pub t1: Option<i64>,
    pub t2: Option<i64>,
}

pub fn parse_move(text: &str) -> Option<Move> {
    let v = nums(&get_tag(text, "move")?);
    if v.len() < 4 {
        return None;
    }
    let (t1, t2) = if v.len() >= 6 { (Some(v[4] as i64), Some(v[5] as i64)) } else { (None, None) };
    Some(Move { x1: v[0], y1: v[1], x2: v[2], y2: v[3], t1, t2 })
}

/// `\fad(in,out)` in ms.
pub fn parse_fad(text: &str) -> Option<(i64, i64)> {
    let v = nums(&get_tag(text, "fad")?);
    (v.len() >= 2).then(|| (v[0] as i64, v[1] as i64))
}

/// Effective numpad alignment of an event (override tag or style).
pub fn alignment(sub: &Subtitle, e: &Event) -> i32 {
    if let Some(a) = get_tag(&e.text, "an").and_then(|a| a.trim().parse::<i32>().ok()).filter(|a| (1..=9).contains(a)) {
        return a;
    }
    if let Some(a) = get_tag(&e.text, "a").and_then(|a| a.trim().parse::<i32>().ok()) {
        return legacy_to_numpad(a);
    }
    sub.style(&e.style).map(|s| s.alignment).unwrap_or(2)
}

/// Anchor point a line would use without `\pos` (from alignment and margins), in script coordinates.
pub fn default_anchor(sub: &Subtitle, e: &Event) -> (f32, f32) {
    let (w, h) = sub.play_res();
    let st = sub.style(&e.style).cloned().unwrap_or_default();
    let ml = if e.margin_l != 0 { e.margin_l } else { st.margin_l } as f32;
    let mr = if e.margin_r != 0 { e.margin_r } else { st.margin_r } as f32;
    let mv = if e.margin_v != 0 { e.margin_v } else { st.margin_v } as f32;
    let a = alignment(sub, e);
    let x = match a {
        1 | 4 | 7 => ml,
        3 | 6 | 9 => w as f32 - mr,
        _ => (ml + (w as f32 - mr)) / 2.0,
    };
    let y = match a {
        7..=9 => mv,
        4..=6 => h as f32 / 2.0,
        _ => h as f32 - mv,
    };
    (x, y)
}

/// Where the line's anchor is at absolute time `t` (handles `\pos`, `\move`, default placement).
pub fn position_at(sub: &Subtitle, e: &Event, t: i64) -> (f32, f32) {
    if let Some(m) = parse_move(&e.text) {
        let dur = e.duration().max(1);
        let (t1, t2) = match (m.t1, m.t2) {
            (Some(a), Some(b)) if b > a => (a, b),
            _ => (0, dur),
        };
        let local = t - e.start;
        let k = ((local - t1) as f32 / (t2 - t1).max(1) as f32).clamp(0.0, 1.0);
        return (m.x1 + (m.x2 - m.x1) * k, m.y1 + (m.y2 - m.y1) * k);
    }
    parse_pos(&e.text).unwrap_or_else(|| default_anchor(sub, e))
}

/// Explicit position of a line (`\pos`, or the start of `\move`), if it has one.
pub fn explicit_position(e: &Event) -> Option<(f32, f32)> {
    parse_pos(&e.text).or_else(|| parse_move(&e.text).map(|m| (m.x1, m.y1)))
}

fn fmt(v: f32) -> String {
    crate::ass::fmt_num((v * 100.0).round() / 100.0)
}

/// Move a line to `(x, y)`: rewrites `\pos`, or shifts both ends of `\move` by the same delta.
pub fn set_position(sub: &Subtitle, e: &mut Event, x: f32, y: f32) {
    if let Some(m) = parse_move(&e.text) {
        let (dx, dy) = (x - m.x1, y - m.y1);
        let times = match (m.t1, m.t2) {
            (Some(a), Some(b)) => format!(",{a},{b}"),
            _ => String::new(),
        };
        e.text = set_tag(&e.text, "move", &format!("({},{},{},{}{})", fmt(m.x1 + dx), fmt(m.y1 + dy), fmt(m.x2 + dx), fmt(m.y2 + dy), times));
    } else {
        let _ = sub;
        e.text = set_tag(&e.text, "pos", &format!("({},{})", fmt(x), fmt(y)));
    }
}

/// Translate a line by a delta (works for `\pos`, `\move`, and default placement).
pub fn nudge(sub: &Subtitle, e: &mut Event, dx: f32, dy: f32) {
    let (x, y) = explicit_position(e).unwrap_or_else(|| default_anchor(sub, e));
    set_position(sub, e, x + dx, y + dy);
}

/// Set numpad alignment with `\an` (keeps the on-screen anchor if the line has `\pos`).
pub fn set_alignment(e: &mut Event, an: i32) {
    let t = remove_tags(&e.text, &["a"]);
    e.text = set_tag(&t, "an", &an.to_string());
}

/// Remove explicit placement (`\pos`, `\move`, `\an`, `\a`, `\org`) so the style decides.
pub fn clear_position(e: &mut Event) {
    e.text = remove_tags(&e.text, &["pos", "move", "an", "a", "org"]);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spans() {
        let s = tag_spans("\\pos(10,20)\\t(0,500,\\fs50\\c&HFF&)\\fscx120\\1c&H00FF00&");
        let names: Vec<&str> = s.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["pos", "t", "fscx", "1c"]);
        assert_eq!(s[2].arg, "120");
    }
    #[test]
    fn set_remove() {
        let t = "{\\an8\\pos(1,2)\\b1}Hi{\\i1}x";
        assert_eq!(set_tag(t, "pos", "(5,6)"), "{\\an8\\b1\\pos(5,6)}Hi{\\i1}x");
        assert_eq!(remove_tags("{\\pos(1,2)}Hi", &["pos"]), "Hi");
        assert_eq!(set_tag("Hi", "pos", "(1,2)"), "{\\pos(1,2)}Hi");
        // tags inside \t are not touched
        assert_eq!(remove_tags("{\\fs20\\t(\\fs40)}a", &["fs"]), "{\\t(\\fs40)}a");
        // comments in blocks survive
        assert_eq!(remove_tags("{note\\pos(1,1)}a", &["pos"]), "{note}a");
    }
    #[test]
    fn positions() {
        let mut s = Subtitle::new();
        s.info_set("PlayResX", "1920");
        s.info_set("PlayResY", "1080");
        let mut e = Event::new(0, 1000, "{\\move(0,0,100,200)}x");
        assert_eq!(position_at(&s, &e, 500), (50.0, 100.0));
        nudge(&s, &mut e, 10.0, 10.0);
        assert_eq!(parse_move(&e.text).unwrap(), Move { x1: 10.0, y1: 10.0, x2: 110.0, y2: 210.0, t1: None, t2: None });
        let mut d = Event::new(0, 1000, "plain");
        let (x, y) = default_anchor(&s, &d);
        assert_eq!((x, y), (960.0, 1080.0 - s.styles[0].margin_v as f32));
        nudge(&s, &mut d, 0.0, -100.0);
        assert_eq!(parse_pos(&d.text), Some((960.0, y - 100.0)));
    }
}
