//! "Fix common errors": typo and typing-error rules for Thai and Latin text, run with a preview.
//! Only visible text is touched: override blocks `{...}` and drawings stay byte-identical.

use crate::Subtitle;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Rule {
    ThaiSaraAm,
    ThaiDoubleE,
    ThaiMarkOrder,
    ThaiDuplicateMarks,
    ThaiSpaceBeforeMark,
    ZeroWidth,
    DoubleSpaces,
    SpaceBeforePunct,
    SpaceAfterPunct,
    Dots,
    LowercaseI,
    DialogDash,
    RepeatedWord,
    SentenceCase,
    EmptyBreaks,
}

impl Rule {
    pub const ALL: [Rule; 15] = [
        Rule::ThaiSaraAm,
        Rule::ThaiDoubleE,
        Rule::ThaiMarkOrder,
        Rule::ThaiDuplicateMarks,
        Rule::ThaiSpaceBeforeMark,
        Rule::ZeroWidth,
        Rule::DoubleSpaces,
        Rule::EmptyBreaks,
        Rule::SpaceBeforePunct,
        Rule::SpaceAfterPunct,
        Rule::Dots,
        Rule::LowercaseI,
        Rule::DialogDash,
        Rule::RepeatedWord,
        Rule::SentenceCase,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Rule::ThaiSaraAm => "thai-sara-am",
            Rule::ThaiDoubleE => "thai-double-e",
            Rule::ThaiMarkOrder => "thai-mark-order",
            Rule::ThaiDuplicateMarks => "thai-dup-marks",
            Rule::ThaiSpaceBeforeMark => "thai-space-mark",
            Rule::ZeroWidth => "zero-width",
            Rule::DoubleSpaces => "spaces",
            Rule::SpaceBeforePunct => "space-before-punct",
            Rule::SpaceAfterPunct => "space-after-punct",
            Rule::Dots => "dots",
            Rule::LowercaseI => "lowercase-i",
            Rule::DialogDash => "dialog-dash",
            Rule::RepeatedWord => "repeated-word",
            Rule::SentenceCase => "sentence-case",
            Rule::EmptyBreaks => "empty-breaks",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Rule::ThaiSaraAm => "Thai: นิคหิต + สระอา → สระอำ (◌ํ า → ◌ำ)",
            Rule::ThaiDoubleE => "Thai: สระเอ สองตัว → สระแอ (เ+เ → แ)",
            Rule::ThaiMarkOrder => "Thai: vowel before tone mark (สระบน/ล่าง ก่อนวรรณยุกต์)",
            Rule::ThaiDuplicateMarks => "Thai: duplicated vowel/tone marks",
            Rule::ThaiSpaceBeforeMark => "Thai: stray space before a vowel/tone mark",
            Rule::ZeroWidth => "Remove zero-width / invisible characters",
            Rule::DoubleSpaces => "Double spaces, spaces at line start/end",
            Rule::SpaceBeforePunct => "Space before , . ! ? : ; (Latin)",
            Rule::SpaceAfterPunct => "Missing space after , . ! ? (Latin)",
            Rule::Dots => "Too many dots (.... → ...)",
            Rule::LowercaseI => "English: lone \"i\" → \"I\"",
            Rule::DialogDash => "Dialogue dash: \"-Hi\" → \"- Hi\"",
            Rule::RepeatedWord => "Repeated word (\"the the\")",
            Rule::SentenceCase => "Capitalise the first letter after . ! ? (Latin)",
            Rule::EmptyBreaks => "Empty line breaks (\\N\\N, leading/trailing \\N)",
        }
    }
    pub fn from_id(s: &str) -> Option<Rule> {
        Rule::ALL.into_iter().find(|r| r.id() == s)
    }
    /// Rules on by default (sentence case is opinionated).
    pub fn default_on(self) -> bool {
        !matches!(self, Rule::SentenceCase)
    }
}

fn is_thai_upper_lower(c: char) -> bool {
    // above/below vowels: ั ิ ี ึ ื ุ ู ฺ, and ็ (mai taikhu)
    matches!(c, '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}')
}
fn is_thai_tone(c: char) -> bool {
    matches!(c, '\u{0E48}'..='\u{0E4B}')
}
fn is_thai_mark(c: char) -> bool {
    is_thai_upper_lower(c) || is_thai_tone(c) || matches!(c, '\u{0E4C}'..='\u{0E4E}')
}
fn is_thai(c: char) -> bool {
    ('\u{0E00}'..='\u{0E7F}').contains(&c)
}

