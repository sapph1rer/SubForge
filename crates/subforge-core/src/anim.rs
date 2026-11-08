//! Tweening (animation) of subtitle lines.
//!
//! A [`Tween`] animates position, scale, rotation, alpha, colour, blur, border and font size from a
//! start state to an end state with an easing curve, plus optional fade in/out. Two output modes:
//!
//! * **Tags** — compact ASS: `\move` + `\t(t1,t2,accel,...)`. Power curves (quad, cubic, ...) map
//!   onto `\t` acceleration; ease-in-out uses two `\t` segments. `\move` is always linear.
//! * **Frame-by-frame** — the line is split into one event per video frame with exact values, so
//!   any curve (back, elastic, bounce) and eased movement are reproduced exactly by every renderer.
//!   The original line is kept as a comment so the animation can be edited or removed later.
//!
//! Settings are stored in a comment block (`{sftw:...}`) that renderers ignore, so the editor can
//! read an animation back.

use crate::model::{Color, Event, Subtitle};
use crate::tags::{self, get_tag, insert_leading, remove_tags};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EaseKind {
    #[default]
    Linear,
    Sine,
    Quad,
    Cubic,
    Quart,
    Expo,
    Back,
    Elastic,
    Bounce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EaseDir {
    In,
    #[default]
    Out,
    InOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Easing {
    pub kind: EaseKind,
    pub dir: EaseDir,
}

impl EaseKind {
    pub const ALL: [EaseKind; 9] = [
        EaseKind::Linear,
        EaseKind::Sine,
        EaseKind::Quad,
        EaseKind::Cubic,
        EaseKind::Quart,
        EaseKind::Expo,
        EaseKind::Back,
        EaseKind::Elastic,
        EaseKind::Bounce,
    ];
    pub fn name(self) -> &'static str {
        match self {
            EaseKind::Linear => "Linear",
            EaseKind::Sine => "Sine",
            EaseKind::Quad => "Quad",
            EaseKind::Cubic => "Cubic",
            EaseKind::Quart => "Quart",
            EaseKind::Expo => "Expo",
            EaseKind::Back => "Back (overshoot)",
            EaseKind::Elastic => "Elastic",
            EaseKind::Bounce => "Bounce",
        }
    }
    fn key(self) -> &'static str {
        match self {
            EaseKind::Linear => "lin",
            EaseKind::Sine => "sine",
            EaseKind::Quad => "quad",
            EaseKind::Cubic => "cubic",
            EaseKind::Quart => "quart",
            EaseKind::Expo => "expo",
            EaseKind::Back => "back",
            EaseKind::Elastic => "elastic",
            EaseKind::Bounce => "bounce",
        }
    }
    /// The "in" curve, 0..1 -> 0..1.
    fn ease_in(self, t: f32) -> f32 {
        use std::f32::consts::PI;
        match self {
            EaseKind::Linear => t,
            EaseKind::Sine => 1.0 - (t * PI / 2.0).cos(),
            EaseKind::Quad => t * t,
            EaseKind::Cubic => t * t * t,
            EaseKind::Quart => t * t * t * t,
            EaseKind::Expo => {
                if t <= 0.0 {
                    0.0
                } else {
                    2f32.powf(10.0 * t - 10.0)
                }
            }
            EaseKind::Back => {
                let c1 = 1.70158;
                let c3 = c1 + 1.0;
                c3 * t * t * t - c1 * t * t
            }
            EaseKind::Elastic => {
                if t <= 0.0 || t >= 1.0 {
                    t
                } else {
                    let c4 = 2.0 * PI / 3.0;
                    -(2f32.powf(10.0 * t - 10.0)) * ((t * 10.0 - 10.75) * c4).sin()
                }
            }
            EaseKind::Bounce => 1.0 - bounce_out(1.0 - t),
        }
    }
    /// Approximate exponent for ASS `\t` acceleration (None = not representable).
    fn power(self) -> Option<f32> {
        match self {
            EaseKind::Linear => Some(1.0),
            EaseKind::Sine => Some(1.6),
            EaseKind::Quad => Some(2.0),
            EaseKind::Cubic => Some(3.0),
            EaseKind::Quart => Some(4.0),
            EaseKind::Expo => Some(6.0),
            _ => None,
        }
    }
}

