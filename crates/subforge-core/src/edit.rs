//! Editing operations shared by the GUI and the CLI.

use crate::model::{Event, Subtitle};
use crate::text::{balance_two_lines, count_chars, plain_text, strip_tags, CountOpts};
use regex::{Regex, RegexBuilder};

/// Split event `idx` into two. If the text has a `\N`, split there; otherwise in the middle.
/// Time is split at `at_ms` when given, else proportionally to text length.
pub fn split(sub: &mut Subtitle, idx: usize, at_ms: Option<i64>) -> Option<usize> {
    let e = sub.events.get(idx)?.clone();
    let (a, b) = if let Some(p) = e.text.find("\\N") {
        (e.text[..p].to_string(), e.text[p + 2..].to_string())
    } else {
        let balanced = balance_two_lines(&e.text);
        match balanced.split_once("\\N") {
            Some((x, y)) => (x.to_string(), y.to_string()),
            None => (e.text.clone(), String::new()),
        }
    };
    let split_t = at_ms.filter(|t| *t > e.start && *t < e.end).unwrap_or_else(|| {
        let ca = count_chars(&plain_text(&a), CountOpts::default()).max(1) as f64;
        let cb = count_chars(&plain_text(&b), CountOpts::default()).max(1) as f64;
        e.start + ((e.duration() as f64) * ca / (ca + cb)).round() as i64
    });
    // carry leading override block (e.g. {\an8}) to the second half
    let lead = leading_block(&e.text);
    let mut first = e.clone();
    first.text = a;
    first.end = split_t;
    let mut second = e;
    second.text = if !lead.is_empty() && !b.starts_with('{') { format!("{}{}", lead, b) } else { b };
    second.start = split_t;
    sub.events[idx] = first;
    sub.events.insert(idx + 1, second);
    Some(idx + 1)
}

fn leading_block(text: &str) -> String {
    if text.starts_with('{') {
        if let Some(j) = text.find('}') {
            return text[..=j].to_string();
        }
    }
    String::new()
}

