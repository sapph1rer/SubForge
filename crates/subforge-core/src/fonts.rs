//! Font checking for ASS scripts: which fonts a script really uses (styles and `\fn`, with
//! bold/italic), where each one can be found (fonts embedded in the script, fonts attached to the
//! video, extra folders, system fonts) and collecting the needed files.
//!
//! Matching follows libass: a requested name matches a face's family name (any language),
//! typographic family, full name or PostScript name, case-insensitively. A requested bold or
//! italic that no face provides is still rendered (libass synthesizes it) but is reported.

use crate::model::Subtitle;
use crate::tags::tag_spans;
use crate::text::{tokenize, Token};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// One font request: family + bold/italic, and where it is used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FontUse {
    /// Name as written in the script (leading `@` for vertical text removed)
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    /// Event indices (0-based)
    pub lines: Vec<usize>,
    /// Styles that request it
    pub styles: Vec<String>,
}

#[derive(Clone)]
struct FontState {
    family: String,
    bold: bool,
    italic: bool,
}

fn parse_bold(arg: &str, style_bold: bool) -> bool {
    match arg.trim().parse::<i32>() {
        Ok(0) => false,
        Ok(1) => true,
        Ok(w) if w >= 100 => w >= 700,
        Ok(_) => style_bold,
        Err(_) => style_bold,
    }
}

/// Every font the script needs to render its visible text.
pub fn used_fonts(sub: &Subtitle) -> Vec<FontUse> {
    let mut map: BTreeMap<(String, bool, bool), FontUse> = BTreeMap::new();
    let style_state = |name: &str| -> FontState {
        let st = sub.style(name).cloned().unwrap_or_default();
        FontState { family: st.fontname.clone(), bold: st.bold, italic: st.italic }
    };
    for (i, e) in sub.events.iter().enumerate() {
        if e.comment {
            continue;
        }
        let base = style_state(&e.style);
        let mut cur = base.clone();
        let mut style_name = e.style.clone();
        let mut drawing = false;
        for tok in tokenize(&e.text) {
            match tok {
                Token::Block(b) => {
                    for t in tag_spans(b) {
                        match t.name.as_str() {
                            "fn" => {
                                cur.family = if t.arg.trim().is_empty() { style_state(&style_name).family } else { t.arg.trim().to_string() };
                            }
                            "b" => cur.bold = if t.arg.trim().is_empty() { style_state(&style_name).bold } else { parse_bold(&t.arg, cur.bold) },
                            "i" => cur.italic = if t.arg.trim().is_empty() { style_state(&style_name).italic } else { t.arg.trim() != "0" },
                            "r" => {
                                let n = t.arg.trim();
                                style_name = if n.is_empty() { e.style.clone() } else { n.to_string() };
                                cur = style_state(&style_name);
                            }
                            "p" => drawing = t.arg.trim().parse::<i32>().map(|v| v > 0).unwrap_or(false),
                            _ => {}
                        }
                    }
                }
                Token::Text(t) => {
                    let visible = t.replace("\\N", "").replace("\\n", "").replace("\\h", " ");
                    if drawing || visible.trim().is_empty() {
                        continue;
                    }
                    let fam = cur.family.trim().trim_start_matches('@').to_string();
                    if fam.is_empty() {
                        continue;
                    }
                    let key = (fam.to_lowercase(), cur.bold, cur.italic);
                    let u = map.entry(key).or_insert_with(|| FontUse { family: fam.clone(), bold: cur.bold, italic: cur.italic, lines: Vec::new(), styles: Vec::new() });
                    if u.lines.last() != Some(&i) {
                        u.lines.push(i);
                    }
                    if !u.styles.contains(&style_name) {
                        u.styles.push(style_name.clone());
                    }
                }
            }
        }
    }
    map.into_values().collect()
}

// ------------------------------------------------------------------ font files

/// One face inside a font file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FontFace {
    pub path: PathBuf,
    /// Face index inside a collection (.ttc)
    pub index: u32,
    /// Display family name (English if available)
    pub family: String,
    pub subfamily: String,
    /// All matchable names, lowercase (families in every language, full names, PostScript name)
    pub names: Vec<String>,
    pub bold: bool,
    pub italic: bool,
    pub weight: u16,
}