fn bounce_out(t: f32) -> f32 {
    let (n1, d1) = (7.5625, 2.75);
    if t < 1.0 / d1 {
        n1 * t * t
    } else if t < 2.0 / d1 {
        let t = t - 1.5 / d1;
        n1 * t * t + 0.75
    } else if t < 2.5 / d1 {
        let t = t - 2.25 / d1;
        n1 * t * t + 0.9375
    } else {
        let t = t - 2.625 / d1;
        n1 * t * t + 0.984375
    }
}

impl Easing {
    pub const LINEAR: Easing = Easing { kind: EaseKind::Linear, dir: EaseDir::Out };
    pub fn new(kind: EaseKind, dir: EaseDir) -> Self {
        Easing { kind, dir }
    }
    /// Evaluate the curve at progress `t` (0..1). May overshoot for Back/Elastic.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let k = self.kind;
        match self.dir {
            EaseDir::In => k.ease_in(t),
            EaseDir::Out => 1.0 - k.ease_in(1.0 - t),
            EaseDir::InOut => {
                if t < 0.5 {
                    k.ease_in(t * 2.0) / 2.0
                } else {
                    1.0 - k.ease_in((1.0 - t) * 2.0) / 2.0
                }
            }
        }
    }
    /// Whether the tags mode can reproduce this curve (otherwise frame-by-frame is needed).
    pub fn tag_compatible(self) -> bool {
        self.kind.power().is_some()
    }
    /// `\t` segments: (start fraction, end fraction, accel, value fraction reached at the end).
    fn segments(self) -> Vec<(f32, f32, f32, f32)> {
        let p = self.kind.power().unwrap_or(1.0);
        if p == 1.0 {
            return vec![(0.0, 1.0, 1.0, 1.0)];
        }
        match self.dir {
            EaseDir::In => vec![(0.0, 1.0, p, 1.0)],
            EaseDir::Out => vec![(0.0, 1.0, 1.0 / p, 1.0)],
            EaseDir::InOut => vec![(0.0, 0.5, p, 0.5), (0.5, 1.0, 1.0 / p, 1.0)],
        }
    }
    fn key(self) -> String {
        let d = match self.dir {
            EaseDir::In => "in",
            EaseDir::Out => "out",
            EaseDir::InOut => "inout",
        };
        format!("{}-{}", self.kind.key(), d)
    }
    fn from_key(s: &str) -> Easing {
        let (k, d) = s.split_once('-').unwrap_or((s, "out"));
        let kind = EaseKind::ALL.iter().copied().find(|x| x.key() == k).unwrap_or(EaseKind::Linear);
        let dir = match d {
            "in" => EaseDir::In,
            "inout" => EaseDir::InOut,
            _ => EaseDir::Out,
        };
        Easing { kind, dir }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Mode {
    #[default]
    Tags,
    FrameByFrame,
}

/// A snapshot of animatable properties.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub x: f32,
    pub y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rot_z: f32,
    pub rot_x: f32,
    pub rot_y: f32,
    /// ASS alpha: 0 = opaque, 255 = invisible
    pub alpha: u8,
    pub color: Color,
    pub blur: f32,
    pub bord: f32,
    pub fs: f32,
}

impl Default for State {
    fn default() -> Self {
        State { x: 960.0, y: 540.0, scale_x: 100.0, scale_y: 100.0, rot_z: 0.0, rot_x: 0.0, rot_y: 0.0, alpha: 0, color: Color::WHITE, blur: 0.0, bord: 2.0, fs: 50.0 }
    }
}

fn tag_f(text: &str, name: &str) -> Option<f32> {
    get_tag(text, name).and_then(|a| a.trim().trim_start_matches('(').trim_end_matches(')').parse::<f32>().ok())
}

impl State {
    /// Current look of a line at its start (tags override the style).
    pub fn of_event(sub: &Subtitle, e: &Event) -> State {
        let st = sub.style(&e.style).cloned().unwrap_or_default();
        let base = strip_generated(&e.text);
        let (x, y) = tags::position_at(sub, &Event { text: base.clone(), ..e.clone() }, e.start);
        let alpha = get_tag(&base, "alpha")
            .or_else(|| get_tag(&base, "1a"))
            .and_then(|a| u8::from_str_radix(a.trim().trim_start_matches("&H").trim_start_matches("&h").trim_end_matches('&'), 16).ok())
            .unwrap_or(st.primary.a);
        let color = get_tag(&base, "1c").and_then(|c| Color::parse_ass(&c)).map(|mut c| {
            c.a = 0;
            c
        });
        State {
            x,
            y,
            scale_x: tag_f(&base, "fscx").unwrap_or(st.scale_x),
            scale_y: tag_f(&base, "fscy").unwrap_or(st.scale_y),
            rot_z: tag_f(&base, "frz").unwrap_or(st.angle),
            rot_x: tag_f(&base, "frx").unwrap_or(0.0),
            rot_y: tag_f(&base, "fry").unwrap_or(0.0),
            alpha,
            color: color.unwrap_or(Color { a: 0, ..st.primary }),
            blur: tag_f(&base, "blur").unwrap_or(0.0),
            bord: tag_f(&base, "bord").unwrap_or(st.outline_w),
            fs: tag_f(&base, "fs").unwrap_or(st.fontsize),
        }
    }
}

