//! Spell checking with Hunspell dictionaries (pure Rust via `spellbook`).
//!
//! English (en_US) and Thai (th_TH, LibreOffice / LEXiTRON word list) are built in; other
//! Hunspell `.aff/.dic` pairs can be loaded. Thai has no spaces, so Thai runs are segmented with
//! the dictionary itself (dynamic programming that minimises unknown characters) and only the
//! parts no dictionary word covers are reported — names and slang are the usual hits, which
//! the user dictionary takes care of.

use crate::Subtitle;
use serde::{Deserialize, Serialize};
use spellbook::Dictionary;
use std::collections::HashSet;
use unicode_segmentation::UnicodeSegmentation;

const EN_AFF: &str = include_str!("../dicts/en_US.aff");
const EN_DIC: &str = include_str!("../dicts/en_US.dic");
const TH_AFF: &str = include_str!("../dicts/th_TH.aff");
const TH_DIC: &str = include_str!("../dicts/th_TH.dic");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Script {
    Latin,
    Thai,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Miss {
    /// byte range in the event's (ASS) text
    pub start: usize,
    pub end: usize,
    pub word: String,
    pub script: Script,
}

pub struct SpellChecker {
    latin: Option<Dictionary>,
    thai: Option<Dictionary>,
    /// user dictionary (case-sensitive as typed; Latin words also match lowercase)
    pub user: HashSet<String>,
    /// "ignore all" for this session
    pub ignore: HashSet<String>,
}

fn is_thai(c: char) -> bool {
    ('\u{0E01}'..='\u{0E4E}').contains(&c) && !matches!(c, '\u{0E3F}' | '\u{0E46}' | '\u{0E2F}' | '\u{0E4F}')
}

fn is_latin_letter(c: char) -> bool {
    c.is_alphabetic() && (c as u32) < 0x0250
}

impl SpellChecker {
    /// Built-in dictionaries. Parsing takes ~0.1–0.3 s; do it off the UI thread.
    pub fn builtin(english: bool, thai: bool) -> Result<Self, String> {
        let latin = if english { Some(Dictionary::new(EN_AFF, EN_DIC).map_err(|e| format!("en_US: {e}"))?) } else { None };
        let thai = if thai { Some(Dictionary::new(TH_AFF, TH_DIC).map_err(|e| format!("th_TH: {e}"))?) } else { None };
        Ok(SpellChecker { latin, thai, user: HashSet::new(), ignore: HashSet::new() })
    }

    /// Replace the Latin-script dictionary with a Hunspell pair from disk (e.g. en_GB, fr_FR).
    pub fn load_latin(&mut self, aff: &std::path::Path, dic: &std::path::Path) -> Result<(), String> {
        let a = std::fs::read_to_string(aff).map_err(|e| e.to_string())?;
        let d = std::fs::read_to_string(dic).map_err(|e| e.to_string())?;
        self.latin = Some(Dictionary::new(&a, &d).map_err(|e| e.to_string())?);
        Ok(())
    }

    pub fn has(&self, s: Script) -> bool {
        match s {
            Script::Latin => self.latin.is_some(),
            Script::Thai => self.thai.is_some(),
        }
    }

    fn known_user(&self, w: &str) -> bool {
        self.user.contains(w) || self.ignore.contains(w) || self.user.contains(&w.to_lowercase())
    }

    fn thai_word(&self, w: &str) -> bool {
        self.known_user(w) || self.thai.as_ref().is_some_and(|d| d.check(w))
    }

    pub fn check_word(&self, w: &str) -> bool {
        if self.known_user(w) {
            return true;
        }
        if w.chars().any(is_thai) {
            return self.thai.as_ref().is_none_or(|d| d.check(w));
        }
        self.latin.as_ref().is_none_or(|d| d.check(w))
    }

    pub fn suggest(&self, w: &str, max: usize) -> Vec<String> {
        let mut out = Vec::new();
        let d = if w.chars().any(is_thai) { self.thai.as_ref() } else { self.latin.as_ref() };
        if let Some(d) = d {
            d.suggest(w, &mut out);
        }
        // rank: single words first, then by edit distance (stable, keeps Hunspell's order on ties)
        let lw: Vec<char> = w.to_lowercase().chars().collect();
        let dist = |s: &str| {
            let b: Vec<char> = s.to_lowercase().chars().collect();
            let mut prev: Vec<usize> = (0..=b.len()).collect();
            for (i, ca) in lw.iter().enumerate() {
                let mut cur = vec![i + 1; b.len() + 1];
                for (j, cb) in b.iter().enumerate() {
                    cur[j + 1] = (prev[j] + (ca != cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1);
                }
                prev = cur;
            }
            prev[b.len()]
        };
        out.sort_by_key(|s| (s.contains(' ') || s.contains('-'), dist(s)));
        out.truncate(max);
        out
    }

    /// Unknown parts of a Thai run (byte ranges relative to `run`).
    fn thai_misses(&self, run: &str) -> Vec<(usize, usize)> {
        self.thai_segment(run).0
    }

    /// Dictionary segmentation: (unknown byte ranges, number of words incl. unknown units).
    fn thai_segment(&self, run: &str) -> (Vec<(usize, usize)>, usize) {
        let units: Vec<(usize, &str)> = run.grapheme_indices(true).collect();
        let n = units.len();
        let off = |i: usize| if i < n { units[i].0 } else { run.len() };
        const MAX_WORD: usize = 24;
        // dp[i] = (unknown units, words) for prefix of i units; back[i] = (prev, known)
        let mut dp = vec![(usize::MAX, usize::MAX); n + 1];
        let mut back = vec![(0usize, false); n + 1];
        dp[0] = (0, 0);
        for i in 0..n {
            if dp[i].0 == usize::MAX {
                continue;
            }
            let cand = (dp[i].0 + 1, dp[i].1 + 1);
            if cand < dp[i + 1] {
                dp[i + 1] = cand;
                back[i + 1] = (i, false);
            }
            for j in (i + 1)..=(i + MAX_WORD).min(n) {
                if self.thai_word(&run[off(i)..off(j)]) {
                    let cand = (dp[i].0, dp[i].1 + 1);
                    if cand < dp[j] {
                        dp[j] = cand;
                        back[j] = (i, true);
                    }
                }
            }
        }
        let words = dp[n].1;
        let mut unknown = vec![false; n];
        // segments as (start unit, end unit, known)
        let mut segs: Vec<(usize, usize, bool)> = Vec::new();
        let mut j = n;
        while j > 0 {
            let (i, known) = back[j];
            if !known {
                unknown[i..j].iter_mut().for_each(|u| *u = true);
            }
            segs.push((i, j, known));
            j = i;
        }
        segs.reverse();
        // report a stray fragment together with its neighbouring words ("ราเม|ง" → "ราเมง"),
        // which is what the reader sees as one misspelled word
        let marked = unknown.clone();
        for (k, &(a, b, known)) in segs.iter().enumerate() {
            if known || !marked[a] {
                continue;
            }
            let short = b - a <= 2;
            if short {
                if let Some(&(pa, pb, true)) = k.checked_sub(1).and_then(|x| segs.get(x)) {
                    unknown[pa..pb].iter_mut().for_each(|u| *u = true);
                }
                if let Some(&(na, nb, true)) = segs.get(k + 1) {
                    unknown[na..nb].iter_mut().for_each(|u| *u = true);
                }
            }
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < n {
            if unknown[i] {
                let a = i;
                while i < n && unknown[i] {
                    i += 1;
                }
                out.push((off(a), off(i)));
            } else {
                i += 1;
            }
        }
        (out, words)
    }

    fn unknown_units(&self, run: &str) -> usize {
        self.thai_misses(run).iter().map(|(a, b)| run[*a..*b].graphemes(true).count()).sum()
    }

    /// Repair typical Thai OCR slips — a lost/extra tone mark, า↔ำ, ิ↔ี, ึ↔ื — when one or two
    /// small edits near the unknown part turn it into dictionary words. Other text is untouched.
    pub fn fix_thai_ocr(&self, text: &str) -> String {
        if self.thai.is_none() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if !is_thai(chars[i]) {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            let a = i;
            while i < chars.len() && is_thai(chars[i]) {
                i += 1;
            }
            let run: String = chars[a..i].iter().collect();
            out.push_str(&self.fix_thai_run(&run));
        }
        out
    }

    fn fix_thai_run(&self, run: &str) -> String {
        const TONES: [char; 4] = ['\u{0E48}', '\u{0E49}', '\u{0E4A}', '\u{0E4B}'];
        let swaps: [(char, char); 6] = [('\u{0E32}', '\u{0E33}'), ('\u{0E33}', '\u{0E32}'), ('\u{0E34}', '\u{0E35}'), ('\u{0E35}', '\u{0E34}'), ('\u{0E36}', '\u{0E37}'), ('\u{0E37}', '\u{0E36}')];
        let is_mark = |c: char| matches!(c, '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}');
        let mut cur = run.to_string();
        let mut u = self.unknown_units(&cur);
        for _round in 0..2 {
            if u == 0 {
                break;
            }
            let c: Vec<char> = cur.chars().collect();
            // char positions inside or next to unknown spans
            let mut near = vec![false; c.len() + 1];
            let byte_to_char: Vec<usize> = {
                let mut v = vec![0; cur.len() + 1];
                for (ci, (bi, ch)) in cur.char_indices().enumerate() {
                    for k in 0..ch.len_utf8() {
                        v[bi + k] = ci;
                    }
                }
                v[cur.len()] = c.len();
                v
            };
            for (x, y) in self.thai_misses(&cur) {
                let (cx, cy) = (byte_to_char[x], byte_to_char[y]);
                for p in cx.saturating_sub(2)..(cy + 2).min(c.len() + 1) {
                    near[p] = true;
                }
            }
            let mut best: Option<(usize, String)> = None;
            let consider = |v: Vec<char>, best: &mut Option<(usize, String)>| {
                let s: String = v.into_iter().collect();
                let uu = self.unknown_units(&s);
                if uu < u && best.as_ref().is_none_or(|b| uu < b.0) {
                    *best = Some((uu, s));
                }
            };
            for p in 0..c.len() {
                if !near[p] {
                    continue;
                }
                if is_mark(c[p]) {
                    let mut v = c.clone();
                    v.remove(p);
                    consider(v, &mut best);
                }
                for (from, to) in swaps {
                    if c[p] == from {
                        let mut v = c.clone();
                        v[p] = to;
                        consider(v, &mut best);
                    }
                }
                // insert a tone mark after a consonant or an upper/lower vowel
                let ch = c[p];
                let base = ('\u{0E01}'..='\u{0E2E}').contains(&ch) || matches!(ch, '\u{0E31}' | '\u{0E34}'..='\u{0E3A}');
                let has_tone = c.get(p + 1).is_some_and(|n| TONES.contains(n)) || (matches!(ch, '\u{0E34}'..='\u{0E3A}' | '\u{0E31}') && false);
                if base && !has_tone {
                    for t in TONES {
                        let mut v = c.clone();
                        v.insert(p + 1, t);
                        consider(v, &mut best);
                    }
                }
            }
            match best {
                Some((uu, s)) if uu == 0 || u - uu >= 2 => {
                    cur = s;
                    u = uu;
                }
                _ => break,
            }
        }
        // ำ under a tone mark is often read as า (น้ำ → น้า): take ำ when it joins words
        if u == 0 {
            let c: Vec<char> = cur.chars().collect();
            let words = self.thai_segment(&cur).1;
            for p in 1..c.len() {
                if c[p] == '\u{0E32}' && TONES.contains(&c[p - 1]) {
                    let mut v = c.clone();
                    v[p] = '\u{0E33}';
                    let s: String = v.into_iter().collect();
                    let (m, w) = self.thai_segment(&s);
                    if m.is_empty() && w < words {
                        return s;
                    }
                }
            }
        }
        cur
    }

    /// Misspellings in one ASS text (override blocks, `\N` and drawings skipped).
    pub fn check_text(&self, text: &str) -> Vec<Miss> {
        let mut out = Vec::new();
        if text.contains("\\p1") {
            return out;
        }
        // visible byte ranges
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut in_tag = false;
        let mut seg_start = 0;
        for (i, c) in text.char_indices() {
            if c == '{' && !in_tag {
                ranges.push((seg_start, i));
                in_tag = true;
            } else if c == '}' && in_tag {
                in_tag = false;
                seg_start = i + 1;
            }
        }
        if !in_tag {
            ranges.push((seg_start, text.len()));
        }
        for (a, b) in ranges {
            let seg = &text[a..b];
            let chars: Vec<(usize, char)> = seg.char_indices().collect();
            let mut k = 0;
            while k < chars.len() {
                let (i, c) = chars[k];
                // ASS escapes \N \n \h
                if c == '\\' && k + 1 < chars.len() && matches!(chars[k + 1].1, 'N' | 'n' | 'h') {
                    k += 2;
                    continue;
                }
                if is_thai(c) {
                    let s = k;
                    while k < chars.len() && is_thai(chars[k].1) {
                        k += 1;
                    }
                    let end = if k < chars.len() { chars[k].0 } else { seg.len() };
                    if self.thai.is_some() {
                        let run = &seg[i..end];
                        let _ = s;
                        for (x, y) in self.thai_misses(run) {
                            let w = &run[x..y];
                            out.push(Miss { start: a + i + x, end: a + i + y, word: w.to_string(), script: Script::Thai });
                        }
                    }
                    continue;
                }
                if is_latin_letter(c) || c.is_ascii_digit() {
                    let s = k;
                    while k < chars.len() {
                        let ch = chars[k].1;
                        let inner_apos = matches!(ch, '\'' | '’') && k + 1 < chars.len() && is_latin_letter(chars[k + 1].1) && k > s;
                        if is_latin_letter(ch) || ch.is_ascii_digit() || inner_apos {
                            k += 1;
                        } else {
                            break;
                        }
                    }
                    let end = if k < chars.len() { chars[k].0 } else { seg.len() };
                    let w = &seg[i..end];
                    let skip = w.chars().count() < 2 || w.chars().any(|c| c.is_ascii_digit()) || (w.chars().all(|c| c.is_uppercase()) && w.chars().count() <= 5);
                    if !skip && self.latin.is_some() && !self.check_word(&w.replace('’', "'")) {
                        out.push(Miss { start: a + i, end: a + end, word: w.to_string(), script: Script::Latin });
                    }
                    continue;
                }
                k += 1;
            }
        }
        out
    }
}

/// All misspellings of a subtitle, grouped by word (in first-seen order).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordReport {
    pub word: String,
    pub script: Script,
    /// (event index, byte start, byte end)
    pub places: Vec<(usize, usize, usize)>,
}

pub fn check_subtitle(sp: &SpellChecker, sub: &Subtitle) -> Vec<WordReport> {
    let mut out: Vec<WordReport> = Vec::new();
    let mut pos: std::collections::HashMap<String, usize> = Default::default();
    for (i, e) in sub.events.iter().enumerate() {
        if e.comment || e.effect.starts_with(crate::anim::EFFECT_GEN) {
            continue;
        }
        for m in sp.check_text(&e.text) {
            let k = *pos.entry(m.word.clone()).or_insert_with(|| {
                out.push(WordReport { word: m.word.clone(), script: m.script, places: Vec::new() });
                out.len() - 1
            });
            out[k].places.push((i, m.start, m.end));
        }
    }
    out
}

/// Replace one occurrence (byte range) and return the new text, if it still matches.
pub fn replace_at(text: &str, start: usize, end: usize, expect: &str, with: &str) -> Option<String> {
    if text.get(start..end) != Some(expect) {
        return None;
    }
    Some(format!("{}{}{}", &text[..start], with, &text[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_and_thai() {
        let t = std::time::Instant::now();
        let mut sp = SpellChecker::builtin(true, true).unwrap();
        eprintln!("load {:?}", t.elapsed());
        let text = "{\\i1}Helo world{\\i0}\\Nแมวกินปลา และ หมากระโดดด ไปบ้าน NASA it's 3D";
        let m = sp.check_text(text);
        let words: Vec<&str> = m.iter().map(|m| m.word.as_str()).collect();
        assert!(words.contains(&"Helo"), "{words:?}");
        assert!(!words.contains(&"world") && !words.contains(&"NASA") && !words.contains(&"it's"), "{words:?}");
        // "หมากระโดดด": หมา + กระโดด are words, the extra ด is flagged
        assert!(words.contains(&"กระโดดด"), "{words:?}");
        assert!(!words.iter().any(|w| w.contains("แมว") || w.contains("ปลา") || w.contains("บ้าน")), "{words:?}");
        for x in &m {
            assert_eq!(&text[x.start..x.end], x.word);
        }
        assert!(sp.suggest("Helo", 5).iter().any(|s| s == "Hello"));
        // names: flagged until added to the user dictionary
        assert!(!sp.check_text("ฮารุโตะคุง").is_empty());
        // OCR slips fixed by the dictionary
        for (bad, good) in [("สวัสดิครับ", "สวัสดีครับ"), ("โรงเรียน", "โรงเรียน"), ("พรุงนี้", "พรุ่งนี้")] {
            assert_eq!(sp.fix_thai_ocr(bad), good, "{bad}");
        }
        // ambiguous (นำ + แข็ง are both words) stays as is
        assert_eq!(sp.fix_thai_ocr("นำแข็ง"), "นำแข็ง");
        assert_eq!(sp.fix_thai_ocr("น้าแข็ง กำลังละลาย"), "น้ำแข็ง กำลังละลาย");
        assert_eq!(sp.fix_thai_ocr("น้าสาว"), "น้าสาว");
        sp.user.insert("ฮารุโตะคุง".into());
        assert!(sp.check_text("ฮารุโตะคุงไปโรงเรียน").is_empty(), "{:?}", sp.check_text("ฮารุโตะคุงไปโรงเรียน"));
    }
}
