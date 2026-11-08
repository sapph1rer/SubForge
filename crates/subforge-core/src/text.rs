//! Text helpers: ASS override-tag tokenizer, SRT/HTML <-> ASS tag conversion,
//! plain-text extraction and grapheme-aware length measurement (works for Thai/Japanese).

use crate::model::Color;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    Text(&'a str),
    /// Inner content of a `{...}` block (without braces).
    Block(&'a str),
}

/// Split ASS text into text runs and override blocks.
pub fn tokenize(s: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        match rest.find('{') {
            Some(i) => {
                if i > 0 {
                    out.push(Token::Text(&rest[..i]));
                }
                match rest[i..].find('}') {
                    Some(j) => {
                        out.push(Token::Block(&rest[i + 1..i + j]));
                        rest = &rest[i + j + 1..];
                    }
                    None => {
                        // unterminated: treat as text
                        out.push(Token::Text(&rest[i..]));
                        rest = "";
                    }
                }
            }
            None => {
                out.push(Token::Text(rest));
                rest = "";
            }
        }
    }
    out
}

const TAG_NAMES: &[&str] = &[
    "iclip", "alpha", "xbord", "ybord", "xshad", "yshad", "fscx", "fscy", "blur", "bord", "clip",
    "fade", "move", "shad", "fad", "fax", "fay", "frx", "fry", "frz", "fsp", "org", "pbo", "pos",
    "an", "be", "fe", "fn", "fr", "fs", "kf", "ko", "1c", "2c", "3c", "4c", "1a", "2a", "3a", "4a",
    "a", "b", "c", "i", "k", "K", "p", "q", "r", "s", "t", "u",
];

/// Parse an override block into `(name, argument)` pairs. Comments (text not starting with `\`) are ignored.
pub fn parse_tags(block: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = block.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        // read one tag up to the next top-level backslash
        let start = i + 1;
        let mut j = start;
        let mut depth = 0i32;
        while j < bytes.len() {
            match bytes[j] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                b'\\' if depth <= 0 => break,
                _ => {}
            }
            j += 1;
        }
        let raw = block[start..j].trim();
        if !raw.is_empty() {
            let name = TAG_NAMES
                .iter()
                .find(|n| raw.starts_with(**n))
                .map(|n| n.to_string())
                .unwrap_or_else(|| raw.chars().take_while(|c| c.is_alphanumeric()).collect());
            let arg = raw[name.len()..].trim().to_string();
            out.push((name, arg));
        }
        i = j;
    }
    out
}

/// Remove override blocks and drawing commands, convert `\N`/`\n`/`\h`. Result uses `\n` newlines.
pub fn plain_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut drawing = false;
    for tok in tokenize(s) {
        match tok {
            Token::Block(b) => {
                for (n, a) in parse_tags(b) {
                    if n == "p" {
                        drawing = a.trim().parse::<i32>().map(|v| v > 0).unwrap_or(false);
                    }
                }
            }
            Token::Text(t) => {
                if !drawing {
                    push_unescaped(&mut out, t);
                }
            }
        }
    }
    out
}

fn push_unescaped(out: &mut String, t: &str) {
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('N') | Some('n') => {
                    chars.next();
                    out.push('\n');
                    continue;
                }
                Some('h') => {
                    chars.next();
                    out.push('\u{a0}');
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
}

/// Remove all override blocks but keep `\N` line breaks.
pub fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut drawing = false;
    for tok in tokenize(s) {
        match tok {
            Token::Block(b) => {
                for (n, a) in parse_tags(b) {
                    if n == "p" {
                        drawing = a.trim().parse::<i32>().map(|v| v > 0).unwrap_or(false);
                    }
                }
            }
            Token::Text(t) if !drawing => out.push_str(t),
            _ => {}
        }
    }
    out
}

/// ASS text -> editor text (`\N` shown as a newline; tags kept).
pub fn ass_to_editable(s: &str) -> String {
    s.replace("\\N", "\n")
}

/// Editor text -> ASS text.
pub fn editable_to_ass(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\\N")
}

