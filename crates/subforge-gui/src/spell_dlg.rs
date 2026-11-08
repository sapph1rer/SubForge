//! Spell check window, red underlines in the line editor, and "Fix common errors".

use crate::app::App;
use crate::icons::{self, btn};
use eframe::egui::{self, Color32, RichText};
use std::sync::mpsc::Receiver;
use subforge_core::fixes::{self, Change, Rule};
use subforge_core::spell::{self, SpellChecker, WordReport};

/// Lazily loaded dictionaries (parsed on a background thread).
#[derive(Default)]
pub struct Speller {
    pub checker: Option<SpellChecker>,
    rx: Option<Receiver<Result<SpellChecker, String>>>,
    /// (english, thai) the checker was built for
    built_for: Option<(bool, bool)>,
}

#[derive(Default)]
pub struct SpellDlg {
    pub open: bool,
    words: Vec<WordReport>,
    gen: Option<u64>,
    pos: usize,
    replacement: String,
    suggestions: Vec<String>,
    sugg_for: String,
}

#[derive(Default)]
pub struct FixDlg {
    pub open: bool,
    changes: Vec<Change>,
    accepted: Vec<bool>,
    key: Option<(u64, Vec<String>)>,
}

impl App {
    /// Start loading dictionaries if needed; returns the checker once ready.
    pub fn speller(&mut self, ctx: &egui::Context) -> Option<&SpellChecker> {
        let want = (self.settings.spell_en, self.settings.spell_th);
        if let Some(rx) = &self.speller.rx {
            if let Ok(r) = rx.try_recv() {
                self.speller.rx = None;
                match r {
                    Ok(mut c) => {
                        c.user.extend(self.settings.spell_user.iter().cloned());
                        self.speller.checker = Some(c);
                    }
                    Err(e) => self.flash_err(format!("Spell check: {e}")),
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        if self.speller.built_for != Some(want) && self.speller.rx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(SpellChecker::builtin(want.0, want.1));
            });
            self.speller.rx = Some(rx);
            self.speller.built_for = Some(want);
            self.speller.checker = None;
        }
        self.speller.checker.as_ref()
    }

}

/// Layout for the editor text box: misspelled words get a red underline.
pub fn spell_layout_job(checker: Option<&SpellChecker>, text: &str, font: egui::FontId, color: Color32, wrap: f32) -> egui::text::LayoutJob {
    {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = wrap;
        let normal = egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
        let bad = egui::TextFormat { font_id: font, color, underline: egui::Stroke::new(1.5, Color32::from_rgb(235, 70, 70)), ..Default::default() };
        let misses = checker.map(|c| c.check_text(text)).unwrap_or_default();
        let mut at = 0;
        for m in misses {
            if m.start < at || m.end > text.len() {
                continue;
            }
            job.append(&text[at..m.start], 0.0, normal.clone());
            job.append(&text[m.start..m.end], 0.0, bad.clone());
            at = m.end;
        }
        job.append(&text[at..], 0.0, normal);
        job
    }
}

impl App {
    fn add_user_word(&mut self, w: &str) {
        if !self.settings.spell_user.iter().any(|x| x == w) {
            self.settings.spell_user.push(w.to_string());
        }
        if let Some(c) = &mut self.speller.checker {
            c.user.insert(w.to_string());
        }
    }

