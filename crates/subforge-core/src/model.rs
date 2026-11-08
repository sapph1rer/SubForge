use serde::{Deserialize, Serialize};

/// Supported subtitle formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Srt,
    Ass,
    Ssa,
    Vtt,
    /// Plain text dump (one event per block, write-only)
    Txt,
}

impl Format {
    pub const ALL: [Format; 5] = [Format::Srt, Format::Ass, Format::Ssa, Format::Vtt, Format::Txt];
    pub const READABLE: [Format; 4] = [Format::Srt, Format::Ass, Format::Ssa, Format::Vtt];

    pub fn ext(self) -> &'static str {
        match self {
            Format::Srt => "srt",
            Format::Ass => "ass",
            Format::Ssa => "ssa",
            Format::Vtt => "vtt",
            Format::Txt => "txt",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Format::Srt => "SubRip (.srt)",
            Format::Ass => "Advanced SubStation Alpha (.ass)",
            Format::Ssa => "SubStation Alpha (.ssa)",
            Format::Vtt => "WebVTT (.vtt)",
            Format::Txt => "Plain text (.txt)",
        }
    }
    pub fn from_ext(ext: &str) -> Option<Format> {
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "srt" => Some(Format::Srt),
            "ass" => Some(Format::Ass),
            "ssa" => Some(Format::Ssa),
            "vtt" | "webvtt" => Some(Format::Vtt),
            "txt" => Some(Format::Txt),
            _ => None,
        }
    }
    pub fn from_path(p: &std::path::Path) -> Option<Format> {
        p.extension().and_then(|e| e.to_str()).and_then(Format::from_ext)
    }
    /// Sniff format from content.
    pub fn detect(content: &str) -> Option<Format> {
        let head: String = content.trim_start_matches('\u{feff}').chars().take(4096).collect();
        let lower = head.to_ascii_lowercase();
        if lower.trim_start().starts_with("webvtt") {
            return Some(Format::Vtt);
        }
        if lower.contains("[script info]") || lower.contains("[events]") {
            if lower.contains("[v4+ styles]") || lower.contains("scripttype: v4.00+") {
                return Some(Format::Ass);
            }
            if lower.contains("[v4 styles]") {
                return Some(Format::Ssa);
            }
            return Some(Format::Ass);
        }
        if head.contains("-->") {
            return Some(Format::Srt);
        }
        None
    }
}