/// Which properties are animated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Animate {
    pub pos: bool,
    pub scale: bool,
    pub rot_z: bool,
    pub rot_x: bool,
    pub rot_y: bool,
    pub alpha: bool,
    pub color: bool,
    pub blur: bool,
    pub bord: bool,
    pub fs: bool,
}

impl Animate {
    pub fn any(&self) -> bool {
        self.pos || self.scale || self.rot_z || self.rot_x || self.rot_y || self.alpha || self.color || self.blur || self.bord || self.fs
    }
    /// Override tags that conflict with the animated properties.
    fn conflicting(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.pos {
            v.extend(["pos", "move"]);
        }
        if self.scale {
            v.extend(["fscx", "fscy"]);
        }
        if self.rot_z {
            v.push("frz");
        }
        if self.rot_x {
            v.push("frx");
        }
        if self.rot_y {
            v.push("fry");
        }
        if self.alpha {
            v.extend(["alpha", "1a"]);
        }
        if self.color {
            v.push("1c");
        }
        if self.blur {
            v.push("blur");
        }
        if self.bord {
            v.push("bord");
        }
        if self.fs {
            v.push("fs");
        }
        v
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tween {
    pub animate: Animate,
    pub from: State,
    pub to: State,
    /// Animation window relative to the line start (ms). `t2 = None` means "until the line ends".
    pub t1: i64,
    pub t2: Option<i64>,
    pub easing: Easing,
    pub mode: Mode,
    pub fade_in: i64,
    pub fade_out: i64,
}

impl Default for Tween {
    fn default() -> Self {
        Tween {
            animate: Animate::default(),
            from: State::default(),
            to: State::default(),
            t1: 0,
            t2: None,
            easing: Easing::new(EaseKind::Cubic, EaseDir::Out),
            mode: Mode::Tags,
            fade_in: 0,
            fade_out: 0,
        }
    }
}

fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}
fn lerp_u8(a: u8, b: u8, k: f32) -> u8 {
    lerp(a as f32, b as f32, k).round().clamp(0.0, 255.0) as u8
}
fn num(v: f32) -> String {
    crate::ass::fmt_num((v * 100.0).round() / 100.0)
}
fn alpha_tag(a: u8) -> String {
    format!("&H{a:02X}&")
}

impl State {
    fn lerp(&self, o: &State, k: f32) -> State {
        State {
            x: lerp(self.x, o.x, k),
            y: lerp(self.y, o.y, k),
            scale_x: lerp(self.scale_x, o.scale_x, k),
            scale_y: lerp(self.scale_y, o.scale_y, k),
            rot_z: lerp(self.rot_z, o.rot_z, k),
            rot_x: lerp(self.rot_x, o.rot_x, k),
            rot_y: lerp(self.rot_y, o.rot_y, k),
            alpha: lerp_u8(self.alpha, o.alpha, k.clamp(0.0, 1.0)),
            color: Color {
                r: lerp_u8(self.color.r, o.color.r, k.clamp(0.0, 1.0)),
                g: lerp_u8(self.color.g, o.color.g, k.clamp(0.0, 1.0)),
                b: lerp_u8(self.color.b, o.color.b, k.clamp(0.0, 1.0)),
                a: 0,
            },
            blur: lerp(self.blur, o.blur, k).max(0.0),
            bord: lerp(self.bord, o.bord, k).max(0.0),
            fs: lerp(self.fs, o.fs, k).max(1.0),
        }
    }
    /// Non-positional animated tags for this state.
    fn tags(&self, a: &Animate, alpha_override: Option<u8>) -> String {
        let mut s = String::new();
        if a.scale {
            s += &format!("\\fscx{}\\fscy{}", num(self.scale_x), num(self.scale_y));
        }
        if a.rot_z {
            s += &format!("\\frz{}", num(self.rot_z));
        }
        if a.rot_x {
            s += &format!("\\frx{}", num(self.rot_x));
        }
        if a.rot_y {
            s += &format!("\\fry{}", num(self.rot_y));
        }
        if let Some(al) = alpha_override {
            s += &format!("\\alpha{}", alpha_tag(al));
        } else if a.alpha {
            s += &format!("\\alpha{}", alpha_tag(self.alpha));
        }
        if a.color {
            s += &format!("\\1c{}", self.color.to_ass_tag());
        }
        if a.blur {
            s += &format!("\\blur{}", num(self.blur));
        }
        if a.bord {
            s += &format!("\\bord{}", num(self.bord));
        }
        if a.fs {
            s += &format!("\\fs{}", num(self.fs));
        }
        s
    }
}

// ------------------------------------------------------------------ metadata

const META: &str = "sftw:";
pub const EFFECT_GEN: &str = "sftw:";
pub const EFFECT_SRC: &str = "sftw-src:";

fn color_hex(c: Color) -> String {
    format!("{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}
fn parse_hex(s: &str) -> Option<Color> {
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8, a: 0 })
}

impl Tween {
    fn encode(&self) -> String {
        let a = &self.animate;
        let mut flags = Vec::new();
        for (on, n) in [
            (a.pos, "pos"),
            (a.scale, "scale"),
            (a.rot_z, "rz"),
            (a.rot_x, "rx"),
            (a.rot_y, "ry"),
            (a.alpha, "alpha"),
            (a.color, "color"),
            (a.blur, "blur"),
            (a.bord, "bord"),
            (a.fs, "fs"),
        ] {
            if on {
                flags.push(n);
            }
        }
        let st = |s: &State, i: u8| {
            format!(
                "x{i}={};y{i}={};sx{i}={};sy{i}={};rz{i}={};rx{i}={};ry{i}={};al{i}={};c{i}={};bl{i}={};bo{i}={};fs{i}={}",
                num(s.x),
                num(s.y),
                num(s.scale_x),
                num(s.scale_y),
                num(s.rot_z),
                num(s.rot_x),
                num(s.rot_y),
                s.alpha,
                color_hex(s.color),
                num(s.blur),
                num(s.bord),
                num(s.fs)
            )
        };
        format!(
            "{META}m={};e={};t1={};t2={};fi={};fo={};a={};{};{}",
            if self.mode == Mode::Tags { "tags" } else { "fbf" },
            self.easing.key(),
            self.t1,
            self.t2.map(|v| v.to_string()).unwrap_or_else(|| "end".into()),
            self.fade_in,
            self.fade_out,
            flags.join("+"),
            st(&self.from, 0),
            st(&self.to, 1)
        )
    }