/// SRT/VTT style HTML-ish markup (with real newlines) -> ASS text.
pub fn html_to_ass(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    let mut out = String::with_capacity(s.len() + 16);
    let mut rest = s.as_str();
    let mut font_stack: Vec<bool> = Vec::new(); // whether each <font> set a colour
    while !rest.is_empty() {
        if rest.starts_with('<') {
            if let Some(end) = rest.find('>') {
                let tag = &rest[1..end];
                let lower = tag.trim().to_ascii_lowercase();
                let mut handled = true;
                match lower.as_str() {
                    "i" | "em" => out.push_str("{\\i1}"),
                    "/i" | "/em" => out.push_str("{\\i0}"),
                    "b" | "strong" => out.push_str("{\\b1}"),
                    "/b" | "/strong" => out.push_str("{\\b0}"),
                    "u" => out.push_str("{\\u1}"),
                    "/u" => out.push_str("{\\u0}"),
                    "s" | "strike" => out.push_str("{\\s1}"),
                    "/s" | "/strike" => out.push_str("{\\s0}"),
                    "/font" => {
                        if font_stack.pop().unwrap_or(false) {
                            out.push_str("{\\c}");
                        }
                    }
                    "br" | "br/" | "br /" => out.push_str("\\N"),
                    _ if lower.starts_with("font") => {
                        let col = extract_attr(tag, "color").and_then(|c| parse_html_color(&c));
                        if let Some(c) = col {
                            out.push_str(&format!("{{\\c{}}}", c.to_ass_tag()));
                        }
                        font_stack.push(col.is_some());
                    }
                    // WebVTT voice / class / lang / ruby / timestamps and anything else: drop
                    _ if lower.starts_with('/')
                        || lower.starts_with("v ")
                        || lower.starts_with("v.")
                        || lower.starts_with('c')
                        || lower.starts_with("lang")
                        || lower.starts_with("ruby")
                        || lower.starts_with("rt")
                        || lower.chars().next().is_some_and(|c| c.is_ascii_digit()) => {}
                    _ => handled = false,
                }
                if handled {
                    rest = &rest[end + 1..];
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap();
        if c == '\n' {
            out.push_str("\\N");
        } else {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    // collapse "{\i1}{\an8}" style adjacent blocks is unnecessary; keep as is
    out
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let i = lower.find(name)?;
    let after = tag[i + name.len()..].trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    if let Some(r) = after.strip_prefix('"') {
        Some(r[..r.find('"')?].to_string())
    } else if let Some(r) = after.strip_prefix('\'') {
        Some(r[..r.find('\'')?].to_string())
    } else {
        Some(after.split_whitespace().next()?.to_string())
    }
}

fn parse_html_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.starts_with('#') {
        let h = &s[1..];
        let h = if h.len() == 3 {
            h.chars().flat_map(|c| [c, c]).collect::<String>()
        } else {
            h.to_string()
        };
        return Color::parse_ass(&format!("#{}", &h[..6.min(h.len())]));
    }
    let named = match s.to_ascii_lowercase().as_str() {
        "white" => (255, 255, 255),
        "black" => (0, 0, 0),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "gray" | "grey" => (128, 128, 128),
        "orange" => (255, 165, 0),
        _ => return None,
    };
    Some(Color::rgba(named.0, named.1, named.2, 0))
}

/// Options for converting ASS text to SRT/VTT markup.
#[derive(Debug, Clone, Copy)]
pub struct HtmlOpts {
    pub keep_formatting: bool,
    pub keep_colors: bool,
    /// Keep `{\anX}` at the start for non-bottom alignment (widely supported SRT extension).
    pub keep_an: bool,
}

impl Default for HtmlOpts {
    fn default() -> Self {
        HtmlOpts { keep_formatting: true, keep_colors: true, keep_an: true }
    }
}

/// ASS text -> SRT/VTT markup with real newlines.
pub fn ass_to_html(s: &str, opts: HtmlOpts) -> String {
    let mut body = String::new();
    let mut prefix = String::new();
    let (mut i, mut b, mut u, mut st, mut font) = (false, false, false, false, false);
    let mut drawing = false;
    for tok in tokenize(s) {
        match tok {
            Token::Block(blk) => {
                for (name, arg) in parse_tags(blk) {
                    let on = |a: &str, cur: bool| -> bool {
                        if a.is_empty() {
                            false
                        } else {
                            a.parse::<i32>().map(|v| v != 0).unwrap_or(cur)
                        }
                    };
                    match name.as_str() {
                        "p" => drawing = arg.parse::<i32>().map(|v| v > 0).unwrap_or(false),
                        "an" if opts.keep_an && prefix.is_empty() => {
                            if let Ok(v) = arg.parse::<i32>() {
                                if (1..=9).contains(&v) && v != 2 {
                                    prefix = format!("{{\\an{}}}", v);
                                }
                            }
                        }
                        "i" | "b" | "u" | "s" if opts.keep_formatting => {
                            let (flag, tag) = match name.as_str() {
                                "i" => (&mut i, "i"),
                                "b" => (&mut b, "b"),
                                "u" => (&mut u, "u"),
                                _ => (&mut st, "s"),
                            };
                            let v = on(&arg, *flag);
                            if v != *flag {
                                body.push_str(&format!("<{}{}>", if v { "" } else { "/" }, tag));
                                *flag = v;
                            }
                        }
                        "c" | "1c" if opts.keep_colors => {
                            if font {
                                body.push_str("</font>");
                                font = false;
                            }
                            if !arg.is_empty() {
                                if let Some(c) = Color::parse_ass(&arg) {
                                    body.push_str(&format!("<font color=\"{}\">", c.to_html()));
                                    font = true;
                                }
                            }
                        }
                        "r" => {
                            if opts.keep_formatting {
                                for (flag, tag) in [(&mut st, "s"), (&mut u, "u"), (&mut b, "b"), (&mut i, "i")] {
                                    if *flag {
                                        body.push_str(&format!("</{}>", tag));
                                        *flag = false;
                                    }
                                }
                            }
                            if font {
                                body.push_str("</font>");
                                font = false;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Token::Text(t) => {
                if !drawing {
                    let mut tmp = String::new();
                    push_unescaped(&mut tmp, t);
                    body.push_str(&tmp.replace('\u{a0}', " "));
                }
            }
        }
    }
    if font {
        body.push_str("</font>");
    }
    for (flag, tag) in [(st, "s"), (u, "u"), (b, "b"), (i, "i")] {
        if flag {
            body.push_str(&format!("</{}>", tag));
        }
    }
    // remove empty pairs like <i></i>
    for t in ["i", "b", "u", "s"] {
        body = body.replace(&format!("<{t}></{t}>"), "");
    }
    format!("{}{}", prefix, body)
}

/// Count user-perceived characters (grapheme clusters). Thai vowel/tone marks are not counted separately.
pub fn grapheme_len(s: &str) -> usize {
    s.graphemes(true).count()
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CountOpts {
    pub ignore_spaces: bool,
    pub ignore_punctuation: bool,
}

impl Default for CountOpts {
    fn default() -> Self {
        CountOpts { ignore_spaces: false, ignore_punctuation: false }
    }
}

/// Number of readable characters in plain text (newlines are never counted).
pub fn count_chars(plain: &str, opts: CountOpts) -> usize {
    plain
        .graphemes(true)
        .filter(|g| {
            let c = g.chars().next().unwrap_or(' ');
            if c == '\n' || c == '\r' {
                return false;
            }
            if opts.ignore_spaces && c.is_whitespace() {
                return false;
            }
            if opts.ignore_punctuation && (c.is_ascii_punctuation() || is_cjk_punct(c)) {
                return false;
            }
            true
        })
        .count()
}

fn is_cjk_punct(c: char) -> bool {
    matches!(c, '、' | '。' | '「' | '」' | '『' | '』' | '・' | '…' | '！' | '？' | '（' | '）' | '♪' | '～' | '―' | '–' | '—' | '“' | '”' | '‘' | '’')
}

/// Characters per second.
pub fn cps(plain: &str, duration_ms: i64, opts: CountOpts) -> f64 {
    if duration_ms <= 0 {
        return f64::INFINITY;
    }
    count_chars(plain, opts) as f64 / (duration_ms as f64 / 1000.0)
}

/// Lengths (in graphemes) of each displayed line.
pub fn line_lengths(plain: &str) -> Vec<usize> {
    plain.split('\n').map(|l| grapheme_len(l.trim_end())).collect()
}

/// Numpad alignment override in text (`\an` or legacy `\a`), if any.
pub fn alignment_override(s: &str) -> Option<i32> {
    for tok in tokenize(s) {
        if let Token::Block(b) = tok {
            for (n, a) in parse_tags(b) {
                if n == "an" {
                    if let Ok(v) = a.parse::<i32>() {
                        if (1..=9).contains(&v) {
                            return Some(v);
                        }
                    }
                }
                if n == "a" {
                    if let Ok(v) = a.parse::<i32>() {
                        return Some(legacy_to_numpad(v));
                    }
                }
            }
        }
    }
    None
}

/// SSA legacy alignment (1-3 bottom, 5-7 top, 9-11 middle) -> numpad.
pub fn legacy_to_numpad(v: i32) -> i32 {
    match v {
        1..=3 => v,
        5..=7 => v + 2,
        9..=11 => v - 5,
        _ => 2,
    }
}

/// Numpad -> SSA legacy alignment.
pub fn numpad_to_legacy(v: i32) -> i32 {
    match v {
        1..=3 => v,
        4..=6 => v + 5,
        7..=9 => v - 2,
        _ => 2,
    }
}

/// Word boundaries (byte offsets, excluding 0 and len). Uses a dictionary segmenter for
/// languages written without spaces (Thai, Lao, Khmer, Burmese, Japanese, Chinese) when the
/// `thai-segment` feature is on; otherwise falls back to whitespace boundaries.
pub fn word_boundaries(s: &str) -> Vec<usize> {
    #[cfg(feature = "thai-segment")]
    {
        use icu_segmenter::{options::WordBreakInvariantOptions, WordSegmenter};
        let seg = WordSegmenter::new_auto(WordBreakInvariantOptions::default());
        seg.segment_str(s).filter(|&b| b > 0 && b < s.len()).collect()
    }
    #[cfg(not(feature = "thai-segment"))]
    {
        let mut v = Vec::new();
        let mut prev_space = false;
        for (i, c) in s.char_indices() {
            let sp = c.is_whitespace();
            if i > 0 && sp != prev_space {
                v.push(i);
            }
            prev_space = sp;
        }
        v
    }
}

/// Characters that must not start a line (closing punctuation, Thai ๆ / ฯ, small kana...).
fn no_line_start(c: char) -> bool {
    matches!(c, '.' | ',' | '!' | '?' | ':' | ';' | ')' | ']' | '}' | '"' | '\'' | '…' | '%'
        | 'ๆ' | 'ฯ' | 'ะ' | 'า' | 'ำ' | 'ๅ' | '。' | '、' | '！' | '？' | '」' | '』' | '）' | '】' | '〉' | '》' | 'ー' | '～'
        | 'ぁ' | 'ぃ' | 'ぅ' | 'ぇ' | 'ぉ' | 'っ' | 'ゃ' | 'ゅ' | 'ょ' | 'ァ' | 'ィ' | 'ゥ' | 'ェ' | 'ォ' | 'ッ' | 'ャ' | 'ュ' | 'ョ')
}
/// Characters that must not end a line (opening brackets/quotes).
fn no_line_end(c: char) -> bool {
    matches!(c, '(' | '[' | '{' | '「' | '『' | '（' | '【' | '〈' | '《' | '“')
}

/// Break one ASS line into two balanced lines.
///
/// Candidates are spaces and dictionary word boundaries (so Thai text is never cut inside a word);
/// spaces are preferred, positions inside `{...}` override blocks are never used, and the
/// balance is measured on visible characters (graphemes, tags excluded).
/// Returns the text unchanged if it is too short or has no valid break position.
pub fn balance_two_lines(text: &str) -> String {
    if text.contains("\\N") {
        return text.to_string();
    }
    // byte ranges covered by override blocks
    let mut in_block = vec![false; text.len() + 1];
    {
        let mut depth = false;
        for (i, c) in text.char_indices() {
            if c == '{' {
                depth = true;
            }
            if depth {
                for k in i..i + c.len_utf8() {
                    in_block[k] = true;
                }
            }
            if c == '}' {
                depth = false;
            }
        }
    }
    let visible = |prefix: &str| grapheme_len(&plain_text(prefix));
    let total = visible(text);
    if total < 4 {
        return text.to_string();
    }
    let half = total as f64 / 2.0;

    // (byte position, is_space)
    let mut cands: Vec<(usize, bool)> = Vec::new();
    for (i, c) in text.char_indices() {
        if c == ' ' && !in_block[i] {
            cands.push((i, true));
        }
    }
    // word boundaries computed on the text with tags blanked out (so tags don't confuse the segmenter)
    let masked: String = text.char_indices().map(|(i, c)| if in_block[i] { ' ' } else { c }).collect();
    for b in word_boundaries(&masked) {
        if in_block[b] || in_block[b.saturating_sub(1)] {
            continue;
        }
        let before = text[..b].chars().next_back();
        let after = text[b..].chars().next();
        if before.is_some_and(char::is_whitespace) || after.is_some_and(char::is_whitespace) {
            continue; // spaces are handled above
        }
        cands.push((b, false));
    }

    let mut best: Option<(f64, usize, bool)> = None;
    for (pos, is_space) in cands {
        let next_pos = if is_space { pos + 1 } else { pos };
        let next = text[next_pos..].chars().find(|c| *c != ' ');
        let prev = text[..pos].trim_end().chars().next_back();
        if next.is_some_and(no_line_start) || prev.is_some_and(no_line_end) {
            continue;
        }
        let left = visible(&text[..pos]);
        let right = total - left - usize::from(is_space);
        if left < 2 || right < 2 {
            continue;
        }
        // distance from the middle; prefer spaces and a slightly longer bottom line (pyramid)
        let mut score = (left as f64 - half).abs();
        if is_space {
            score *= 0.5;
        } else {
            score += 1.5;
        }
        if left > right {
            score += 1.0;
        }
        if best.map(|b| score < b.0).unwrap_or(true) {
            best = Some((score, pos, is_space));
        }
    }
    match best {
        Some((_, pos, true)) => format!("{}\\N{}", text[..pos].trim_end(), text[pos + 1..].trim_start()),
        Some((_, pos, false)) => format!("{}\\N{}", &text[..pos], &text[pos..]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn html_roundtrip() {
        let a = html_to_ass("<i>Hello</i>\n<font color=\"#ff0000\">red</font>");
        assert_eq!(a, "{\\i1}Hello{\\i0}\\N{\\c&H0000FF&}red{\\c}");
        let h = ass_to_html(&a, HtmlOpts::default());
        assert_eq!(h, "<i>Hello</i>\n<font color=\"#ff0000\">red</font>");
    }
    #[test]
    fn tags_with_t() {
        let t = parse_tags("\\pos(10,20)\\t(0,500,\\fs50\\c&HFF&)\\i1");
        assert_eq!(t.len(), 3);
        assert_eq!(t[0], ("pos".into(), "(10,20)".into()));
        assert_eq!(t[2], ("i".into(), "1".into()));
    }
    #[test]
    fn plain() {
        assert_eq!(plain_text("{\\an8}Hi\\Nthere{\\p1}m 0 0 l 1 1{\\p0}!"), "Hi\nthere!");
        assert_eq!(alignment_override("{\\an8}x"), Some(8));
    }
    #[test]
    fn balance() {
        assert_eq!(balance_two_lines("hello there my good friend"), "hello there\\Nmy good friend");
        assert_eq!(balance_two_lines("{\\i1}hello there{\\i0} my friend"), "{\\i1}hello there{\\i0}\\Nmy friend");
        #[cfg(feature = "thai-segment")]
        {
            // no spaces: must break on a word boundary, never inside a word
            let r = balance_two_lines("วันนี้อากาศดีมากเลยนะครับเพื่อนรัก");
            let (a, b) = r.split_once("\\N").expect("should break");
            assert!(word_boundaries("วันนี้อากาศดีมากเลยนะครับเพื่อนรัก").contains(&a.len()), "{r}");
            assert!(!b.starts_with('ๆ'));
            // ๆ must stay with the previous word
            let r = balance_two_lines("เด็กๆกำลังเล่นกันอยู่ที่สนาม");
            assert!(!r.contains("\\Nๆ"), "{r}");
        }
    }
    #[test]
    fn thai_graphemes() {
        // "น้ำ" = 3 code points incl. tone mark; graphemes = 2 (น้, ำ)
        assert!(grapheme_len("น้ำ") < "น้ำ".chars().count());
        assert_eq!(count_chars("ab c\nd", CountOpts { ignore_spaces: true, ignore_punctuation: false }), 4);
    }
}