fn decode_names(face: &ttf_parser::Face) -> (String, String, Vec<String>) {
    use ttf_parser::name_id;
    let mut family = String::new();
    let mut sub = String::new();
    let mut names: Vec<String> = Vec::new();
    let mut typo_family = String::new();
    for n in face.names() {
        let Some(s) = n.to_string() else { continue };
        let s = s.trim().to_string();
        if s.is_empty() {
            continue;
        }
        let english = n.language() == ttf_parser::Language::English_UnitedStates;
        match n.name_id {
            name_id::FAMILY => {
                if family.is_empty() || english {
                    family = s.clone();
                }
            }
            name_id::TYPOGRAPHIC_FAMILY => {
                if typo_family.is_empty() || english {
                    typo_family = s.clone();
                }
            }
            name_id::SUBFAMILY => {
                if sub.is_empty() || english {
                    sub = s.clone();
                }
            }
            _ => {}
        }
        if matches!(n.name_id, name_id::FAMILY | name_id::FULL_NAME | name_id::POST_SCRIPT_NAME | name_id::TYPOGRAPHIC_FAMILY) {
            let l = s.to_lowercase();
            if !names.contains(&l) {
                names.push(l);
            }
        }
    }
    if family.is_empty() {
        family = typo_family;
    }
    (family, sub, names)
}

/// Parse every face in font data.
pub fn faces_in_data(data: &[u8], path: &Path) -> Vec<FontFace> {
    let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    let mut out = Vec::new();
    for index in 0..count.min(64) {
        let Ok(face) = ttf_parser::Face::parse(data, index) else { continue };
        let (family, subfamily, names) = decode_names(&face);
        if names.is_empty() {
            continue;
        }
        let weight = face.weight().to_number();
        out.push(FontFace { path: path.to_path_buf(), index, family, subfamily, names, bold: face.is_bold() || weight >= 600, italic: face.is_italic() || face.is_oblique(), weight });
    }
    out
}

pub fn faces_in_file(path: &Path) -> Vec<FontFace> {
    match std::fs::read(path) {
        Ok(d) => faces_in_data(&d, path),
        Err(_) => Vec::new(),
    }
}

fn is_font_file(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc"))
}

/// Searchable index of font files. Re-scanning reuses entries of unchanged files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FontIndex {
    pub faces: Vec<FontFace>,
    /// path -> (size, mtime secs) of every scanned file
    pub files: HashMap<String, (u64, u64)>,
}

fn stamp(p: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((m.len(), t))
}

impl FontIndex {
    /// Standard font folders of this OS.
    pub fn system_dirs() -> Vec<PathBuf> {
        let mut v = Vec::new();
        #[cfg(windows)]
        {
            v.push(PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into())).join("Fonts"));
            if let Some(l) = std::env::var_os("LOCALAPPDATA") {
                v.push(PathBuf::from(l).join("Microsoft").join("Windows").join("Fonts"));
            }
        }
        #[cfg(target_os = "macos")]
        {
            v.push(PathBuf::from("/System/Library/Fonts"));
            v.push(PathBuf::from("/Library/Fonts"));
            if let Some(h) = std::env::var_os("HOME") {
                v.push(PathBuf::from(h).join("Library/Fonts"));
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            v.push(PathBuf::from("/usr/share/fonts"));
            v.push(PathBuf::from("/usr/local/share/fonts"));
            if let Some(h) = std::env::var_os("HOME") {
                v.push(PathBuf::from(&h).join(".local/share/fonts"));
                v.push(PathBuf::from(&h).join(".fonts"));
            }
        }
        v
    }

    /// Scan folders (recursively). `progress(done, total)` is called as files are processed.
    pub fn scan(dirs: &[PathBuf], prev: Option<&FontIndex>, mut progress: impl FnMut(usize, usize)) -> FontIndex {
        let mut files = Vec::new();
        fn walk(d: &Path, out: &mut Vec<PathBuf>, depth: u32) {
            if depth > 6 {
                return;
            }
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out, depth + 1);
                } else if is_font_file(&p) {
                    out.push(p);
                }
            }
        }
        for d in dirs {
            walk(d, &mut files, 0);
        }
        files.sort();
        files.dedup();
        let mut by_path: HashMap<&Path, Vec<&FontFace>> = HashMap::new();
        if let Some(p) = prev {
            for f in &p.faces {
                by_path.entry(f.path.as_path()).or_default().push(f);
            }
        }
        let mut idx = FontIndex::default();
        let total = files.len();
        for (k, f) in files.iter().enumerate() {
            let st = stamp(f).unwrap_or((0, 0));
            let key = f.to_string_lossy().to_string();
            let reuse = prev.and_then(|p| p.files.get(&key)).is_some_and(|s| *s == st);
            if reuse {
                if let Some(v) = by_path.get(f.as_path()) {
                    idx.faces.extend(v.iter().map(|x| (*x).clone()));
                }
            } else {
                idx.faces.extend(faces_in_file(f));
            }
            idx.files.insert(key, st);
            if k % 25 == 0 {
                progress(k, total);
            }
        }
        progress(total, total);
        idx
    }

    /// Faces whose names match `family` (case-insensitive, `@` ignored).
    pub fn find(&self, family: &str) -> Vec<&FontFace> {
        let want = family.trim().trim_start_matches('@').to_lowercase();
        self.faces.iter().filter(|f| f.names.iter().any(|n| *n == want)).collect()
    }
}