/// Merge the given events (indices need not be contiguous) into the first one.
pub fn merge(sub: &mut Subtitle, idxs: &[usize], join_with_newline: bool) -> Option<usize> {
    let mut v: Vec<usize> = idxs.iter().copied().filter(|&i| i < sub.events.len()).collect();
    v.sort();
    v.dedup();
    if v.len() < 2 {
        return None;
    }
    let sep = if join_with_newline { "\\N" } else { " " };
    let first = v[0];
    let start = v.iter().map(|&i| sub.events[i].start).min().unwrap();
    let end = v.iter().map(|&i| sub.events[i].end).max().unwrap();
    let text = v.iter().map(|&i| sub.events[i].text.clone()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(sep);
    for &i in v.iter().skip(1).rev() {
        sub.events.remove(i);
    }
    let e = &mut sub.events[first];
    e.start = start;
    e.end = end;
    e.text = text;
    Some(first)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum FindField {
    #[default]
    Text,
    Actor,
    Style,
    Effect,
}

#[derive(Debug, Clone, Default)]
pub struct FindOpts {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub field: FindField,
}

impl FindOpts {
    pub fn build(&self) -> Result<Regex, String> {
        let mut pat = if self.regex { self.pattern.clone() } else { regex::escape(&self.pattern) };
        if self.whole_word {
            pat = format!(r"\b(?:{})\b", pat);
        }
        RegexBuilder::new(&pat).case_insensitive(!self.case_sensitive).build().map_err(|e| e.to_string())
    }
}

fn field_mut(e: &mut Event, f: FindField) -> &mut String {
    match f {
        FindField::Text => &mut e.text,
        FindField::Actor => &mut e.actor,
        FindField::Style => &mut e.style,
        FindField::Effect => &mut e.effect,
    }
}

fn field_ref(e: &Event, f: FindField) -> &str {
    match f {
        FindField::Text => &e.text,
        FindField::Actor => &e.actor,
        FindField::Style => &e.style,
        FindField::Effect => &e.effect,
    }
}

/// Find next matching event index after `from` (wrapping around).
pub fn find_next(sub: &Subtitle, opts: &FindOpts, from: Option<usize>, backwards: bool) -> Result<Option<usize>, String> {
    if opts.pattern.is_empty() || sub.events.is_empty() {
        return Ok(None);
    }
    let re = opts.build()?;
    let n = sub.events.len();
    for k in 1..=n {
        let i = match (from, backwards) {
            (Some(f), false) => (f + k) % n,
            (Some(f), true) => (f + n * 2 - k) % n,
            (None, false) => (k - 1) % n,
            (None, true) => n - k,
        };
        if re.is_match(field_ref(&sub.events[i], opts.field)) {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

/// Replace in one event. Returns number of replacements.
pub fn replace_in(sub: &mut Subtitle, idx: usize, opts: &FindOpts, replacement: &str) -> Result<usize, String> {
    let re = opts.build()?;
    let Some(e) = sub.events.get_mut(idx) else { return Ok(0) };
    let f = field_mut(e, opts.field);
    let n = re.find_iter(f).count();
    if n > 0 {
        let rep = if opts.regex { replacement.to_string() } else { replacement.replace('$', "$$") };
        *f = re.replace_all(f, rep.as_str()).into_owned();
    }
    Ok(n)
}

/// Replace in all events. Returns total replacements.
pub fn replace_all(sub: &mut Subtitle, opts: &FindOpts, replacement: &str) -> Result<usize, String> {
    if opts.pattern.is_empty() {
        return Ok(0);
    }
    let mut total = 0;
    for i in 0..sub.events.len() {
        total += replace_in(sub, i, opts, replacement)?;
    }
    Ok(total)
}

/// Remove all override tags from every event.
pub fn strip_all_tags(sub: &mut Subtitle) -> usize {
    let mut n = 0;
    for e in sub.events.iter_mut() {
        let s = strip_tags(&e.text);
        if s != e.text {
            e.text = s;
            n += 1;
        }
    }
    n
}

/// Insert a new empty event after `idx` (or at end), timed right after it.
pub fn insert_after(sub: &mut Subtitle, idx: Option<usize>, duration: i64) -> usize {
    let (pos, start, style) = match idx.and_then(|i| sub.events.get(i).map(|e| (i, e))) {
        Some((i, e)) => (i + 1, e.end, e.style.clone()),
        None => (sub.events.len(), sub.last_end(), sub.styles.first().map(|s| s.name.clone()).unwrap_or_else(|| "Default".into())),
    };
    let mut e = Event::new(start, start + duration, "");
    e.style = style;
    sub.events.insert(pos, e);
    pos
}

/// Insert before `idx`, ending where it starts.
pub fn insert_before(sub: &mut Subtitle, idx: usize, duration: i64) -> usize {
    let idx = idx.min(sub.events.len());
    let (end, style) = sub.events.get(idx).map(|e| (e.start, e.style.clone())).unwrap_or((0, "Default".into()));
    let mut e = Event::new((end - duration).max(0), end, "");
    e.style = style;
    sub.events.insert(idx, e);
    idx
}

/// Pair events of `reference` with events of `target` (for translation mode).
/// Returns for each target index the best overlapping reference index.
pub fn match_by_time(target: &Subtitle, reference: &Subtitle) -> Vec<Option<usize>> {
    target
        .events
        .iter()
        .map(|t| {
            reference
                .events
                .iter()
                .enumerate()
                .filter(|(_, r)| !r.comment)
                .map(|(i, r)| (i, (t.end.min(r.end) - t.start.max(r.start)).max(0)))
                .filter(|(_, ov)| *ov > 0)
                .max_by_key(|(_, ov)| *ov)
                .map(|(i, _)| i)
                .or_else(|| {
                    reference
                        .events
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| !r.comment)
                        .min_by_key(|(_, r)| (r.start - t.start).abs())
                        .filter(|(_, r)| (r.start - t.start).abs() < 1000)
                        .map(|(i, _)| i)
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_merge() {
        let mut s = Subtitle::new();
        s.events.push(Event::new(0, 4000, "{\\an8}Hello\\Nworld"));
        split(&mut s, 0, None).unwrap();
        assert_eq!(s.events.len(), 2);
        assert_eq!(s.events[1].text, "{\\an8}world");
        assert_eq!(s.events[0].end, s.events[1].start);
        merge(&mut s, &[0, 1], true).unwrap();
        assert_eq!(s.events.len(), 1);
        assert_eq!(s.events[0].end, 4000);
    }
    #[test]
    fn replace() {
        let mut s = Subtitle::new();
        s.events.push(Event::new(0, 1, "cat Cat"));
        let n = replace_all(&mut s, &FindOpts { pattern: "cat".into(), ..Default::default() }, "dog").unwrap();
        assert_eq!(n, 2);
        assert_eq!(s.events[0].text, "dog dog");
    }
}
