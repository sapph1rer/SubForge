use crate::model::{Event, Subtitle};
use crate::text::{ass_to_html, html_to_ass, HtmlOpts};
use crate::time::{fmt_srt, parse_clock};

fn timing_line(line: &str) -> Option<(i64, i64)> {
    let (a, b) = line.split_once("-->")?;
    let start = parse_clock(a.trim())?;
    // drop trailing coordinates / VTT settings
    let b = b.trim().split_whitespace().next()?;
    let end = parse_clock(b)?;
    Some((start, end))
}

/// Parse SubRip. Tolerant of missing indexes, extra blank lines, `.` instead of `,`.
pub fn parse(content: &str) -> Subtitle {
    let content = content.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = content.split('\n').collect();
    let timings: Vec<(usize, (i64, i64))> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| timing_line(l).map(|t| (i, t)))
        .collect();

    let mut sub = Subtitle::new();
    for (k, &(i, (start, end))) in timings.iter().enumerate() {
        let mut j = timings.get(k + 1).map(|t| t.0).unwrap_or(lines.len());
        // the line right before the next timing is usually its index number
        if k + 1 < timings.len() && j > i + 1 && lines[j - 1].trim().chars().all(|c| c.is_ascii_digit()) && !lines[j - 1].trim().is_empty() {
            j -= 1;
        }
        let mut body: Vec<&str> = lines[i + 1..j].to_vec();
        while body.last().is_some_and(|l| l.trim().is_empty()) {
            body.pop();
        }
        while body.first().is_some_and(|l| l.trim().is_empty()) {
            body.remove(0);
        }
        let text = html_to_ass(&body.join("\n"));
        sub.events.push(Event::new(start, end, text));
    }
    sub
}

pub fn write(sub: &Subtitle, opts: HtmlOpts) -> String {
    let mut out = String::new();
    let mut n = 0;
    for e in sub.events.iter().filter(|e| !e.comment) {
        n += 1;
        let text = ass_to_html(&e.text, opts);
        out.push_str(&format!("{}\n{} --> {}\n{}\n\n", n, fmt_srt(e.start), fmt_srt(e.end), text.trim_end_matches('\n')));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_basic() {
        let s = "1\n00:00:01,000 --> 00:00:02,500\nHello\n<i>World</i>\n\n2\n00:00:03,000 --> 00:00:04,000\n\n3\n00:00:05.000 --> 00:00:06.000 X1:0\n42\n";
        let sub = parse(s);
        assert_eq!(sub.events.len(), 3);
        assert_eq!(sub.events[0].text, "Hello\\N{\\i1}World{\\i0}");
        assert_eq!(sub.events[1].text, "");
        assert_eq!(sub.events[2].text, "42");
        assert_eq!(sub.events[2].start, 5000);
        let w = write(&sub, HtmlOpts::default());
        assert!(w.contains("00:00:01,000 --> 00:00:02,500\nHello\n<i>World</i>"));
    }
}