// ------------------------------------------------------------------ embedded [Fonts]

/// Decode the ASS `[Fonts]` section (SSA uuencoding) into (file name, bytes).
pub fn embedded_fonts(sub: &Subtitle) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for (name, lines) in &sub.extra_sections {
        if !name.eq_ignore_ascii_case("fonts") {
            continue;
        }
        let mut cur: Option<(String, String)> = None;
        for l in lines {
            let l = l.trim_end_matches('\r');
            if let Some(n) = l.strip_prefix("fontname:") {
                if let Some((n0, data)) = cur.take() {
                    out.push((n0, uudecode(&data)));
                }
                cur = Some((n.trim().to_string(), String::new()));
            } else if let Some((_, data)) = cur.as_mut() {
                data.push_str(l.trim());
            }
        }
        if let Some((n0, data)) = cur.take() {
            out.push((n0, uudecode(&data)));
        }
    }
    out
}

fn uudecode(s: &str) -> Vec<u8> {
    let v: Vec<u8> = s.bytes().filter(|b| (33..=96).contains(b)).map(|b| b - 33).collect();
    let mut out = Vec::with_capacity(v.len() * 3 / 4);
    for ch in v.chunks(4) {
        let b = |i: usize| *ch.get(i).unwrap_or(&0) as u32;
        let n = (b(0) << 18) | (b(1) << 12) | (b(2) << 6) | b(3);
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        let take = match ch.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => 0,
        };
        out.extend_from_slice(&bytes[..take]);
    }
    out
}

// ------------------------------------------------------------------ check

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
pub enum Source {
    /// In the script's `[Fonts]` section
    Embedded,
    /// Attached to the video (MKV attachments)
    Attached,
    /// A folder chosen by the user
    Folder,
    System,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Embedded => "embedded in script",
            Source::Attached => "attached to video",
            Source::Folder => "fonts folder",
            Source::System => "installed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Status {
    /// Exact face available
    Ok,
    /// Family found but no bold and/or italic face — the renderer will fake it
    Synthesized { bold: bool, italic: bool },
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontReport {
    pub usage: FontUse,
    pub status: Status,
    pub source: Option<Source>,
    /// Best matching face (file to collect / attach)
    pub face: Option<FontFace>,
}

fn pick<'a>(faces: &[&'a FontFace], want_bold: bool, want_italic: bool, full_name_hit: bool) -> (&'a FontFace, Status) {
    if full_name_hit && faces.len() == 1 {
        return (faces[0], Status::Ok);
    }
    let score = |f: &FontFace| (f.bold == want_bold) as i32 * 2 + (f.italic == want_italic) as i32 * 2 - ((f.weight as i32 - if want_bold { 700 } else { 400 }).abs() / 100);
    let best = *faces.iter().max_by_key(|f| score(f)).unwrap();
    let fake_bold = want_bold && !best.bold;
    let fake_italic = want_italic && !best.italic;
    if fake_bold || fake_italic {
        (best, Status::Synthesized { bold: fake_bold, italic: fake_italic })
    } else {
        (best, Status::Ok)
    }
}

/// Check every used font against the given sources (searched in order).
pub fn check(sub: &Subtitle, sources: &[(Source, &FontIndex)]) -> Vec<FontReport> {
    used_fonts(sub)
        .into_iter()
        .map(|u| {
            // first source with an exact face wins; a faked bold/italic only if nothing better exists
            let mut fallback: Option<FontReport> = None;
            for (src, idx) in sources {
                let hits = idx.find(&u.family);
                if !hits.is_empty() {
                    // a full-name / PostScript hit ("Arial Bold") names one specific face
                    let want = u.family.to_lowercase();
                    let full = hits.iter().all(|f| f.family.to_lowercase() != want);
                    let (face, status) = pick(&hits, u.bold, u.italic, full);
                    let r = FontReport { usage: u.clone(), status: status.clone(), source: Some(*src), face: Some(face.clone()) };
                    if status == Status::Ok {
                        return r;
                    }
                    fallback.get_or_insert(r);
                }
            }
            fallback.unwrap_or(FontReport { usage: u, status: Status::Missing, source: None, face: None })
        })
        .collect()
}