    fn decode(meta: &str) -> Option<Tween> {
        let body = meta.strip_prefix(META)?;
        let mut tw = Tween::default();
        let mut kv = std::collections::HashMap::new();
        for part in body.split(';') {
            if let Some((k, v)) = part.split_once('=') {
                kv.insert(k.trim(), v.trim());
            }
        }
        let f = |k: &str, d: f32| kv.get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
        let i = |k: &str, d: i64| kv.get(k).and_then(|v| v.parse::<i64>().ok()).unwrap_or(d);
        tw.mode = if kv.get("m") == Some(&"fbf") { Mode::FrameByFrame } else { Mode::Tags };
        tw.easing = kv.get("e").map(|e| Easing::from_key(e)).unwrap_or_default();
        tw.t1 = i("t1", 0);
        tw.t2 = kv.get("t2").and_then(|v| v.parse::<i64>().ok());
        tw.fade_in = i("fi", 0);
        tw.fade_out = i("fo", 0);
        for flag in kv.get("a").copied().unwrap_or("").split('+') {
            let a = &mut tw.animate;
            match flag {
                "pos" => a.pos = true,
                "scale" => a.scale = true,
                "rz" => a.rot_z = true,
                "rx" => a.rot_x = true,
                "ry" => a.rot_y = true,
                "alpha" => a.alpha = true,
                "color" => a.color = true,
                "blur" => a.blur = true,
                "bord" => a.bord = true,
                "fs" => a.fs = true,
                _ => {}
            }
        }
        for (s, n) in [(&mut tw.from, '0'), (&mut tw.to, '1')] {
            let d = State::default();
            s.x = f(&format!("x{n}"), d.x);
            s.y = f(&format!("y{n}"), d.y);
            s.scale_x = f(&format!("sx{n}"), d.scale_x);
            s.scale_y = f(&format!("sy{n}"), d.scale_y);
            s.rot_z = f(&format!("rz{n}"), 0.0);
            s.rot_x = f(&format!("rx{n}"), 0.0);
            s.rot_y = f(&format!("ry{n}"), 0.0);
            s.alpha = f(&format!("al{n}"), 0.0) as u8;
            s.color = kv.get(format!("c{n}").as_str()).and_then(|c| parse_hex(c)).unwrap_or(d.color);
            s.blur = f(&format!("bl{n}"), 0.0);
            s.bord = f(&format!("bo{n}"), d.bord);
            s.fs = f(&format!("fs{n}"), d.fs);
        }
        Some(tw)
    }
}

/// The `{sftw:...}` block at the start of a text, if any: (meta content, byte length incl. braces).
fn leading_meta(text: &str) -> Option<(&str, usize)> {
    let rest = text.strip_prefix('{')?;
    let end = rest.find('}')?;
    let inner = &rest[..end];
    inner.starts_with(META).then_some((inner, end + 2))
}

/// Remove the metadata block and (in tags mode) the generated block that follows it.
pub fn strip_generated(text: &str) -> String {
    match leading_meta(text) {
        Some((meta, len)) => {
            let rest = &text[len..];
            if meta.contains("m=tags") && rest.starts_with('{') {
                if let Some(end) = rest.find('}') {
                    return rest[end + 1..].to_string();
                }
            }
            rest.to_string()
        }
        None => text.to_string(),
    }
}

fn effect_id(effect: &str, prefix: &str) -> Option<String> {
    effect.strip_prefix(prefix).map(|s| s.to_string())
}

/// For a generated frame-by-frame line, the index of its source comment.
pub fn find_source(sub: &Subtitle, idx: usize) -> Option<usize> {
    let e = sub.events.get(idx)?;
    if leading_meta(&e.text).is_some() || e.effect.starts_with(EFFECT_SRC) {
        return Some(idx);
    }
    let id = effect_id(&e.effect, EFFECT_GEN)?;
    let want = format!("{EFFECT_SRC}{id}");
    sub.events.iter().position(|x| x.effect == want)
}

/// Read the animation attached to a line (also works on generated frame-by-frame lines).
pub fn read_tween(sub: &Subtitle, idx: usize) -> Option<Tween> {
    let src = find_source(sub, idx)?;
    let (meta, _) = leading_meta(&sub.events[src].text)?;
    Tween::decode(meta)
}

/// Remove an animation, restoring the original line. Returns the restored line's index.
pub fn remove_tween(sub: &mut Subtitle, idx: usize) -> Option<usize> {
    let src = find_source(sub, idx)?;
    let e = &sub.events[src];
    if let Some(id) = effect_id(&e.effect, EFFECT_SRC) {
        let gen = format!("{EFFECT_GEN}{id}");
        let mut i = 0;
        let mut src_idx = src;
        while i < sub.events.len() {
            if sub.events[i].effect == gen {
                sub.events.remove(i);
                if i < src_idx {
                    src_idx -= 1;
                }
            } else {
                i += 1;
            }
        }
        let e = &mut sub.events[src_idx];
        e.comment = false;
        e.effect.clear();
        e.text = strip_generated(&e.text);
        Some(src_idx)
    } else {
        let e = &mut sub.events[src];
        e.text = strip_generated(&e.text);
        Some(src)
    }
}

pub struct Applied {
    /// Index of the line to select afterwards
    pub index: usize,
    /// Number of events produced
    pub events: usize,
    pub warnings: Vec<String>,
}

/// Apply (or re-apply) a tween to the line at `idx`. `fps` is needed for frame-by-frame mode.
pub fn apply_tween(sub: &mut Subtitle, idx: usize, tw: &Tween, fps: f64) -> Result<Applied, String> {
    if idx >= sub.events.len() {
        return Err("no such line".into());
    }
    let idx = remove_tween(sub, idx).unwrap_or(idx);
    let orig = sub.events[idx].clone();
    let dur = orig.duration();
    if dur <= 0 {
        return Err("line has no duration".into());
    }
    let mut warnings = Vec::new();
    let t1 = tw.t1.clamp(0, dur);
    let t2 = tw.t2.unwrap_or(dur).clamp(t1, dur);
    let a = tw.animate;
    let base = remove_tags(&orig.text, &{
        let mut c = a.conflicting();
        if tw.fade_in > 0 || tw.fade_out > 0 {
            c.extend(["fad", "fade"]);
        }
        c
    });
    let meta = format!("{{{}}}", tw.encode());

    match tw.mode {
        Mode::Tags => {
            let mut gen = String::new();
            if a.pos {
                if (tw.from.x, tw.from.y) == (tw.to.x, tw.to.y) {
                    gen += &format!("\\pos({},{})", num(tw.from.x), num(tw.from.y));
                } else {
                    gen += &format!("\\move({},{},{},{},{},{})", num(tw.from.x), num(tw.from.y), num(tw.to.x), num(tw.to.y), t1, t2);
                    if tw.easing.kind != EaseKind::Linear {
                        warnings.push("\\move is always linear in tags mode — use Frame-by-frame for eased movement".into());
                    }
                }
            }
            if tw.fade_in > 0 || tw.fade_out > 0 {
                gen += &format!("\\fad({},{})", tw.fade_in, tw.fade_out);
            }
            let rest = Animate { pos: false, ..a };
            if rest.any() {
                if !tw.easing.tag_compatible() {
                    warnings.push(format!("{} easing can't be expressed with \\t — approximated as cubic; use Frame-by-frame for the exact curve", tw.easing.kind.name()));
                }
                let easing = if tw.easing.tag_compatible() { tw.easing } else { Easing { kind: EaseKind::Cubic, dir: tw.easing.dir } };
                gen += &tw.from.tags(&rest, None);
                for (fs, fe, accel, vf) in easing.segments() {
                    let ta = t1 + ((t2 - t1) as f32 * fs).round() as i64;
                    let tb = t1 + ((t2 - t1) as f32 * fe).round() as i64;
                    let target = tw.from.lerp(&tw.to, vf);
                    let acc = if (accel - 1.0).abs() < 1e-3 { String::new() } else { format!("{},", num(accel)) };
                    gen += &format!("\\t({ta},{tb},{acc}{})", target.tags(&rest, None));
                }
            }
            let e = &mut sub.events[idx];
            e.text = format!("{meta}{{{gen}}}{base}");
            Ok(Applied { index: idx, events: 1, warnings })
        }
        Mode::FrameByFrame => {
            if !(fps > 1.0) {
                return Err("frame-by-frame needs a frame rate (open the video)".into());
            }
            let fm = 1000.0 / fps;
            let k0 = (orig.start as f64 / fm - 1e-6).ceil() as i64;
            let k1 = (orig.end as f64 / fm - 1e-6).ceil() as i64 - 1;
            if k1 < k0 {
                return Err("line is shorter than one frame".into());
            }
            if k1 - k0 > 20_000 {
                return Err("line is too long for frame-by-frame".into());
            }
            let id = format!("{:x}", {
                let mut h: u64 = 0xcbf29ce484222325;
                for b in format!("{:?}{}{}", std::time::SystemTime::now(), orig.start, orig.text).bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                h & 0xffff_ffff
            });
            let fading = tw.fade_in > 0 || tw.fade_out > 0;
            let mut out: Vec<Event> = Vec::new();
            let mut last_tags = String::new();
            for k in k0..=k1 {
                let ft = (k as f64 * fm) as i64;
                let local = (ft - orig.start).max(0);
                let p = if t2 > t1 { (local - t1) as f32 / (t2 - t1) as f32 } else if local >= t2 { 1.0 } else { 0.0 };
                let s = tw.from.lerp(&tw.to, tw.easing.apply(p.clamp(0.0, 1.0)));
                let alpha = if fading {
                    let mut vis = 1.0f32;
                    if tw.fade_in > 0 && local < tw.fade_in {
                        vis = vis.min(local as f32 / tw.fade_in as f32);
                    }
                    if tw.fade_out > 0 && local > dur - tw.fade_out {
                        vis = vis.min((dur - local) as f32 / tw.fade_out as f32);
                    }
                    let base_a = if a.alpha { s.alpha } else { tw.from.alpha };
                    Some((255.0 - (255.0 - base_a as f32) * vis.clamp(0.0, 1.0)).round() as u8)
                } else {
                    None
                };
                let mut tags = String::new();
                if a.pos {
                    tags += &format!("\\pos({},{})", num(s.x), num(s.y));
                }
                tags += &s.tags(&a, alpha);
                let start = if k == k0 { orig.start } else { (k as f64 * fm).round() as i64 };
                let end = if k == k1 { orig.end } else { ((k + 1) as f64 * fm).round() as i64 };
                if tags == last_tags {
                    if let Some(prev) = out.last_mut() {
                        prev.end = end;
                        continue;
                    }
                }
                last_tags = tags.clone();
                let mut e = orig.clone();
                e.start = start;
                e.end = end;
                e.effect = format!("{EFFECT_GEN}{id}");
                e.comment = false;
                e.text = insert_leading_block(&tags, &base);
                out.push(e);
            }
            let n = out.len();
            let src = &mut sub.events[idx];
            src.comment = true;
            src.effect = format!("{EFFECT_SRC}{id}");
            src.text = format!("{meta}{}", src.text);
            for (k, e) in out.into_iter().enumerate() {
                sub.events.insert(idx + 1 + k, e);
            }
            Ok(Applied { index: idx + 1, events: n, warnings })
        }
    }
}

fn insert_leading_block(tags: &str, base: &str) -> String {
    if tags.is_empty() {
        base.to_string()
    } else {
        // separate block so the base text's own leading block stays untouched
        let _ = insert_leading;
        format!("{{{tags}}}{base}")
    }
}

/// Ready-made animations. `here` is the line's resting state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    SlideFromLeft,
    SlideFromRight,
    SlideFromTop,
    SlideFromBottom,
    PopIn,
    ZoomOut,
    FadeRise,
    SpinIn,
    Drop,
    Pulse,
}

