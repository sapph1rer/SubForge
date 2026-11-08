//! Undo/redo storing only what changed.
//!
//! A step records the differing middle range of the event list (common prefix/suffix removed)
//! plus styles / script info / extra sections only when they changed. Editing one line in a
//! 5 000-line ASS stores one event instead of a full copy of the file.
//!
//! Continuous edits (typing, dragging on the timeline, style tweaks) open a *session*:
//! `begin` keeps one base snapshot and the diff is computed once when the session is committed.

use subforge_core::{Event, Subtitle};

#[derive(Debug, Clone)]
struct Patch {
    at: usize,
    old: Vec<Event>,
    new: Vec<Event>,
    /// Non-event parts that changed: (old, new)
    meta: Option<Box<(Meta, Meta)>>,
    cursor: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
struct Meta {
    info: Vec<(String, String)>,
    styles: Vec<subforge_core::Style>,
    extra: Vec<(String, Vec<String>)>,
}

impl Meta {
    fn of(s: &Subtitle) -> Meta {
        Meta { info: s.info.clone(), styles: s.styles.clone(), extra: s.extra_sections.clone() }
    }
    fn apply(self, s: &mut Subtitle) {
        s.info = self.info;
        s.styles = self.styles;
        s.extra_sections = self.extra;
    }
}

impl Patch {
    fn diff(before: &Subtitle, after: &Subtitle, cursor: Option<usize>) -> Option<Patch> {
        let (a, b) = (&before.events, &after.events);
        let mut p = 0;
        while p < a.len() && p < b.len() && a[p] == b[p] {
            p += 1;
        }
        let mut s = 0;
        while s < a.len() - p && s < b.len() - p && a[a.len() - 1 - s] == b[b.len() - 1 - s] {
            s += 1;
        }
        let meta_changed = before.info != after.info || before.styles != after.styles || before.extra_sections != after.extra_sections;
        if p == a.len() && p == b.len() && !meta_changed {
            return None;
        }
        Some(Patch {
            at: p,
            old: a[p..a.len() - s].to_vec(),
            new: b[p..b.len() - s].to_vec(),
            meta: meta_changed.then(|| Box::new((Meta::of(before), Meta::of(after)))),
            cursor,
        })
    }
    /// Apply in reverse (undo) or forward (redo).
    fn apply(&self, sub: &mut Subtitle, reverse: bool) {
        let (remove, insert) = if reverse { (self.new.len(), &self.old) } else { (self.old.len(), &self.new) };
        let end = (self.at + remove).min(sub.events.len());
        sub.events.splice(self.at..end, insert.iter().cloned());
        if let Some(m) = &self.meta {
            let m = if reverse { m.0.clone() } else { m.1.clone() };
            m.apply(sub);
        }
    }
    fn bytes(&self) -> usize {
        let ev = |v: &Vec<Event>| v.iter().map(|e| e.text.len() + e.style.len() + e.actor.len() + e.effect.len() + 96).sum::<usize>();
        ev(&self.old) + ev(&self.new) + self.meta.as_ref().map(|_| 4096).unwrap_or(0)
    }
}

pub struct History {
    undo: Vec<Patch>,
    redo: Vec<Patch>,
    /// Base snapshot of an open session.
    base: Option<(Subtitle, Option<usize>)>,
    max_bytes: usize,
}

impl Default for History {
    fn default() -> Self {
        History { undo: Vec::new(), redo: Vec::new(), base: None, max_bytes: 64 * 1024 * 1024 }
    }
}

impl History {
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.base = None;
    }
    /// Start a change (no-op if a session is already open).
    pub fn begin(&mut self, current: &Subtitle, cursor: Option<usize>) {
        if self.base.is_none() {
            self.base = Some((current.clone(), cursor));
        }
    }
    /// Close the open session, recording the difference to `current`.
    pub fn commit(&mut self, current: &Subtitle) {
        if let Some((base, cursor)) = self.base.take() {
            if let Some(p) = Patch::diff(&base, current, cursor) {
                self.undo.push(p);
                self.redo.clear();
                self.trim();
            }
        }
    }
    fn trim(&mut self) {
        let mut total: usize = self.undo.iter().map(Patch::bytes).sum();
        while (total > self.max_bytes || self.undo.len() > 1000) && self.undo.len() > 1 {
            total -= self.undo.remove(0).bytes();
        }
    }
    /// Undo one step; returns the cursor to restore.
    pub fn undo(&mut self, sub: &mut Subtitle) -> Option<Option<usize>> {
        self.commit(sub);
        let p = self.undo.pop()?;
        p.apply(sub, true);
        let c = p.cursor;
        self.redo.push(p);
        Some(c)
    }
    pub fn redo(&mut self, sub: &mut Subtitle) -> Option<Option<usize>> {
        self.commit(sub);
        let p = self.redo.pop()?;
        p.apply(sub, false);
        let c = p.cursor;
        self.undo.push(p);
        Some(c)
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() || self.base.is_some()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    /// Approximate memory held by the history (for the status bar / debugging).
    pub fn approx_bytes(&self) -> usize {
        self.undo.iter().chain(self.redo.iter()).map(Patch::bytes).sum()
    }
    pub fn len(&self) -> usize {
        self.undo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sub(n: usize) -> Subtitle {
        let mut s = Subtitle::new();
        for i in 0..n {
            s.events.push(Event::new(i as i64 * 1000, i as i64 * 1000 + 900, format!("line {i}")));
        }
        s
    }
    #[test]
    fn roundtrip() {
        let mut s = sub(1000);
        let orig = s.clone();
        let mut h = History::default();
        // edit one line
        h.begin(&s, Some(5));
        s.events[500].text = "changed".into();
        h.commit(&s);
        assert_eq!(h.undo[0].old.len(), 1);
        // insert + delete + style change
        h.begin(&s, Some(5));
        s.events.insert(10, Event::new(1, 2, "new"));
        s.events.remove(900);
        s.styles[0].fontsize = 99.0;
        h.commit(&s);
        let after = s.clone();
        h.undo(&mut s).unwrap();
        h.undo(&mut s).unwrap();
        assert_eq!(s, orig);
        h.redo(&mut s).unwrap();
        h.redo(&mut s).unwrap();
        assert_eq!(s, after);
        assert!(h.approx_bytes() < 300_000, "{}", h.approx_bytes());
    }
    #[test]
    fn no_change_no_step() {
        let s = sub(3);
        let mut h = History::default();
        h.begin(&s, None);
        h.commit(&s);
        assert!(!h.can_undo());
    }
}
#[cfg(test)]
mod bench {
    use super::*;
    #[test]
    fn memory_vs_snapshots() {
        let mut s = Subtitle::new();
        for i in 0..5000 {
            s.events.push(Event::new(i * 1000, i * 1000 + 900, format!("{{\\an8\\pos(960,100)}}บรรทัดที่ {i} ข้อความตัวอย่างสำหรับทดสอบ")));
        }
        let snap: usize = s.events.iter().map(|e| e.text.len() + e.style.len() + 96).sum();
        let mut h = History::default();
        for k in 0..200 {
            h.begin(&s, None);
            s.events[(k * 37) % 5000].text.push('x');
            h.commit(&s);
        }
        println!("200 single-line edits on 5000 lines: diff history ~{} KB vs snapshots ~{} MB", h.approx_bytes() / 1024, snap * 200 / 1024 / 1024);
        assert!(h.approx_bytes() * 100 < snap * 200);
    }
}