/// Index the fonts embedded in the script (written to `tmp_dir` so they have a path).
pub fn index_embedded(sub: &Subtitle, tmp_dir: &Path) -> FontIndex {
    let mut idx = FontIndex::default();
    let list = embedded_fonts(sub);
    if list.is_empty() {
        return idx;
    }
    let _ = std::fs::create_dir_all(tmp_dir);
    for (name, data) in list {
        let p = tmp_dir.join(&name);
        let _ = std::fs::write(&p, &data);
        idx.faces.extend(faces_in_data(&data, &p));
    }
    idx
}

/// Copy the font files needed by the report into `dir`. Returns copied file paths.
pub fn collect(reports: &[FontReport], dir: &Path) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut done: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for r in reports {
        let Some(f) = &r.face else { continue };
        if done.contains(&f.path) {
            continue;
        }
        done.push(f.path.clone());
        let name = f.path.file_name().ok_or("bad font path")?;
        let dest = dir.join(name);
        std::fs::copy(&f.path, &dest).map_err(|e| format!("{}: {e}", f.path.display()))?;
        out.push(dest);
    }
    Ok(out)
}

/// Distinct font files used by the report (for attaching to MKV).
pub fn files(reports: &[FontReport]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = reports.iter().filter_map(|r| r.face.as_ref().map(|f| f.path.clone())).collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, Style};

    #[test]
    fn usage() {
        let mut s = Subtitle::new();
        s.styles.push(Style { name: "Sign".into(), fontname: "@Meiryo".into(), bold: true, ..Default::default() });
        s.events.push(Event::new(0, 1, "plain {\\b1}bold{\\b0}{\\fnComic Sans MS\\i1}fancy{\\r}back"));
        s.events.push(Event { style: "Sign".into(), ..Event::new(0, 1, "{\\p1}m 0 0 l 1 1{\\p0}x") });
        s.events.push(Event { comment: true, ..Event::new(0, 1, "{\\fnNotUsed}x") });
        let u = used_fonts(&s);
        let keys: Vec<(String, bool, bool)> = u.iter().map(|f| (f.family.clone(), f.bold, f.italic)).collect();
        assert!(keys.contains(&("Arial".into(), false, false)));
        assert!(keys.contains(&("Arial".into(), true, false)));
        assert!(keys.contains(&("Comic Sans MS".into(), false, true)));
        assert!(keys.contains(&("Meiryo".into(), true, false)));
        assert!(!keys.iter().any(|k| k.0 == "NotUsed"));
    }

    #[test]
    fn uu_roundtrip() {
        // encode like Aegisub does and decode back
        let data: Vec<u8> = (0u8..=200).collect();
        let mut enc = String::new();
        for ch in data.chunks(3) {
            let b = |i: usize| *ch.get(i).unwrap_or(&0) as u32;
            let n = (b(0) << 16) | (b(1) << 8) | b(2);
            let chars = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
            let keep = ch.len() + 1;
            for c in &chars[..keep] {
                enc.push((*c as u8 + 33) as char);
            }
        }
        assert_eq!(uudecode(&enc), data);
    }

    #[test]
    fn index_and_check() {
        // use whatever fonts this machine has; skip if none
        let idx = FontIndex::scan(&FontIndex::system_dirs(), None, |_, _| {});
        let Some(face) = idx.faces.iter().find(|f| !f.bold && !f.italic) else { return };
        let mut s = Subtitle::new();
        s.styles[0].fontname = face.family.to_uppercase(); // case-insensitive
        s.events.push(Event::new(0, 1, "x{\\fnDefinitely Missing Font 123}y"));
        let r = check(&s, &[(Source::System, &idx)]);
        assert_eq!(r.len(), 2);
        let found = r.iter().find(|x| x.usage.family.eq_ignore_ascii_case(&face.family)).unwrap();
        assert_ne!(found.status, Status::Missing);
        assert_eq!(r.iter().find(|x| x.usage.family.starts_with("Definitely")).unwrap().status, Status::Missing);
        // rescan reuses unchanged files
        let again = FontIndex::scan(&FontIndex::system_dirs(), Some(&idx), |_, _| {});
        assert_eq!(again.faces.len(), idx.faces.len());
    }
}
