//! Advanced SubStation Alpha (v4.00+) and SubStation Alpha (v4.00) reader/writer.

use crate::model::{Color, Event, Style, Subtitle};
use crate::text::{legacy_to_numpad, numpad_to_legacy};
use crate::time::{fmt_ass, parse_clock};

const ASS_STYLE_FORMAT: &str = "Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding";
const SSA_STYLE_FORMAT: &str = "Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, TertiaryColour, BackColour, Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, AlphaLevel, Encoding";
const ASS_EVENT_FORMAT: &str = "Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text";
const SSA_EVENT_FORMAT: &str = "Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text";

fn split_format(s: &str) -> Vec<String> {
    s.split(',').map(|f| f.trim().to_ascii_lowercase()).collect()
}

fn parse_bool(s: &str) -> bool {
    s.trim().parse::<i32>().map(|v| v != 0).unwrap_or(false)
}
fn parse_f(s: &str, d: f32) -> f32 {
    s.trim().parse().unwrap_or(d)
}
fn parse_i(s: &str, d: i32) -> i32 {
    s.trim().parse::<f64>().map(|v| v as i32).unwrap_or(d)
}

pub fn fmt_num(v: f32) -> String {
    if (v - v.round()).abs() < 1e-4 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{:.3}", v);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Section {
    None,
    Info,
    Styles { legacy: bool },
    Events,
    Other,
}