/// ASS colour. `a` is ASS alpha: 0 = opaque, 255 = fully transparent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 0 };
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0, a: 0 };
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }

    /// Parse `&HAABBGGRR&`, `&HBBGGRR`, `#RRGGBB` or a decimal integer (SSA).
    pub fn parse_ass(s: &str) -> Option<Color> {
        let t = s.trim().trim_end_matches('&');
        let v: u32 = if let Some(h) = t.strip_prefix("&H").or_else(|| t.strip_prefix("&h")) {
            u32::from_str_radix(h, 16).ok()?
        } else if let Some(h) = t.strip_prefix('#') {
            let v = u32::from_str_radix(h, 16).ok()?;
            // #RRGGBB -> BBGGRR
            return Some(Color { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8, a: 0 });
        } else {
            let n: i64 = t.parse().ok()?;
            n as u32
        };
        Some(Color {
            r: (v & 0xff) as u8,
            g: ((v >> 8) & 0xff) as u8,
            b: ((v >> 16) & 0xff) as u8,
            a: ((v >> 24) & 0xff) as u8,
        })
    }
    pub fn to_ass(self) -> String {
        format!("&H{:02X}{:02X}{:02X}{:02X}", self.a, self.b, self.g, self.r)
    }
    /// For override tags like `\c&HBBGGRR&`
    pub fn to_ass_tag(self) -> String {
        format!("&H{:02X}{:02X}{:02X}&", self.b, self.g, self.r)
    }
    /// SSA v4 stores colours as decimal BGR integers
    pub fn to_ssa(self) -> String {
        let v = (self.b as u32) << 16 | (self.g as u32) << 8 | self.r as u32;
        v.to_string()
    }
    pub fn to_html(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Style {
    pub name: String,
    pub fontname: String,
    pub fontsize: f32,
    pub primary: Color,
    pub secondary: Color,
    pub outline: Color,
    pub back: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub scale_x: f32,
    pub scale_y: f32,
    pub spacing: f32,
    pub angle: f32,
    pub border_style: i32,
    pub outline_w: f32,
    pub shadow: f32,
    /// Numpad alignment (1..=9), as in ASS.
    pub alignment: i32,
    pub margin_l: i32,
    pub margin_r: i32,
    pub margin_v: i32,
    pub encoding: i32,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            name: "Default".into(),
            fontname: "Arial".into(),
            fontsize: 70.0,
            primary: Color::WHITE,
            secondary: Color::rgba(255, 0, 0, 0),
            outline: Color::BLACK,
            back: Color::rgba(0, 0, 0, 128),
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
            scale_x: 100.0,
            scale_y: 100.0,
            spacing: 0.0,
            angle: 0.0,
            border_style: 1,
            outline_w: 3.5,
            shadow: 1.5,
            alignment: 2,
            margin_l: 60,
            margin_r: 60,
            margin_v: 50,
            encoding: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub start: i64,
    pub end: i64,
    /// Text in ASS notation: `\N` = line break, `{...}` = override blocks.
    pub text: String,
    #[serde(default = "default_style_name")]
    pub style: String,
    #[serde(default)]
    pub actor: String,
    #[serde(default)]
    pub layer: i32,
    #[serde(default)]
    pub margin_l: i32,
    #[serde(default)]
    pub margin_r: i32,
    #[serde(default)]
    pub margin_v: i32,
    #[serde(default)]
    pub effect: String,
    /// `Comment:` line in ASS (not rendered).
    #[serde(default)]
    pub comment: bool,
}

fn default_style_name() -> String {
    "Default".into()
}

impl Default for Event {
    fn default() -> Self {
        Event {
            start: 0,
            end: 2000,
            text: String::new(),
            style: default_style_name(),
            actor: String::new(),
            layer: 0,
            margin_l: 0,
            margin_r: 0,
            margin_v: 0,
            effect: String::new(),
            comment: false,
        }
    }
}

impl Event {
    pub fn new(start: i64, end: i64, text: impl Into<String>) -> Self {
        Event { start, end, text: text.into(), ..Default::default() }
    }
    pub fn duration(&self) -> i64 {
        self.end - self.start
    }
    /// Text without override tags, with real newlines.
    pub fn plain(&self) -> String {
        crate::text::plain_text(&self.text)
    }
    /// Text for an editor: ASS `\N` shown as real newlines, tags kept.
    pub fn editable(&self) -> String {
        crate::text::ass_to_editable(&self.text)
    }
    pub fn set_editable(&mut self, s: &str) {
        self.text = crate::text::editable_to_ass(s);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Subtitle {
    /// `[Script Info]` key/value pairs, order preserved. Comment lines stored with key ";".
    pub info: Vec<(String, String)>,
    pub styles: Vec<Style>,
    pub events: Vec<Event>,
    /// Unknown sections preserved verbatim (`[Fonts]`, `[Aegisub Project Garbage]`, ...).
    pub extra_sections: Vec<(String, Vec<String>)>,
}

impl Subtitle {
    pub fn new() -> Self {
        let mut s = Subtitle::default();
        s.ensure_defaults();
        s
    }

    pub fn info_get(&self, key: &str) -> Option<&str> {
        self.info
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
    pub fn info_set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(e) = self.info.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            e.1 = value;
        } else {
            self.info.push((key.to_string(), value));
        }
    }
    pub fn play_res(&self) -> (i32, i32) {
        let x = self.info_get("PlayResX").and_then(|v| v.trim().parse().ok());
        let y = self.info_get("PlayResY").and_then(|v| v.trim().parse().ok());
        match (x, y) {
            (Some(x), Some(y)) => (x, y),
            (Some(x), None) => (x, if x == 1280 { 1024 } else { x * 3 / 4 }),
            (None, Some(y)) => (if y == 1024 { 1280 } else { y * 4 / 3 }, y),
            (None, None) => (384, 288), // libass default
        }
    }

    /// Make sure there is a Default style and a sensible Script Info.
    pub fn ensure_defaults(&mut self) {
        if self.info_get("ScriptType").is_none() {
            self.info.insert(0, ("ScriptType".into(), "v4.00+".into()));
        }
        if self.info_get("PlayResX").is_none() && self.info_get("PlayResY").is_none() {
            self.info_set("PlayResX", "1920");
            self.info_set("PlayResY", "1080");
        }
        if self.info_get("WrapStyle").is_none() {
            self.info_set("WrapStyle", "0");
        }
        if self.info_get("ScaledBorderAndShadow").is_none() {
            self.info_set("ScaledBorderAndShadow", "yes");
        }
        if self.styles.is_empty() {
            self.styles.push(Style::default());
        }
    }

    pub fn style(&self, name: &str) -> Option<&Style> {
        self.styles
            .iter()
            .find(|s| s.name == name)
            .or_else(|| self.styles.iter().find(|s| s.name.eq_ignore_ascii_case(name)))
    }

    pub fn sort_by_time(&mut self) {
        self.events.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    }

    /// Events visible at time `t` (not comments).
    pub fn active_at(&self, t: i64) -> impl Iterator<Item = (usize, &Event)> {
        self.events
            .iter()
            .enumerate()
            .filter(move |(_, e)| !e.comment && e.start <= t && t < e.end)
    }

    pub fn last_end(&self) -> i64 {
        self.events.iter().map(|e| e.end).max().unwrap_or(0)
    }
}
