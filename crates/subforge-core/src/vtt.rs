use crate::model::{Event, Subtitle};
use crate::text::{alignment_override, ass_to_html, html_to_ass, HtmlOpts};
use crate::time::{fmt_vtt, parse_clock};

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", "\u{a0}")
        .replace("&lrm;", "\u{200e}")
        .replace("&rlm;", "\u{200f}")
        .replace("&amp;", "&")
}

pub fn parse(content: &str) -> Subtitle {
    let content = content.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let mut sub = Subtitle::new();
    for block in content.split("\n\n") {
        let lines: Vec<&str> = block.split('\n').filter(|l| !l.is_empty()).collect();
        if lines.is_empty() {
            continue;
        }
        let first = lines[0].trim_start();
        if first.starts_with("WEBVTT") || first.starts_with("NOTE") || first.starts_with("STYLE") || first.starts_with("REGION") {
            // header may be immediately followed by a cue only when separated by blank line; skip
            continue;
        }
        let Some(ti) = lines.iter().position(|l| l.contains("-->")) else { continue };
        let (a, b) = lines[ti].split_once("-->").unwrap();
        let mut rest = b.trim().split_whitespace();
        let (Some(start), Some(end)) = (parse_clock(a.trim()), rest.next().and_then(parse_clock)) else { continue };
        let settings: Vec<&str> = rest.collect();
        let raw_text = lines[ti + 1..].join("\n");
        // voice span -> actor
        let mut actor = String::new();
        if let Some(i) = raw_text.find("<v") {
            let after = &raw_text[i + 2..];
            if let Some(j) = after.find('>') {
                let inner = after[..j].trim_start_matches(|c| c == '.' || c == ' ');
                let name = inner.split_once(' ').map(|x| x.1).unwrap_or(inner);
                if after.starts_with(' ') || after.starts_with('.') {
                    actor = name.trim().to_string();
                }
            }
        }
        let mut text = unescape(&html_to_ass(&raw_text));
        let top = settings.iter().any(|s| {
            s.strip_prefix("line:")
                .map(|v| {
                    let v = v.split(',').next().unwrap_or("");
                    if let Some(p) = v.strip_suffix('%') {
                        p.parse::<f32>().map(|x| x < 40.0).unwrap_or(false)
                    } else {
                        v.parse::<i32>().map(|x| x >= 0 && x < 4).unwrap_or(false)
                    }
                })
                .unwrap_or(false)
        });
        if top && !text.contains("\\an") {
            text = format!("{{\\an8}}{}", text);
        }
        let mut e = Event::new(start, end, text);
        e.actor = actor;
        sub.events.push(e);
    }
    sub
}

pub fn write(sub: &Subtitle) -> String {
    let mut out = String::from("WEBVTT\n\n");
    let opts = HtmlOpts { keep_formatting: true, keep_colors: false, keep_an: false };
    for e in sub.events.iter().filter(|e| !e.comment) {
        let mut settings = String::new();
        match alignment_override(&e.text).unwrap_or(2) {
            7..=9 => settings.push_str(" line:10%"),
            4..=6 => settings.push_str(" line:50%"),
            _ => {}
        }
        let mut text = ass_to_html(&e.text, opts);
        if !e.actor.is_empty() {
            text = format!("<v {}>{}", e.actor, text);
        }
        out.push_str(&format!("{} --> {}{}\n{}\n\n", fmt_vtt(e.start), fmt_vtt(e.end), settings, text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vtt() {
        let s = "WEBVTT\n\nNOTE x\n\n1\n00:01.000 --> 00:02.000 line:0\n<v Bob>Hi &amp; bye\n\n00:00:03.000 --> 00:00:04.000\n<i>yo</i>\n";
        let sub = parse(s);
        assert_eq!(sub.events.len(), 2);
        assert_eq!(sub.events[0].actor, "Bob");
        assert_eq!(sub.events[0].text, "{\\an8}Hi & bye");
        assert_eq!(sub.events[1].text, "{\\i1}yo{\\i0}");
        let w = write(&sub);
        assert!(w.contains("00:00:01.000 --> 00:00:02.000 line:10%\n<v Bob>Hi & bye"));
    }
}
