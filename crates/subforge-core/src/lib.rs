//! SubForge core: subtitle model, SRT/ASS/SSA/VTT I/O, timing tools, QC and editing helpers.
//! Used by both the egui editor and the CLI, and usable as a library from other apps.

pub mod anim;
pub mod audio;
pub mod ass;
pub mod edit;
pub mod encoding;
pub mod extract;
pub mod fixes;
pub mod fonts;
pub mod model;
pub mod ocr;
pub mod qc;
pub mod speech;
pub mod spell;
pub mod srt;
pub mod tags;
pub mod text;
pub mod time;
pub mod timing;
pub mod tools;
pub mod track;
pub mod vtt;

pub use model::{Color, Event, Format, Style, Subtitle};
pub use text::HtmlOpts;

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error on {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("cannot detect subtitle format of {0}")]
    UnknownFormat(String),
    #[error("{0} cannot be read (write-only format)")]
    WriteOnly(&'static str),
    #[error("encoding error: {0}")]
    Encoding(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A loaded file with its metadata.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub sub: Subtitle,
    pub format: Format,
    pub encoding: String,
    pub had_bom: bool,
}

/// Options for serialising.
#[derive(Debug, Clone)]
pub struct WriteOpts {
    /// "utf-8", "utf-8-bom", "utf-16le", "windows-874", "shift_jis", ...
    pub encoding: String,
    /// Use CRLF line endings (Windows tools sometimes expect this for SRT).
    pub crlf: bool,
    pub html: HtmlOpts,
}

impl Default for WriteOpts {
    fn default() -> Self {
        WriteOpts { encoding: "utf-8".into(), crlf: false, html: HtmlOpts::default() }
    }
}

/// Parse text content in a given (or auto-detected) format.
pub fn parse_str(content: &str, format: Option<Format>) -> Result<(Subtitle, Format)> {
    let fmt = match format {
        Some(Format::Txt) | None => Format::detect(content).ok_or_else(|| Error::UnknownFormat("<text>".into()))?,
        Some(f) => {
            // trust content over extension when they clearly disagree (e.g. .srt that is actually ASS)
            match Format::detect(content) {
                Some(d) if d != f && !(matches!(d, Format::Ass | Format::Ssa) && matches!(f, Format::Ass | Format::Ssa)) => d,
                _ => f,
            }
        }
    };
    let sub = match fmt {
        Format::Srt => srt::parse(content),
        Format::Ass | Format::Ssa => ass::parse(content),
        Format::Vtt => vtt::parse(content),
        Format::Txt => return Err(Error::WriteOnly("txt")),
    };
    Ok((sub, fmt))
}

/// Read and parse a subtitle file. `force_encoding` overrides detection for non-BOM files.
pub fn load(path: impl AsRef<Path>, force_encoding: Option<&str>) -> Result<Loaded> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| Error::Io { path: path.to_path_buf(), source: e })?;
    let dec = encoding::decode(&bytes, force_encoding);
    let (sub, format) = parse_str(&dec.text, Format::from_path(path)).map_err(|e| match e {
        Error::UnknownFormat(_) => Error::UnknownFormat(path.display().to_string()),
        e => e,
    })?;
    Ok(Loaded { sub, format, encoding: dec.encoding, had_bom: dec.had_bom })
}

/// Serialise to a string in `format`.
pub fn to_string(sub: &Subtitle, format: Format, opts: &WriteOpts) -> String {
    let s = match format {
        Format::Srt => srt::write(sub, opts.html),
        Format::Ass => ass::write_ass(sub),
        Format::Ssa => ass::write_ssa(sub),
        Format::Vtt => vtt::write(sub),
        Format::Txt => sub
            .events
            .iter()
            .filter(|e| !e.comment)
            .map(|e| e.plain().replace('\u{a0}', " "))
            .collect::<Vec<_>>()
            .join("\n\n")
            + "\n",
    };
    if opts.crlf {
        s.replace('\n', "\r\n")
    } else {
        s
    }
}

/// Serialise and encode to bytes.
pub fn to_bytes(sub: &Subtitle, format: Format, opts: &WriteOpts) -> Result<Vec<u8>> {
    encoding::encode(&to_string(sub, format, opts), &opts.encoding).map_err(Error::Encoding)
}

/// Write a file (format from `format` or the path's extension, default SRT).
pub fn save(path: impl AsRef<Path>, sub: &Subtitle, format: Option<Format>, opts: &WriteOpts) -> Result<Format> {
    let path = path.as_ref();
    let fmt = format.or_else(|| Format::from_path(path)).unwrap_or(Format::Srt);
    let bytes = to_bytes(sub, fmt, opts)?;
    // write atomically-ish: temp file then rename
    let tmp = path.with_extension(format!("{}.tmp", fmt.ext()));
    std::fs::write(&tmp, &bytes).map_err(|e| Error::Io { path: tmp.clone(), source: e })?;
    std::fs::rename(&tmp, path).or_else(|_| {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)
    }).map_err(|e| Error::Io { path: path.to_path_buf(), source: e })?;
    Ok(fmt)
}

/// Is this a path with a readable subtitle extension?
pub fn is_subtitle_path(p: &Path) -> bool {
    Format::from_path(p).is_some_and(|f| f != Format::Txt)
}

/// Collect subtitle files from a list of files/directories.
pub fn collect_inputs(inputs: &[PathBuf], recursive: bool) -> Vec<PathBuf> {
    fn walk(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                if recursive {
                    walk(&p, recursive, out);
                }
            } else if is_subtitle_path(&p) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for i in inputs {
        if i.is_dir() {
            walk(i, recursive, &mut out);
        } else {
            out.push(i.clone());
        }
    }
    out
}

/// Prepare a subtitle for output in `target` format: for ASS/SSA targets coming from SRT/VTT,
/// make sure styles exist; for SRT/VTT nothing is needed.
pub fn prepare_for(sub: &mut Subtitle, target: Format) {
    if matches!(target, Format::Ass | Format::Ssa) {
        sub.ensure_defaults();
        let default = sub.styles.first().map(|s| s.name.clone()).unwrap_or_else(|| "Default".into());
        let names: Vec<String> = sub.styles.iter().map(|s| s.name.clone()).collect();
        for e in sub.events.iter_mut() {
            if !names.iter().any(|n| n == &e.style) {
                e.style = default.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn srt_to_ass_to_srt() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\n<i>สวัสดี</i>\nครับ\n\n";
        let (mut sub, f) = parse_str(srt, None).unwrap();
        assert_eq!(f, Format::Srt);
        prepare_for(&mut sub, Format::Ass);
        let ass = to_string(&sub, Format::Ass, &WriteOpts::default());
        let (sub2, f2) = parse_str(&ass, None).unwrap();
        assert_eq!(f2, Format::Ass);
        let back = to_string(&sub2, Format::Srt, &WriteOpts::default());
        assert_eq!(back, srt);
    }
    #[test]
    fn thai_encoding() {
        let bytes = encoding::encode("1\n00:00:01,000 --> 00:00:02,000\nภาษาไทย\n", "windows-874").unwrap();
        let d = encoding::decode(&bytes, None);
        assert!(d.text.contains("ภาษาไทย"), "{}", d.encoding);
    }
}
