//! Visual typesetting tools on the video: rotate (`\frz`), scale (`\fscx`/`\fscy`) and clip
//! editing (`\clip` / `\iclip`, vector polygon or rectangle).
//!
//! Every tool edits all selected lines and records one undo step per drag.

use crate::app::App;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Response, Stroke, Vec2};
use subforge_core::tags;

fn fmt(v: f32) -> String {
    subforge_core::ass::fmt_num((v * 100.0).round() / 100.0)
}

/// A parsed clip.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipShape {
    Rect { x1: f32, y1: f32, x2: f32, y2: f32 },
    /// Straight-line polygon (`m x y l x y ...`)
    Polygon(Vec<(f32, f32)>),
    /// Curves or other drawing commands: shown, but not editable point by point
    Complex(Vec<(f32, f32)>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub inverse: bool,
    pub shape: ClipShape,
}

impl Clip {
    pub fn parse(text: &str) -> Option<Clip> {
        let (inverse, arg) = match tags::get_tag(text, "clip") {
            Some(a) => (false, a),
            None => (true, tags::get_tag(text, "iclip")?),
        };
        let inner = arg.trim().trim_start_matches('(').trim_end_matches(')').trim().to_string();
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() == 4 && parts.iter().all(|p| p.parse::<f32>().is_ok()) {
            let v: Vec<f32> = parts.iter().map(|p| p.parse().unwrap()).collect();
            return Some(Clip { inverse, shape: ClipShape::Rect { x1: v[0], y1: v[1], x2: v[2], y2: v[3] } });
        }
        // optional scale argument: "(2, m 0 0 l ...)"
        let (scale, draw) = match inner.split_once(',') {
            Some((a, b)) if a.trim().parse::<i32>().is_ok() => (a.trim().parse::<i32>().unwrap_or(1).max(1), b),
            _ => (1, inner.as_str()),
        };
        let div = 2f32.powi(scale - 1);
        let mut pts = Vec::new();
        let mut simple = true; // scaled clips are converted to script coordinates
        let mut nums: Vec<f32> = Vec::new();
        for tok in draw.split_whitespace() {
            match tok {
                "m" | "l" | "n" => {}
                t if t.parse::<f32>().is_ok() => nums.push(t.parse::<f32>().unwrap() / div),
                _ => simple = false, // b, s, p, c ...
            }
        }
        for c in nums.chunks_exact(2) {
            pts.push((c[0], c[1]));
        }
        // a polygon has exactly one "m"
        if draw.split_whitespace().filter(|t| *t == "m").count() > 1 {
            simple = false;
        }
        Some(Clip { inverse, shape: if simple { ClipShape::Polygon(pts) } else { ClipShape::Complex(pts) } })
    }