    pub fn spell_dialog(&mut self, ctx: &egui::Context) {
        if !self.dialogs.spell.open {
            return;
        }
        let ready = self.speller(ctx).is_some();
        if ready && self.dialogs.spell.gen != Some(self.doc.gen) {
            let words = spell::check_subtitle(self.speller.checker.as_ref().unwrap(), &self.doc.sub);
            let d = &mut self.dialogs.spell;
            d.words = words;
            d.gen = Some(self.doc.gen);
            d.pos = d.pos.min(d.words.len().saturating_sub(1));
        }
        // suggestions for the current word
        let cur_word = self.dialogs.spell.words.get(self.dialogs.spell.pos).map(|w| w.word.clone());
        if let Some(w) = &cur_word {
            if self.dialogs.spell.sugg_for != *w {
                self.dialogs.spell.suggestions = self.speller.checker.as_ref().map(|c| c.suggest(w, 8)).unwrap_or_default();
                self.dialogs.spell.replacement = self.dialogs.spell.suggestions.first().cloned().unwrap_or_else(|| w.clone());
                self.dialogs.spell.sugg_for = w.clone();
            }
        }
        let mut open = true;
        let mut act: Option<&'static str> = None;
        let mut goto: Option<usize> = None;
        egui::Window::new("Spell check").open(&mut open).resizable(true).default_size([640.0, 420.0]).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.settings.spell_en, "English");
                ui.checkbox(&mut self.settings.spell_th, "Thai");
                ui.checkbox(&mut self.settings.spell_underline, "Underline in editor");
                ui.label(RichText::new(format!("{} word(s) in your dictionary", self.settings.spell_user.len())).small().weak());
            });
            ui.separator();
            if !ready {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading dictionaries…");
                });
                return;
            }
            let d = &mut self.dialogs.spell;
            if d.words.is_empty() {
                ui.label(RichText::new("No unknown words.").color(Color32::from_rgb(90, 190, 110)));
                return;
            }
            ui.columns(2, |cols| {
                egui::ScrollArea::vertical().id_salt("sp_words").auto_shrink([false, false]).show(&mut cols[0], |ui| {
                    for (i, w) in d.words.iter().enumerate() {
                        if ui.add(egui::Button::selectable(d.pos == i, format!("{}   ×{}", w.word, w.places.len())).truncate()).clicked() {
                            d.pos = i;
                            goto = w.places.first().map(|p| p.0);
                        }
                    }
                });
                let ui = &mut cols[1];
                let Some(w) = d.words.get(d.pos) else { return };
                ui.label(RichText::new(&w.word).size(20.0).color(Color32::from_rgb(235, 90, 90)));
                if let Some(&(ev, a, b)) = w.places.first() {
                    if let Some(e) = self.doc.sub.events.get(ev) {
                        let t = &e.text;
                        let (a, b) = (a.min(t.len()), b.min(t.len()));
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.label(RichText::new(format!("#{}  ", ev + 1)).weak());
                            ui.label(t[..a].replace("\\N", " / "));
                            ui.label(RichText::new(&t[a..b]).strong().underline());
                            ui.label(t[b..].replace("\\N", " / "));
                        });
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Replace with");
                    ui.text_edit_singleline(&mut d.replacement);
                });
                egui::ScrollArea::vertical().id_salt("sp_sugg").max_height(120.0).show(ui, |ui| {
                    for s in &d.suggestions {
                        if ui.selectable_label(d.replacement == *s, s).clicked() {
                            d.replacement = s.clone();
                        }
                    }
                    if d.suggestions.is_empty() {
                        ui.label(RichText::new("no suggestions").weak());
                    }
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.add(btn(icons::ICON_EDIT, "Change")).on_hover_text("This occurrence").clicked() {
                        act = Some("change");
                    }
                    if ui.add(btn(icons::ICON_DONE_ALL, "Change all")).clicked() {
                        act = Some("change_all");
                    }
                    if ui.add(btn(icons::ICON_SKIP_NEXT, "Skip")).clicked() {
                        act = Some("skip");
                    }
                    if ui.add(btn(icons::ICON_VISIBILITY_OFF, "Ignore all")).on_hover_text("For this session").clicked() {
                        act = Some("ignore");
                    }
                    if ui.add(btn(icons::ICON_BOOKMARK_ADD, "Add to dictionary")).clicked() {
                        act = Some("add");
                    }
                });
            });
        });
        self.dialogs.spell.open = open;
        if let Some(i) = goto {
            self.select_only(Some(i));
            self.scroll_to = Some(i);
        }
        let Some(a) = act else { return };
        let d = &self.dialogs.spell;
        let Some(w) = d.words.get(d.pos).cloned() else { return };
        let rep = d.replacement.clone();
        match a {
            "change" | "change_all" => {
                let places: Vec<(usize, usize, usize)> = if a == "change" { w.places.iter().take(1).copied().collect() } else { w.places.clone() };
                let n = self.modify(|s| {
                    let mut n = 0;
                    // replace from the end so earlier byte offsets stay valid
                    for &(ev, st, en) in places.iter().rev() {
                        if let Some(t) = spell::replace_at(&s.events[ev].text, st, en, &w.word, &rep) {
                            s.events[ev].text = t;
                            n += 1;
                        }
                    }
                    n
                });
                self.buf_for = None;
                self.flash(format!("Replaced {n} × \"{}\" → \"{rep}\"", w.word));
            }
            "skip" => {
                let d = &mut self.dialogs.spell;
                d.pos = (d.pos + 1).min(d.words.len().saturating_sub(1));
                if let Some(p) = d.words.get(d.pos).and_then(|w| w.places.first()) {
                    let i = p.0;
                    self.select_only(Some(i));
                    self.scroll_to = Some(i);
                }
            }
            "ignore" => {
                if let Some(c) = &mut self.speller.checker {
                    c.ignore.insert(w.word.clone());
                }
                self.dialogs.spell.gen = None;
            }
            "add" => {
                self.add_user_word(&w.word);
                self.dialogs.spell.gen = None;
                self.flash(format!("Added \"{}\" to your dictionary", w.word));
            }
            _ => {}
        }
    }

    fn enabled_rules(&self) -> Vec<Rule> {
        if self.settings.fix_rules.is_empty() {
            Rule::ALL.into_iter().filter(|r| r.default_on()).collect()
        } else {
            self.settings.fix_rules.iter().filter_map(|s| Rule::from_id(s)).collect()
        }
    }

    pub fn fix_errors_dialog(&mut self, ctx: &egui::Context) {
        if !self.dialogs.fixes.open {
            return;
        }
        let rules = self.enabled_rules();
        let key = (self.doc.gen, rules.iter().map(|r| r.id().to_string()).collect::<Vec<_>>());
        if self.dialogs.fixes.key.as_ref() != Some(&key) {
            let ch = fixes::preview(&self.doc.sub, &rules);
            self.dialogs.fixes.accepted = vec![true; ch.len()];
            self.dialogs.fixes.changes = ch;
            self.dialogs.fixes.key = Some(key);
        }
        let mut open = true;
        let mut apply = false;
        let mut new_rules: Option<Vec<String>> = None;
        let mut goto = None;
        egui::Window::new("Fix common errors").open(&mut open).resizable(true).default_size([860.0, 500.0]).show(ctx, |ui| {
            ui.columns(2, |cols| {
                cols[0].set_max_width(300.0);
                cols[0].label(RichText::new("Rules").strong());
                let mut ids: Vec<String> = rules.iter().map(|r| r.id().to_string()).collect();
                let mut changed = false;
                for r in Rule::ALL {
                    let mut on = ids.iter().any(|x| x == r.id());
                    if cols[0].checkbox(&mut on, r.label()).changed() {
                        changed = true;
                        if on {
                            ids.push(r.id().into());
                        } else {
                            ids.retain(|x| x != r.id());
                        }
                    }
                }
                if changed {
                    new_rules = Some(ids);
                }
                let ui = &mut cols[1];
                let d = &mut self.dialogs.fixes;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} change(s)", d.changes.len())).strong());
                    if ui.small_button("all").clicked() {
                        d.accepted.iter_mut().for_each(|a| *a = true);
                    }
                    if ui.small_button("none").clicked() {
                        d.accepted.iter_mut().for_each(|a| *a = false);
                    }
                    let n = d.accepted.iter().filter(|a| **a).count();
                    if ui.add_enabled(n > 0, btn(icons::ICON_AUTO_FIX_HIGH, &format!("Apply {n}"))).clicked() {
                        apply = true;
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 46.0, d.changes.len(), |ui, range| {
                    for i in range {
                        let c = &d.changes[i];
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut d.accepted[i], "");
                            if ui.link(format!("#{}", c.index + 1)).clicked() {
                                goto = Some(c.index);
                            }
                            ui.vertical(|ui| {
                                ui.label(RichText::new(c.before.replace("\\N", " / ")).color(Color32::from_rgb(220, 110, 110)));
                                ui.label(RichText::new(c.after.replace("\\N", " / ")).color(Color32::from_rgb(110, 200, 130)));
                            });
                        })
                        .response
                        .on_hover_text(c.rules.iter().map(|r| r.label()).collect::<Vec<_>>().join("\n"));
                    }
                });
            });
        });
        self.dialogs.fixes.open = open;
        if let Some(r) = new_rules {
            self.settings.fix_rules = r;
        }
        if let Some(i) = goto {
            self.select_only(Some(i));
            self.scroll_to = Some(i);
        }
        if apply {
            let chosen: Vec<Change> = self.dialogs.fixes.changes.iter().zip(&self.dialogs.fixes.accepted).filter(|(_, a)| **a).map(|(c, _)| c.clone()).collect();
            let n = self.modify(|s| fixes::apply(s, &chosen));
            self.buf_for = None;
            self.flash(format!("Fixed {n} line(s)"));
        }
    }
}