impl Preset {
    pub const ALL: [Preset; 10] = [
        Preset::SlideFromLeft,
        Preset::SlideFromRight,
        Preset::SlideFromTop,
        Preset::SlideFromBottom,
        Preset::PopIn,
        Preset::ZoomOut,
        Preset::FadeRise,
        Preset::SpinIn,
        Preset::Drop,
        Preset::Pulse,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Preset::SlideFromLeft => "Slide in ←",
            Preset::SlideFromRight => "Slide in →",
            Preset::SlideFromTop => "Slide in ↓",
            Preset::SlideFromBottom => "Slide in ↑",
            Preset::PopIn => "Pop in",
            Preset::ZoomOut => "Zoom out",
            Preset::FadeRise => "Fade + rise",
            Preset::SpinIn => "Spin in",
            Preset::Drop => "Drop & bounce",
            Preset::Pulse => "Pulse",
        }
    }
    pub fn build(self, here: State, play_res: (i32, i32), dur: i64) -> Tween {
        let (w, h) = (play_res.0 as f32, play_res.1 as f32);
        let win = (dur / 3).clamp(150, 600);
        let mut t = Tween { from: here, to: here, t1: 0, t2: Some(win), mode: Mode::FrameByFrame, ..Default::default() };
        match self {
            Preset::SlideFromLeft | Preset::SlideFromRight | Preset::SlideFromTop | Preset::SlideFromBottom => {
                t.animate.pos = true;
                t.animate.alpha = true;
                t.from.alpha = 255;
                match self {
                    Preset::SlideFromLeft => t.from.x -= w * 0.15,
                    Preset::SlideFromRight => t.from.x += w * 0.15,
                    Preset::SlideFromTop => t.from.y -= h * 0.12,
                    _ => t.from.y += h * 0.12,
                }
                t.easing = Easing::new(EaseKind::Cubic, EaseDir::Out);
            }
            Preset::PopIn => {
                t.animate.scale = true;
                t.from.scale_x = 0.0;
                t.from.scale_y = 0.0;
                t.easing = Easing::new(EaseKind::Back, EaseDir::Out);
            }
            Preset::ZoomOut => {
                t.animate.scale = true;
                t.animate.alpha = true;
                t.from.scale_x = here.scale_x * 1.6;
                t.from.scale_y = here.scale_y * 1.6;
                t.from.alpha = 255;
                t.easing = Easing::new(EaseKind::Expo, EaseDir::Out);
                t.mode = Mode::Tags;
            }
            Preset::FadeRise => {
                t.animate.pos = true;
                t.animate.alpha = true;
                t.from.y += h * 0.04;
                t.from.alpha = 255;
                t.easing = Easing::new(EaseKind::Quad, EaseDir::Out);
            }
            Preset::SpinIn => {
                t.animate.rot_z = true;
                t.animate.scale = true;
                t.from.rot_z = here.rot_z + 360.0;
                t.from.scale_x = 0.0;
                t.from.scale_y = 0.0;
                t.easing = Easing::new(EaseKind::Cubic, EaseDir::Out);
                t.mode = Mode::Tags;
            }
            Preset::Drop => {
                t.animate.pos = true;
                t.from.y -= h * 0.2;
                t.t2 = Some((dur / 2).clamp(300, 900));
                t.easing = Easing::new(EaseKind::Bounce, EaseDir::Out);
            }
            Preset::Pulse => {
                t.animate.scale = true;
                t.to.scale_x = here.scale_x * 1.15;
                t.to.scale_y = here.scale_y * 1.15;
                t.t2 = None;
                t.easing = Easing::new(EaseKind::Sine, EaseDir::InOut);
                t.mode = Mode::Tags;
            }
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn easing_curves() {
        for k in EaseKind::ALL {
            for d in [EaseDir::In, EaseDir::Out, EaseDir::InOut] {
                let e = Easing::new(k, d);
                assert!((e.apply(0.0)).abs() < 1e-4, "{k:?} {d:?}");
                assert!((e.apply(1.0) - 1.0).abs() < 1e-4, "{k:?} {d:?}");
            }
        }
        assert!(Easing::new(EaseKind::Back, EaseDir::Out).apply(0.6) > 1.0); // overshoot
    }
    #[test]
    fn tags_mode() {
        let mut s = Subtitle::new();
        s.events.push(Event::new(1000, 3000, "{\\b1\\fscx150}Hello"));
        let mut tw = Tween::default();
        tw.animate.pos = true;
        tw.animate.scale = true;
        tw.from = State::of_event(&s, &s.events[0]);
        tw.to = tw.from;
        tw.to.x += 100.0;
        tw.to.scale_x = 200.0;
        tw.t2 = Some(1000);
        tw.easing = Easing::new(EaseKind::Quad, EaseDir::InOut);
        assert_eq!(tw.from.scale_x, 150.0);
        let r = apply_tween(&mut s, 0, &tw, 23.976).unwrap();
        let t = &s.events[0].text;
        assert!(t.starts_with("{sftw:"), "{t}");
        assert!(t.contains("\\move(960,"), "{t}");
        assert!(t.contains("\\t(0,500,2,\\fscx175"), "{t}");
        assert!(t.contains("\\t(500,1000,0.5,\\fscx200"), "{t}");
        assert!(t.ends_with("{\\b1}Hello"), "{t}");
        assert_eq!(r.warnings.len(), 1); // eased \move
        assert_eq!(read_tween(&s, 0).unwrap().to.scale_x, 200.0);
        assert_eq!(s.events[0].plain(), "Hello");
        remove_tween(&mut s, 0);
        assert_eq!(s.events[0].text, "{\\b1}Hello");
    }
    #[test]
    fn fbf_mode() {
        let mut s = Subtitle::new();
        s.events.push(Event::new(0, 1000, "Hi"));
        s.events.push(Event::new(2000, 3000, "next"));
        let mut tw = Tween { mode: Mode::FrameByFrame, ..Default::default() };
        tw.animate.pos = true;
        tw.from = State::of_event(&s, &s.events[0]);
        tw.to = tw.from;
        tw.to.y -= 100.0;
        tw.t2 = Some(500);
        tw.easing = Easing::new(EaseKind::Bounce, EaseDir::Out);
        let r = apply_tween(&mut s, 0, &tw, 25.0).unwrap();
        // 500ms of motion at 25fps = ~13 distinct frames, then one merged static tail
        assert!(r.events > 10 && r.events < 16, "{}", r.events);
        assert!(s.events[0].comment);
        let gens: Vec<_> = s.events.iter().filter(|e| e.effect.starts_with(EFFECT_GEN)).collect();
        assert_eq!(gens.first().unwrap().start, 0);
        assert_eq!(gens.last().unwrap().end, 1000);
        for w in gens.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
        // re-read & remove from a generated line
        assert_eq!(read_tween(&s, 3).unwrap().easing.kind, EaseKind::Bounce);
        let i = remove_tween(&mut s, 3).unwrap();
        assert_eq!(i, 0);
        assert_eq!(s.events.len(), 2);
        assert_eq!(s.events[0].text, "Hi");
        assert!(!s.events[0].comment);
    }
}