    pub fn to_tag(&self) -> (String, String) {
        let name = if self.inverse { "iclip" } else { "clip" };
        let arg = match &self.shape {
            ClipShape::Rect { x1, y1, x2, y2 } => {
                format!("({},{},{},{})", fmt(x1.min(*x2)), fmt(y1.min(*y2)), fmt(x1.max(*x2)), fmt(y1.max(*y2)))
            }
            ClipShape::Polygon(p) | ClipShape::Complex(p) => {
                let mut s = String::from("(m");
                for (i, (x, y)) in p.iter().enumerate() {
                    if i == 1 {
                        s.push_str(" l");
                    }
                    s += &format!(" {} {}", x.round() as i64, y.round() as i64);
                }
                s.push(')');
                s
            }
        };
        (name.to_string(), arg)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipMode {
    #[default]
    Vector,
    Rect,
}

#[derive(Debug, Clone)]
enum Drag {
    /// rotate/scale: anchor (screen), press point (screen), per-line base values (idx, frz, fscx, fscy)
    Transform { anchor: Pos2, start: Pos2, bases: Vec<(usize, f32, f32, f32)> },
    Vertex(usize),
    RectNew(Pos2),
    RectCorner(u8),
    RectMove { start: (f32, f32), orig: (f32, f32, f32, f32) },
}

#[derive(Default)]
pub struct TypesetState {
    pub clip_mode: ClipMode,
    drag: Option<Drag>,
}

fn tag_f(text: &str, name: &str) -> Option<f32> {
    tags::get_tag(text, name).and_then(|a| a.trim().parse::<f32>().ok())
}

impl App {
    fn ts_anchor(&self, i: usize, t: i64) -> Option<(f32, f32)> {
        let e = self.doc.sub.events.get(i)?;
        if let Some(org) = tags::get_tag(&e.text, "org") {
            let v: Vec<f32> = org.trim_matches(|c| c == '(' || c == ')').split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() == 2 {
                return Some((v[0], v[1]));
            }
        }
        Some(tags::position_at(&self.doc.sub, e, t.clamp(e.start, (e.end - 1).max(e.start))))
    }

    /// Write the same clip to every selected line.
    fn write_clip(&mut self, clip: &Clip) {
        let (name, arg) = clip.to_tag();
        if !self.text_session {
            self.checkpoint();
            self.text_session = true;
        }
        for i in self.selected() {
            if let Some(e) = self.doc.sub.events.get_mut(i) {
                let t = tags::remove_tags(&e.text, &["clip", "iclip"]);
                e.text = tags::set_tag(&t, &name, &arg);
            }
        }
        self.touch();
        self.buf_for = None;
    }

    pub fn clear_clip(&mut self) {
        let v = self.selected();
        self.modify(|s| {
            for i in v {
                let e = &mut s.events[i];
                e.text = tags::remove_tags(&e.text, &["clip", "iclip"]);
            }
        });
        self.buf_for = None;
    }

    pub fn toggle_clip_inverse(&mut self) {
        let Some(c) = self.cur else { return };
        let Some(mut clip) = Clip::parse(&self.doc.sub.events[c].text) else { return };
        clip.inverse = !clip.inverse;
        self.write_clip(&clip);
        self.end_text_session();
    }

    /// Rotate / Scale tool: drag around the line's anchor (or `\org`).
    pub fn transform_tool(&mut self, ui: &egui::Ui, resp: &Response, painter: &egui::Painter, vrect: Rect, t: i64, rotate: bool) {
        let (pw, ph) = self.doc.sub.play_res();
        let (sx, sy) = (vrect.width() / pw.max(1) as f32, vrect.height() / ph.max(1) as f32);
        let to_screen = |x: f32, y: f32| Pos2::new(vrect.left() + x * sx, vrect.top() + y * sy);
        let Some(c) = self.cur else { return };
        let Some((ax, ay)) = self.ts_anchor(c, t) else { return };
        let anchor = to_screen(ax, ay);
        let accent = Color32::from_rgb(255, 170, 70);

        // gizmo
        if rotate {
            painter.circle_stroke(anchor, 70.0, Stroke::new(1.5, accent.gamma_multiply(0.8)));
        } else {
            painter.rect_stroke(Rect::from_center_size(anchor, Vec2::splat(120.0)), 0.0, Stroke::new(1.5, accent.gamma_multiply(0.8)), egui::StrokeKind::Middle);
        }
        painter.circle_filled(anchor, 4.0, accent);

        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let bases = self
                    .selected()
                    .into_iter()
                    .filter_map(|i| {
                        let e = self.doc.sub.events.get(i)?;
                        let st = self.doc.sub.style(&e.style).cloned().unwrap_or_default();
                        Some((i, tag_f(&e.text, "frz").or_else(|| tag_f(&e.text, "fr")).unwrap_or(st.angle), tag_f(&e.text, "fscx").unwrap_or(st.scale_x), tag_f(&e.text, "fscy").unwrap_or(st.scale_y)))
                    })
                    .collect();
                self.typeset.drag = Some(Drag::Transform { anchor, start: p, bases });
                self.checkpoint();
                self.text_session = true;
            }
        }
        if let (Some(Drag::Transform { anchor, start, bases }), Some(p)) = (self.typeset.drag.clone(), resp.interact_pointer_pos()) {
            if resp.dragged() {
                let shift = ui.input(|i| i.modifiers.shift);
                let label;
                if rotate {
                    let a0 = (start - anchor).angle();
                    let a1 = (p - anchor).angle();
                    // screen y points down: counter-clockwise on screen = positive \frz
                    let mut d = -(a1 - a0).to_degrees();
                    if shift {
                        d = (d / 15.0).round() * 15.0;
                    }
                    for (i, frz, _, _) in &bases {
                        let v = ((frz + d + 540.0).rem_euclid(360.0)) - 180.0;
                        let e = &mut self.doc.sub.events[*i];
                        e.text = tags::set_tag(&e.text, "frz", &fmt(v));
                    }
                    painter.line_segment([anchor, p], Stroke::new(1.0, accent));
                    label = format!("{:+.1}°", d);
                } else {
                    let d0 = (start - anchor).length().max(4.0);
                    let (fx, fy) = if shift {
                        let bx = (start.x - anchor.x).abs().max(4.0);
                        let by = (start.y - anchor.y).abs().max(4.0);
                        ((p.x - anchor.x).abs() / bx, (p.y - anchor.y).abs() / by)
                    } else {
                        let f = (p - anchor).length() / d0;
                        (f, f)
                    };
                    for (i, _, bx, by) in &bases {
                        let e = &mut self.doc.sub.events[*i];
                        let t1 = tags::set_tag(&e.text, "fscx", &fmt((bx * fx).max(0.0)));
                        e.text = tags::set_tag(&t1, "fscy", &fmt((by * fy).max(0.0)));
                    }
                    painter.line_segment([anchor, p], Stroke::new(1.0, accent));
                    label = format!("{:.0}% × {:.0}%", fx * 100.0, fy * 100.0);
                }
                painter.text(p + Vec2::new(12.0, -12.0), Align2::LEFT_BOTTOM, label, FontId::monospace(12.0), accent);
                self.touch();
                self.buf_for = None;
            }
        }
        if resp.drag_stopped() {
            self.typeset.drag = None;
            self.end_text_session();
        }
    }