/// Parse ASS or SSA content.
pub fn parse(content: &str) -> Subtitle {
    let content = content.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let mut sub = Subtitle::default();
    let mut section = Section::None;
    let mut style_fmt: Option<Vec<String>> = None;
    let mut event_fmt: Option<Vec<String>> = None;
    let mut legacy_doc = false;

    for raw in content.split('\n') {
        // keep trailing spaces: they are significant in Dialogue text (and in [Fonts] data)
        let line = raw;
        let t = line.trim_start();
        let th = t.trim_end();
        if th.starts_with('[') && th.ends_with(']') {
            let name = th[1..th.len() - 1].trim().to_string();
            let lower = name.to_ascii_lowercase();
            section = match lower.as_str() {
                "script info" => Section::Info,
                "v4+ styles" | "v4 styles+" => Section::Styles { legacy: false },
                "v4 styles" => {
                    legacy_doc = true;
                    Section::Styles { legacy: true }
                }
                "events" => Section::Events,
                _ => {
                    sub.extra_sections.push((name, Vec::new()));
                    Section::Other
                }
            };
            continue;
        }
        match section {
            Section::None => {}
            Section::Other => {
                if let Some(last) = sub.extra_sections.last_mut() {
                    last.1.push(line.to_string());
                }
            }
            Section::Info => {
                if t.is_empty() {
                    continue;
                }
                let t = t.trim_end();
                if let Some(c) = t.strip_prefix(';') {
                    sub.info.push((";".into(), c.trim().to_string()));
                } else if let Some((k, v)) = t.split_once(':') {
                    sub.info.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            Section::Styles { legacy } => {
                let t = t.trim_end();
                if let Some(f) = t.strip_prefix("Format:") {
                    style_fmt = Some(split_format(f));
                } else if let Some(v) = t.strip_prefix("Style:") {
                    let fmt = style_fmt.clone().unwrap_or_else(|| split_format(if legacy { SSA_STYLE_FORMAT } else { ASS_STYLE_FORMAT }));
                    sub.styles.push(parse_style(v, &fmt, legacy));
                }
            }
            Section::Events => {
                if let Some(f) = t.strip_prefix("Format:") {
                    event_fmt = Some(split_format(f.trim_end()));
                    continue;
                }
                let (comment, rest) = if let Some(r) = t.strip_prefix("Dialogue:") {
                    (false, r)
                } else if let Some(r) = t.strip_prefix("Comment:") {
                    (true, r)
                } else {
                    continue;
                };
                let fmt = event_fmt.clone().unwrap_or_else(|| split_format(if legacy_doc { SSA_EVENT_FORMAT } else { ASS_EVENT_FORMAT }));
                if let Some(mut e) = parse_event(rest, &fmt) {
                    e.comment = comment;
                    sub.events.push(e);
                }
            }
        }
    }
    // trim trailing blank lines inside extra sections
    for (_, lines) in sub.extra_sections.iter_mut() {
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
    }
    if sub.styles.is_empty() {
        sub.styles.push(Style::default());
    }
    sub
}

fn parse_style(v: &str, fmt: &[String], legacy: bool) -> Style {
    let vals: Vec<&str> = v.splitn(fmt.len().max(1), ',').collect();
    let mut s = Style::default();
    let mut alpha_level: Option<u8> = None;
    for (k, val) in fmt.iter().zip(vals.iter()) {
        let val = val.trim();
        match k.as_str() {
            "name" => s.name = val.to_string(),
            "fontname" => s.fontname = val.to_string(),
            "fontsize" => s.fontsize = parse_f(val, s.fontsize),
            "primarycolour" => s.primary = Color::parse_ass(val).unwrap_or(s.primary),
            "secondarycolour" => s.secondary = Color::parse_ass(val).unwrap_or(s.secondary),
            "outlinecolour" | "tertiarycolour" => s.outline = Color::parse_ass(val).unwrap_or(s.outline),
            "backcolour" => s.back = Color::parse_ass(val).unwrap_or(s.back),
            "bold" => s.bold = parse_bool(val),
            "italic" => s.italic = parse_bool(val),
            "underline" => s.underline = parse_bool(val),
            "strikeout" => s.strikeout = parse_bool(val),
            "scalex" => s.scale_x = parse_f(val, 100.0),
            "scaley" => s.scale_y = parse_f(val, 100.0),
            "spacing" => s.spacing = parse_f(val, 0.0),
            "angle" => s.angle = parse_f(val, 0.0),
            "borderstyle" => s.border_style = parse_i(val, 1),
            "outline" => s.outline_w = parse_f(val, 2.0),
            "shadow" => s.shadow = parse_f(val, 2.0),
            "alignment" => {
                let a = parse_i(val, 2);
                s.alignment = if legacy { legacy_to_numpad(a) } else { a.clamp(1, 9) };
            }
            "marginl" => s.margin_l = parse_i(val, 10),
            "marginr" => s.margin_r = parse_i(val, 10),
            "marginv" => s.margin_v = parse_i(val, 10),
            "alphalevel" => alpha_level = val.parse().ok(),
            "encoding" => s.encoding = parse_i(val, 1),
            _ => {}
        }
    }
    if let Some(a) = alpha_level {
        if a > 0 {
            s.secondary.a = a;
        }
    }
    s
}

fn parse_event(v: &str, fmt: &[String]) -> Option<Event> {
    let vals: Vec<&str> = v.splitn(fmt.len().max(1), ',').collect();
    let mut e = Event::default();
    let mut have_time = (false, false);
    for (k, val) in fmt.iter().zip(vals.iter()) {
        match k.as_str() {
            "layer" => e.layer = parse_i(val, 0),
            "marked" => {}
            "start" => {
                e.start = parse_clock(val.trim())?;
                have_time.0 = true;
            }
            "end" => {
                e.end = parse_clock(val.trim())?;
                have_time.1 = true;
            }
            "style" => e.style = val.trim().trim_start_matches('*').to_string(),
            "name" | "actor" => e.actor = val.trim().to_string(),
            "marginl" => e.margin_l = parse_i(val, 0),
            "marginr" => e.margin_r = parse_i(val, 0),
            "marginv" => e.margin_v = parse_i(val, 0),
            "effect" => e.effect = val.trim().to_string(),
            "text" => e.text = val.to_string(),
            _ => {}
        }
    }
    if have_time.0 && have_time.1 {
        Some(e)
    } else {
        None
    }
}

fn write_info(sub: &Subtitle, out: &mut String, script_type: &str) {
    out.push_str("[Script Info]\n");
    let has_generated = sub.info.iter().any(|(k, v)| k == ";" && v.contains("SubForge"));
    if !has_generated {
        out.push_str("; Script generated by SubForge\n");
    }
    let mut wrote_type = false;
    for (k, v) in &sub.info {
        if k == ";" {
            out.push_str(&format!("; {}\n", v));
        } else if k.eq_ignore_ascii_case("ScriptType") {
            out.push_str(&format!("ScriptType: {}\n", script_type));
            wrote_type = true;
        } else {
            out.push_str(&format!("{}: {}\n", k, v));
        }
    }
    if !wrote_type {
        out.push_str(&format!("ScriptType: {}\n", script_type));
    }
    out.push('\n');
}

fn sanitize(s: &str) -> String {
    s.replace("\r\n", "\\N").replace('\n', "\\N").replace('\r', "")
}

fn write_extra(sub: &Subtitle, out: &mut String) {
    for (name, lines) in &sub.extra_sections {
        out.push_str(&format!("\n[{}]\n", name));
        for l in lines {
            out.push_str(l);
            out.push('\n');
        }
    }
}

pub fn write_ass(sub: &Subtitle) -> String {
    let mut sub = sub.clone();
    sub.ensure_defaults();
    let mut out = String::new();
    write_info(&sub, &mut out, "v4.00+");
    out.push_str("[V4+ Styles]\n");
    out.push_str(&format!("Format: {}\n", ASS_STYLE_FORMAT));
    for s in &sub.styles {
        out.push_str(&format!(
            "Style: {},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            s.name.replace(',', ";"),
            s.fontname.replace(',', ";"),
            fmt_num(s.fontsize),
            s.primary.to_ass(),
            s.secondary.to_ass(),
            s.outline.to_ass(),
            s.back.to_ass(),
            if s.bold { -1 } else { 0 },
            if s.italic { -1 } else { 0 },
            if s.underline { -1 } else { 0 },
            if s.strikeout { -1 } else { 0 },
            fmt_num(s.scale_x),
            fmt_num(s.scale_y),
            fmt_num(s.spacing),
            fmt_num(s.angle),
            s.border_style,
            fmt_num(s.outline_w),
            fmt_num(s.shadow),
            s.alignment,
            s.margin_l,
            s.margin_r,
            s.margin_v,
            s.encoding
        ));
    }
    out.push_str("\n[Events]\n");
    out.push_str(&format!("Format: {}\n", ASS_EVENT_FORMAT));
    for e in &sub.events {
        out.push_str(&format!(
            "{}: {},{},{},{},{},{},{},{},{},{}\n",
            if e.comment { "Comment" } else { "Dialogue" },
            e.layer,
            fmt_ass(e.start),
            fmt_ass(e.end),
            e.style.replace(',', ";"),
            e.actor.replace(',', ";"),
            e.margin_l,
            e.margin_r,
            e.margin_v,
            e.effect.replace(',', ";"),
            sanitize(&e.text)
        ));
    }
    write_extra(&sub, &mut out);
    out
}

pub fn write_ssa(sub: &Subtitle) -> String {
    let mut sub = sub.clone();
    sub.ensure_defaults();
    let mut out = String::new();
    write_info(&sub, &mut out, "v4.00");
    out.push_str("[V4 Styles]\n");
    out.push_str(&format!("Format: {}\n", SSA_STYLE_FORMAT));
    for s in &sub.styles {
        out.push_str(&format!(
            "Style: {},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},0,{}\n",
            s.name.replace(',', ";"),
            s.fontname.replace(',', ";"),
            fmt_num(s.fontsize),
            s.primary.to_ssa(),
            s.secondary.to_ssa(),
            s.outline.to_ssa(),
            s.back.to_ssa(),
            if s.bold { -1 } else { 0 },
            if s.italic { -1 } else { 0 },
            s.border_style,
            fmt_num(s.outline_w),
            fmt_num(s.shadow),
            numpad_to_legacy(s.alignment),
            s.margin_l,
            s.margin_r,
            s.margin_v,
            s.encoding
        ));
    }
    out.push_str("\n[Events]\n");
    out.push_str(&format!("Format: {}\n", SSA_EVENT_FORMAT));
    for e in &sub.events {
        out.push_str(&format!(
            "{}: Marked=0,{},{},{},{},{:04},{:04},{:04},{},{}\n",
            if e.comment { "Comment" } else { "Dialogue" },
            fmt_ass(e.start),
            fmt_ass(e.end),
            e.style.replace(',', ";"),
            e.actor.replace(',', ";"),
            e.margin_l,
            e.margin_r,
            e.margin_v,
            e.effect.replace(',', ";"),
            sanitize(&e.text)
        ));
    }
    write_extra(&sub, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    const SAMPLE: &str = "[Script Info]\n; hi\nTitle: Test\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,60,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,-1,0,0,0,100,100,0,0,1,3,1,2,20,20,40,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:03.50,Default,Bob,0,0,0,,Hello, world\\Nline2\nComment: 0,0:00:04.00,0:00:05.00,Default,,0,0,0,,note\n\n[Aegisub Project Garbage]\nVideo File: ep01.mkv\n";

    #[test]
    fn roundtrip() {
        let s = parse(SAMPLE);
        assert_eq!(s.events.len(), 2);
        assert_eq!(s.events[0].text, "Hello, world\\Nline2");
        assert_eq!(s.events[0].actor, "Bob");
        assert!(s.events[1].comment);
        assert!(s.styles[0].bold);
        assert_eq!(s.styles[0].back.a, 0x80);
        let out = write_ass(&s);
        let s2 = parse(&out);
        assert_eq!(s.events, s2.events);
        assert_eq!(s.styles, s2.styles);
        assert!(out.contains("[Aegisub Project Garbage]\nVideo File: ep01.mkv"));
    }

    #[test]
    fn typeset_roundtrip() {
        // heavy typesetting: drawings, vector clips, nested \t, karaoke, trailing spaces,
        // comment lines, layers, custom Format order, embedded fonts and unknown sections
        let src = "\u{feff}[Script Info]\r\n; Script generated by Aegisub 3.2.2\r\nTitle: TS test\r\nScriptType: v4.00+\r\nWrapStyle: 0\r\nScaledBorderAndShadow: yes\r\nYCbCr Matrix: TV.709\r\nPlayResX: 1920\r\nPlayResY: 1080\r\n\r\n[Aegisub Project Garbage]\r\nVideo File: ep01.mkv\r\n\r\n[V4+ Styles]\r\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\r\nStyle: Sign,Noto Sans Thai,62.5,&H00FFFFFF,&H000000FF,&H40102030,&HA0000000,-1,0,0,0,95.5,100,1.5,-3,1,2.25,0.5,7,10,20,30,222\r\n\r\n[Events]\r\nFormat: Layer, Start, End, Style, Actor, MarginL, MarginR, MarginV, Effect, Text\r\nDialogue: 5,0:01:02.03,0:01:04.56,Sign,TS,0,0,0,,{\\an7\\pos(812.5,96)\\clip(m 10 10 l 200 10 200 80 10 80)\\t(0,300,0.5,\\fscx120\\t(\\blur3))\\p1}m 0 0 b 10 0 20 10 20 20 l 0 20{\\p0}\r\nDialogue: 0,0:01:05.00,0:01:06.00,Sign,,0,0,0,Banner;5;0,{\\k20}ka{\\kf35}ra{\\ko10}o{\\K5}ke  \r\nComment: 1,0:00:00.00,0:00:00.00,Sign,,0,0,0,template line,{\\fad(100,100)}!retime(\"line\",0,0)!\r\n\r\n[Fonts]\r\nfontname: Test_0.ttf\r\nM)#3&*!H5E#\r\n0@\r\n";
        let a = parse(src);
        assert_eq!(a.events.len(), 3);
        assert!(a.events[1].text.ends_with("ke  "), "trailing spaces lost: {:?}", a.events[1].text);
        assert_eq!(a.events[1].effect, "Banner;5;0");
        assert_eq!(a.events[0].layer, 5);
        assert_eq!(a.styles[0].encoding, 222);
        assert_eq!(a.styles[0].outline.a, 0x40);
        let out = write_ass(&a);
        let b = parse(&out);
        assert_eq!(a.events, b.events);
        assert_eq!(a.styles, b.styles);
        assert_eq!(a.extra_sections, b.extra_sections);
        assert!(out.contains("[Fonts]\nfontname: Test_0.ttf\nM)#3&*!H5E#\n0@\n"), "{out}");
        assert!(out.contains("YCbCr Matrix: TV.709"));
        // byte-identical event lines (after CRLF normalisation)
        for l in src.replace("\r\n", "\n").lines().filter(|l| l.starts_with("Dialogue:") || l.starts_with("Comment:")) {
            assert!(out.contains(l), "line changed: {l}");
        }
    }

    #[test]
    fn ssa_roundtrip() {
        let s = parse(SAMPLE);
        let ssa = write_ssa(&s);
        assert!(ssa.contains("[V4 Styles]"));
        let s2 = parse(&ssa);
        assert_eq!(s2.styles[0].alignment, 2);
        assert_eq!(s2.styles[0].primary, s.styles[0].primary);
        assert_eq!(s2.events[0].text, s.events[0].text);
    }
}