fn thai_sara_am(s: &str) -> String {
    // ํ + า → ำ ; ํ + tone + า → tone + ำ ; also tone after ำ is moved before it (น้ำ typed as นำ้)
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '\u{0E4D}' && i + 1 < c.len() && c[i + 1] == '\u{0E32}' {
            out.push('\u{0E33}');
            i += 2;
        } else if c[i] == '\u{0E4D}' && i + 2 < c.len() && is_thai_tone(c[i + 1]) && c[i + 2] == '\u{0E32}' {
            out.push(c[i + 1]);
            out.push('\u{0E33}');
            i += 3;
        } else if c[i] == '\u{0E33}' && i + 1 < c.len() && is_thai_tone(c[i + 1]) {
            out.push(c[i + 1]);
            out.push('\u{0E33}');
            i += 2;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

fn thai_mark_order(s: &str) -> String {
    let mut c: Vec<char> = s.chars().collect();
    for i in 1..c.len() {
        if is_thai_tone(c[i - 1]) && is_thai_upper_lower(c[i]) {
            c.swap(i - 1, i);
        }
    }
    c.into_iter().collect()
}

fn thai_dup_marks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if prev == Some(c) && is_thai_mark(c) {
            continue;
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

fn thai_space_before_mark(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for i in 0..c.len() {
        if c[i] == ' ' && i + 1 < c.len() && (is_thai_mark(c[i + 1]) || c[i + 1] == '\u{0E33}') && i > 0 && is_thai(c[i - 1]) {
            continue;
        }
        out.push(c[i]);
    }
    out
}

/// Thai cleanup used after OCR and by the Thai rules.
pub fn normalize_thai(s: &str) -> String {
    let s = s.replace("เเ", "แ");
    let s = thai_sara_am(&s);
    let s = thai_mark_order(&s);
    let s = thai_dup_marks(&s);
    thai_space_before_mark(&s)
}

fn latin_word_char(c: char) -> bool {
    c.is_alphabetic() && !is_thai(c) && (c as u32) < 0x2E80
}

fn apply_line_rule(rule: Rule, l: &str) -> String {
    match rule {
        Rule::ThaiSaraAm => thai_sara_am(l),
        Rule::ThaiDoubleE => l.replace("เเ", "แ"),
        Rule::ThaiMarkOrder => thai_mark_order(l),
        Rule::ThaiDuplicateMarks => thai_dup_marks(l),
        Rule::ThaiSpaceBeforeMark => thai_space_before_mark(l),
        Rule::ZeroWidth => l.chars().filter(|c| !matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}')).collect(),
        Rule::SpaceBeforePunct => {
            let c: Vec<char> = l.chars().collect();
            let mut out = String::new();
            for i in 0..c.len() {
                if c[i] == ' ' && i > 0 && latin_word_char(c[i - 1]) && i + 1 < c.len() && matches!(c[i + 1], ',' | '.' | '!' | '?' | ':' | ';') && !(c[i + 1] == '.' && c.get(i + 2) == Some(&'.')) {
                    continue;
                }
                out.push(c[i]);
            }
            out
        }
        Rule::SpaceAfterPunct => {
            let c: Vec<char> = l.chars().collect();
            let mut out = String::new();
            for i in 0..c.len() {
                out.push(c[i]);
                if matches!(c[i], ',' | '!' | '?' | '.') && i > 0 && latin_word_char(c[i - 1]) {
                    if let Some(&n) = c.get(i + 1) {
                        // "Hello,world" / "end.Next" — but not "e.g." / "U.S." (single letters) or "...".
                        let prev_word_len = c[..i].iter().rev().take_while(|x| latin_word_char(**x)).count();
                        let ok_next = if c[i] == '.' { n.is_uppercase() && prev_word_len > 1 } else { latin_word_char(n) };
                        if ok_next && latin_word_char(n) {
                            out.push(' ');
                        }
                    }
                }
            }
            out
        }
        Rule::Dots => {
            let mut out = String::new();
            let mut run = 0;
            for ch in l.chars() {
                if ch == '.' {
                    run += 1;
                    if run > 3 {
                        continue;
                    }
                } else {
                    run = 0;
                }
                out.push(ch);
            }
            out
        }
        Rule::LowercaseI => {
            let c: Vec<char> = l.chars().collect();
            let mut out = String::new();
            for i in 0..c.len() {
                let before_ok = i == 0 || !c[i - 1].is_alphanumeric();
                let after_ok = c.get(i + 1).is_none_or(|n| !n.is_alphanumeric() && *n != '.');
                // only inside Latin text (lone "i" next to Thai is left alone)
                let latin_ctx = c.iter().filter(|x| latin_word_char(**x)).count() > 3;
                if c[i] == 'i' && before_ok && after_ok && latin_ctx {
                    out.push('I');
                } else {
                    out.push(c[i]);
                }
            }
            out
        }
        Rule::DialogDash => {
            let mut c = l.chars();
            match (c.next(), c.next()) {
                (Some('-'), Some(n)) if n.is_alphanumeric() || n == '"' || n == '\'' => format!("- {}", &l[1..]),
                _ => l.to_string(),
            }
        }
        Rule::RepeatedWord => {
            let words: Vec<&str> = l.split(' ').collect();
            let mut out: Vec<&str> = Vec::new();
            for w in words {
                if let Some(p) = out.last() {
                    let clean = |s: &str| s.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
                    let (a, b) = (clean(p), clean(w));
                    if !a.is_empty() && a == b && a.chars().all(latin_word_char) && a.len() > 1 && !p.ends_with([',', '.', '!', '?', '-']) {
                        continue;
                    }
                }
                out.push(w);
            }
            out.join(" ")
        }
        Rule::DoubleSpaces | Rule::EmptyBreaks | Rule::SentenceCase => l.to_string(),
    }
}

/// Apply rules to the visible parts of an ASS text. Returns the new text.
pub fn fix_text(text: &str, rules: &[Rule], prev_ends_sentence: bool) -> String {
    if text.contains("\\p1") || text.contains("\\p2") || text.contains("\\p4") {
        return text.to_string(); // drawings
    }
    // split into (is_tag, chunk); line rules run per visual line of the visible text
    let mut pieces: Vec<(bool, String)> = Vec::new();
    let mut cur = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '{' if !in_tag => {
                if !cur.is_empty() {
                    pieces.push((false, std::mem::take(&mut cur)));
                }
                in_tag = true;
                cur.push(c);
            }
            '}' if in_tag => {
                cur.push(c);
                pieces.push((true, std::mem::take(&mut cur)));
                in_tag = false;
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        pieces.push((in_tag, cur));
    }
    for r in rules {
        for p in pieces.iter_mut().filter(|p| !p.0) {
            let lines: Vec<String> = p.1.split("\\N").map(|l| apply_line_rule(*r, l)).collect();
            p.1 = lines.join("\\N");
        }
    }
    let mut out: String = pieces.iter().map(|p| p.1.as_str()).collect();
    if rules.contains(&Rule::DoubleSpaces) {
        out = collapse_spaces(&out);
    }
    if rules.contains(&Rule::EmptyBreaks) {
        while out.contains("\\N\\N") {
            out = out.replace("\\N\\N", "\\N");
        }
        out = trim_breaks(&out);
    }
    if rules.contains(&Rule::SentenceCase) {
        out = sentence_case(&out, prev_ends_sentence);
    }
    out
}

fn collapse_spaces(s: &str) -> String {
    // outside tags only; also trims around \N
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut prev_space = false;
    let b = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap();
        if c == '{' {
            in_tag = true;
        }
        if !in_tag && c == ' ' {
            // skip spaces adjacent to \N and duplicates
            let next_is_break = s[i..].trim_start_matches(' ').starts_with("\\N");
            let prev_is_break = out.ends_with("\\N") || out.is_empty() || out.ends_with('}') && out.trim_end_matches(|c| c != '{').is_empty();
            if prev_space || next_is_break || prev_is_break {
                i += 1;
                continue;
            }
            prev_space = true;
        } else if !in_tag {
            prev_space = false;
        }
        if c == '}' {
            in_tag = false;
        }
        out.push(c);
        i += c.len_utf8();
        let _ = b;
    }
    // trailing spaces before a closing tag sequence / end
    let trimmed_len = out.trim_end_matches(' ').len();
    if !out[..trimmed_len].ends_with('}') {
        out.truncate(trimmed_len);
    }
    out
}

fn trim_breaks(s: &str) -> String {
    // leading/trailing \N (ignoring tags at the edges)
    let mut s = s.to_string();
    loop {
        let lead = s.find(|c| c != '{').map(|_| ());
        let _ = lead;
        let start_vis = visible_start(&s);
        if s[start_vis..].starts_with("\\N") {
            s.replace_range(start_vis..start_vis + 2, "");
            continue;
        }
        if s.ends_with("\\N") {
            s.truncate(s.len() - 2);
            continue;
        }
        break;
    }
    s
}

fn visible_start(s: &str) -> usize {
    let mut i = 0;
    while s[i..].starts_with('{') {
        match s[i..].find('}') {
            Some(e) => i += e + 1,
            None => break,
        }
    }
    i
}

fn sentence_case(s: &str, prev_ends: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut cap_next = prev_ends;
    let mut after_backslash = false;
    for c in s.chars() {
        if c == '{' {
            in_tag = true;
        }
        if in_tag {
            out.push(c);
            if c == '}' {
                in_tag = false;
            }
            continue;
        }
        if after_backslash {
            after_backslash = false;
            out.push(c);
            continue;
        }
        if c == '\\' {
            after_backslash = true;
            out.push(c);
            continue;
        }
        if cap_next && latin_word_char(c) {
            out.extend(c.to_uppercase());
            cap_next = false;
            continue;
        }
        if c.is_alphanumeric() {
            cap_next = false;
        }
        if matches!(c, '.' | '!' | '?') {
            cap_next = true;
        }
        out.push(c);
    }
    out
}

fn ends_sentence(text: &str) -> bool {
    let p = crate::text::plain_text(text);
    let t = p.trim_end();
    t.is_empty() || (t.ends_with(['.', '!', '?']) && !t.ends_with("..."))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub index: usize,
    pub before: String,
    pub after: String,
    /// rules that changed this line
    pub rules: Vec<Rule>,
}

/// Preview the changes `rules` would make (comments and generated frames are skipped).
pub fn preview(sub: &Subtitle, rules: &[Rule]) -> Vec<Change> {
    let mut out = Vec::new();
    let mut prev_end = true;
    for (i, e) in sub.events.iter().enumerate() {
        if e.comment || e.effect.starts_with(crate::anim::EFFECT_GEN) {
            continue;
        }
        let after = fix_text(&e.text, rules, prev_end);
        prev_end = ends_sentence(&e.text);
        if after != e.text {
            let which: Vec<Rule> = rules.iter().copied().filter(|r| fix_text(&e.text, &[*r], true) != e.text || (*r == Rule::SentenceCase && after != fix_text(&e.text, &rules.iter().copied().filter(|x| *x != Rule::SentenceCase).collect::<Vec<_>>(), true))).collect();
            out.push(Change { index: i, before: e.text.clone(), after, rules: which });
        }
    }
    out
}

/// Apply previewed changes (only those whose `before` still matches).
pub fn apply(sub: &mut Subtitle, changes: &[Change]) -> usize {
    let mut n = 0;
    for c in changes {
        if let Some(e) = sub.events.get_mut(c.index) {
            if e.text == c.before {
                e.text = c.after.clone();
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thai_rules() {
        assert_eq!(normalize_thai("น\u{0E4D}\u{0E32}"), "นำ");
        assert_eq!(normalize_thai("น\u{0E4D}\u{0E49}\u{0E32}"), "น้ำ");
        assert_eq!(normalize_thai("นำ\u{0E49}"), "น้ำ");
        assert_eq!(normalize_thai("เเมว"), "แมว");
        assert_eq!(normalize_thai("ก\u{0E48}\u{0E34}"), "ก\u{0E34}\u{0E48}");
        assert_eq!(normalize_thai("ก\u{0E48}\u{0E48}"), "ก\u{0E48}");
        assert_eq!(normalize_thai("ไม \u{0E48}"), "ไม\u{0E48}");
    }

    #[test]
    fn latin_and_tags() {
        let all = Rule::ALL;
        let t = fix_text("{\\i1}i think  so ,but -- it's the the end....{\\i0}", &all, true);
        assert_eq!(t, "{\\i1}I think so, but -- it's the end...{\\i0}");
        assert_eq!(fix_text("-Hi!\\N-Hello.World", &all, true), "- Hi!\\N- Hello. World");
        assert_eq!(fix_text("e.g. U.S. and 3.5", &[Rule::SpaceAfterPunct], true), "e.g. U.S. and 3.5");
        assert_eq!(fix_text("\\Nสวัสดี\\N\\Nครับ \\N", &all, true), "สวัสดี\\Nครับ");
        // tags untouched even if they look like errors
        assert_eq!(fix_text("{\\fnArial  Bold}hello world. next", &[Rule::DoubleSpaces, Rule::SentenceCase], true), "{\\fnArial  Bold}Hello world. Next");
        // drawing untouched
        assert_eq!(fix_text("{\\p1}m 0 0  l 1 1{\\p0}", &all, true), "{\\p1}m 0 0  l 1 1{\\p0}");
    }

    #[test]
    fn preview_lists_rules() {
        let mut sub = Subtitle::new();
        sub.events = vec![crate::Event::new(0, 1, "เเมว  ดำ"), crate::Event::new(1, 2, "ok")];
        let ch = preview(&sub, &Rule::ALL);
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].after, "แมว ดำ");
        assert!(ch[0].rules.contains(&Rule::ThaiDoubleE) && ch[0].rules.contains(&Rule::DoubleSpaces));
        assert_eq!(apply(&mut sub, &ch), 1);
    }
}