    /// Clip tool: vector (click to add points, drag points, right-click to delete) or rectangle.
    pub fn clip_tool(&mut self, ui: &egui::Ui, resp: &Response, painter: &egui::Painter, vrect: Rect) {
        let (pw, ph) = self.doc.sub.play_res();
        let (sx, sy) = (vrect.width() / pw.max(1) as f32, vrect.height() / ph.max(1) as f32);
        let to_screen = |x: f32, y: f32| Pos2::new(vrect.left() + x * sx, vrect.top() + y * sy);
        let to_script = |p: Pos2| (((p.x - vrect.left()) / sx).clamp(-(pw as f32), pw as f32 * 2.0), ((p.y - vrect.top()) / sy).clamp(-(ph as f32), ph as f32 * 2.0));
        let Some(c) = self.cur else { return };
        let clip = self.doc.sub.events.get(c).and_then(|e| Clip::parse(&e.text));
        let col = Color32::from_rgb(120, 220, 140);
        let hover = resp.hover_pos();

        // ---- draw
        match clip.as_ref().map(|c| &c.shape) {
            Some(ClipShape::Rect { x1, y1, x2, y2 }) => {
                let r = Rect::from_two_pos(to_screen(*x1, *y1), to_screen(*x2, *y2));
                painter.rect_stroke(r, 0.0, Stroke::new(1.5, col), egui::StrokeKind::Middle);
                for p in [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()] {
                    painter.rect_filled(Rect::from_center_size(p, Vec2::splat(8.0)), 1.0, col);
                }
            }
            Some(ClipShape::Polygon(pts)) | Some(ClipShape::Complex(pts)) => {
                let sp: Vec<Pos2> = pts.iter().map(|(x, y)| to_screen(*x, *y)).collect();
                if sp.len() >= 2 {
                    let mut closed = sp.clone();
                    closed.push(sp[0]);
                    let complex = matches!(clip.as_ref().map(|c| &c.shape), Some(ClipShape::Complex(_)));
                    if complex {
                        painter.add(egui::Shape::dashed_line(&closed, Stroke::new(1.5, col), 6.0, 4.0));
                    } else {
                        painter.add(egui::Shape::line(closed, Stroke::new(1.5, col)));
                    }
                }
                if matches!(clip.as_ref().map(|c| &c.shape), Some(ClipShape::Polygon(_))) {
                    for (k, p) in sp.iter().enumerate() {
                        let hot = hover.is_some_and(|h| h.distance(*p) < 8.0);
                        let s = if k == 0 { 10.0 } else { 8.0 };
                        painter.rect_filled(Rect::from_center_size(*p, Vec2::splat(if hot { s + 3.0 } else { s })), 1.0, if k == 0 { Color32::WHITE } else { col });
                    }
                }
            }
            None => {}
        }
        if let Some(cl) = &clip {
            let label = format!("{}{}", if cl.inverse { "\\iclip" } else { "\\clip" }, if matches!(cl.shape, ClipShape::Complex(_)) { " (curves — redraw to edit)" } else { "" });
            painter.text(vrect.left_bottom() + Vec2::new(6.0, -6.0), Align2::LEFT_BOTTOM, label, FontId::monospace(11.0), col);
        }

        let inverse = clip.as_ref().map(|c| c.inverse).unwrap_or(false);
        let near_vertex = |p: Pos2| -> Option<usize> {
            match clip.as_ref().map(|c| &c.shape) {
                Some(ClipShape::Polygon(pts)) => pts.iter().position(|(x, y)| to_screen(*x, *y).distance(p) < 9.0),
                _ => None,
            }
        };
        let rect_corner = |p: Pos2| -> Option<u8> {
            if let Some(ClipShape::Rect { x1, y1, x2, y2 }) = clip.as_ref().map(|c| &c.shape) {
                let r = Rect::from_two_pos(to_screen(*x1, *y1), to_screen(*x2, *y2));
                return [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()].iter().position(|q| q.distance(p) < 10.0).map(|k| k as u8);
            }
            None
        };

        // ---- interact
        match self.typeset.clip_mode {
            ClipMode::Vector => {
                if resp.secondary_clicked() {
                    if let (Some(p), Some(ClipShape::Polygon(mut pts))) = (resp.interact_pointer_pos(), clip.as_ref().map(|c| c.shape.clone())) {
                        if let Some(k) = near_vertex(p) {
                            pts.remove(k);
                            if pts.is_empty() {
                                self.clear_clip();
                            } else {
                                self.write_clip(&Clip { inverse, shape: ClipShape::Polygon(pts) });
                                self.end_text_session();
                            }
                        }
                    }
                }
                if resp.drag_started() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        if let Some(k) = near_vertex(p) {
                            self.typeset.drag = Some(Drag::Vertex(k));
                        }
                    }
                }
                if let (Some(Drag::Vertex(k)), true, Some(p)) = (self.typeset.drag.clone(), resp.dragged(), resp.interact_pointer_pos()) {
                    if let Some(ClipShape::Polygon(mut pts)) = clip.as_ref().map(|c| c.shape.clone()) {
                        if k < pts.len() {
                            pts[k] = to_script(p);
                            self.write_clip(&Clip { inverse, shape: ClipShape::Polygon(pts) });
                        }
                    }
                }
                if resp.clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        if near_vertex(p).is_none() {
                            let mut pts = match clip.as_ref().map(|c| c.shape.clone()) {
                                Some(ClipShape::Polygon(v)) => v,
                                _ => Vec::new(), // start fresh (also replaces rect / curves)
                            };
                            // insert into the nearest edge if clicking close to one, else append
                            let q = to_script(p);
                            let mut at = pts.len();
                            if pts.len() >= 3 {
                                let mut best = f32::MAX;
                                for k in 0..pts.len() {
                                    let a = to_screen(pts[k].0, pts[k].1);
                                    let b = to_screen(pts[(k + 1) % pts.len()].0, pts[(k + 1) % pts.len()].1);
                                    let d = seg_dist(p, a, b);
                                    if d < best {
                                        best = d;
                                        at = k + 1;
                                    }
                                }
                                if best > 12.0 {
                                    at = pts.len();
                                }
                            }
                            pts.insert(at, q);
                            self.write_clip(&Clip { inverse, shape: ClipShape::Polygon(pts) });
                            self.end_text_session();
                        }
                    }
                }
            }
            ClipMode::Rect => {
                if resp.drag_started() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.typeset.drag = Some(if let Some(k) = rect_corner(p) {
                            Drag::RectCorner(k)
                        } else if let Some(ClipShape::Rect { x1, y1, x2, y2 }) = clip.as_ref().map(|c| c.shape.clone()) {
                            let r = Rect::from_two_pos(to_screen(x1, y1), to_screen(x2, y2));
                            if r.contains(p) {
                                Drag::RectMove { start: to_script(p), orig: (x1, y1, x2, y2) }
                            } else {
                                Drag::RectNew(p)
                            }
                        } else {
                            Drag::RectNew(p)
                        });
                    }
                }
                if let (Some(d), true, Some(p)) = (self.typeset.drag.clone(), resp.dragged(), resp.interact_pointer_pos()) {
                    let q = to_script(p);
                    let shape = match (d, clip.as_ref().map(|c| c.shape.clone())) {
                        (Drag::RectNew(s), _) => {
                            let a = to_script(s);
                            Some(ClipShape::Rect { x1: a.0, y1: a.1, x2: q.0, y2: q.1 })
                        }
                        (Drag::RectCorner(k), Some(ClipShape::Rect { mut x1, mut y1, mut x2, mut y2 })) => {
                            // corners are reported in screen order: lt, rt, rb, lb (normalised rect)
                            let (l, t, r, b) = (x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2));
                            (x1, y1, x2, y2) = (l, t, r, b);
                            match k {
                                0 => (x1, y1) = q,
                                1 => (x2, y1) = q,
                                2 => (x2, y2) = q,
                                _ => (x1, y2) = q,
                            }
                            Some(ClipShape::Rect { x1, y1, x2, y2 })
                        }
                        (Drag::RectMove { start, orig }, _) => {
                            let (dx, dy) = (q.0 - start.0, q.1 - start.1);
                            Some(ClipShape::Rect { x1: orig.0 + dx, y1: orig.1 + dy, x2: orig.2 + dx, y2: orig.3 + dy })
                        }
                        _ => None,
                    };
                    if let Some(shape) = shape {
                        self.write_clip(&Clip { inverse, shape });
                    }
                }
            }
        }
        if resp.drag_stopped() {
            self.typeset.drag = None;
            self.end_text_session();
        }
        if let Some(h) = hover {
            if near_vertex(h).is_some() || rect_corner(h).is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            } else {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
        }
    }
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t).distance(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clip_parse_write() {
        let c = Clip::parse("{\\clip(10,20,30,40)}x").unwrap();
        assert_eq!(c.shape, ClipShape::Rect { x1: 10.0, y1: 20.0, x2: 30.0, y2: 40.0 });
        let v = Clip::parse("{\\iclip(m 0 0 l 100 0 100 50)}x").unwrap();
        assert!(v.inverse);
        assert_eq!(v.shape, ClipShape::Polygon(vec![(0.0, 0.0), (100.0, 0.0), (100.0, 50.0)]));
        assert_eq!(v.to_tag(), ("iclip".into(), "(m 0 0 l 100 0 100 50)".into()));
        let s = Clip::parse("{\\clip(2,m 0 0 l 200 0 200 100)}x").unwrap();
        assert_eq!(s.shape, ClipShape::Polygon(vec![(0.0, 0.0), (100.0, 0.0), (100.0, 50.0)]));
        assert!(matches!(Clip::parse("{\\clip(m 0 0 b 1 2 3 4 5 6)}x").unwrap().shape, ClipShape::Complex(_)));
    }
}
