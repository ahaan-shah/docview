//! Lucide icons (the same set allwrite uses), drawn with cairo from their SVG source so they
//! take the widget's CSS color and stroke exactly like allwrite's 17px, 1.75-stroke icons.
//! Only the SVG subset Lucide uses is supported: path, circle, ellipse, rect, line, polyline.

use gtk::prelude::*;
use gtk::{cairo, glib};

const ICONS: &[(&str, &str)] = &[
    ("open", include_str!("../data/icons/open.svg")),
    ("back", include_str!("../data/icons/back.svg")),
    ("forward", include_str!("../data/icons/forward.svg")),
    ("zoom-in", include_str!("../data/icons/zoom-in.svg")),
    ("zoom-out", include_str!("../data/icons/zoom-out.svg")),
    ("fit-width", include_str!("../data/icons/fit-width.svg")),
    ("fit-page", include_str!("../data/icons/fit-page.svg")),
    ("continuous", include_str!("../data/icons/continuous.svg")),
    ("dual", include_str!("../data/icons/dual.svg")),
    ("night", include_str!("../data/icons/night.svg")),
    ("highlight", include_str!("../data/icons/highlight.svg")),
    ("search", include_str!("../data/icons/search.svg")),
    ("outline", include_str!("../data/icons/outline.svg")),
    ("palette", include_str!("../data/icons/palette.svg")),
    ("settings", include_str!("../data/icons/settings.svg")),
    ("print", include_str!("../data/icons/print.svg")),
    ("up", include_str!("../data/icons/up.svg")),
    ("down", include_str!("../data/icons/down.svg")),
    ("close", include_str!("../data/icons/close.svg")),
    ("copy", include_str!("../data/icons/copy.svg")),
    ("focus", include_str!("../data/icons/focus.svg")),
    ("case", include_str!("../data/icons/case.svg")),
    ("file", include_str!("../data/icons/file.svg")),
];

pub fn svg(name: &str) -> Option<&'static str> {
    ICONS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// A widget drawing the named icon at `size` px in the current CSS color.
pub fn icon(name: &str, size: i32) -> gtk::Widget {
    let shapes = svg(name).map(parse).unwrap_or_default();
    let area = gtk::DrawingArea::builder().content_width(size).content_height(size).can_target(false).build();
    area.add_css_class("icon");
    area.set_halign(gtk::Align::Center);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(move |a, cr, w, h| {
        let c = a.color();
        cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, c.alpha() as f64);
        let s = (w.min(h) as f64) / 24.0;
        cr.translate((w as f64 - 24.0 * s) / 2.0, (h as f64 - 24.0 * s) / 2.0);
        cr.scale(s, s);
        cr.set_line_width(1.75);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);
        for shape in &shapes {
            draw_shape(cr, shape);
            let _ = cr.stroke();
        }
    });
    // The icon color follows the button's :hover / .active state.
    area.connect_state_flags_changed(|a, _| a.queue_draw());
    area.upcast()
}

#[derive(Clone, Debug, PartialEq)]
enum Shape {
    Path(Vec<Seg>),
    Circle(f64, f64, f64),
    Ellipse(f64, f64, f64, f64),
    Rect(f64, f64, f64, f64, f64),
    Lines(Vec<(f64, f64)>, bool),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    Move(f64, f64),
    Line(f64, f64),
    Cubic(f64, f64, f64, f64, f64, f64),
    Close,
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!(" {name}=\"");
    let start = tag.find(&pat)? + pat.len();
    let end = tag[start..].find('"')? + start;
    Some(&tag[start..end])
}

fn num(tag: &str, name: &str) -> f64 {
    attr(tag, name).and_then(|v| v.parse().ok()).unwrap_or(0.0)
}

fn parse(svg: &str) -> Vec<Shape> {
    let mut out = Vec::new();
    for raw in svg.split('<').skip(1) {
        let tag = raw.split('>').next().unwrap_or("");
        let name = tag.split_whitespace().next().unwrap_or("");
        let tag = format!(" {}", &tag[name.len().min(tag.len())..]);
        match name {
            "path" => {
                if let Some(d) = attr(&tag, "d") {
                    out.push(Shape::Path(parse_path(d)));
                }
            }
            "circle" => out.push(Shape::Circle(num(&tag, "cx"), num(&tag, "cy"), num(&tag, "r"))),
            "ellipse" => out.push(Shape::Ellipse(num(&tag, "cx"), num(&tag, "cy"), num(&tag, "rx"), num(&tag, "ry"))),
            "rect" => {
                let rx = attr(&tag, "rx").or(attr(&tag, "ry")).and_then(|v| v.parse().ok()).unwrap_or(0.0);
                out.push(Shape::Rect(num(&tag, "x"), num(&tag, "y"), num(&tag, "width"), num(&tag, "height"), rx));
            }
            "line" => out.push(Shape::Lines(
                vec![(num(&tag, "x1"), num(&tag, "y1")), (num(&tag, "x2"), num(&tag, "y2"))],
                false,
            )),
            "polyline" | "polygon" => {
                let nums: Vec<f64> = attr(&tag, "points")
                    .unwrap_or("")
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                out.push(Shape::Lines(nums.as_chunks::<2>().0.iter().map(|p| (p[0], p[1])).collect(), name == "polygon"));
            }
            _ => {}
        }
    }
    out
}

fn tokenize(d: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let b = d.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let ch = b[i] as char;
        if ch.is_ascii_alphabetic() {
            toks.push(Tok::Cmd(ch));
            i += 1;
        } else if ch == '-' || ch == '.' || ch.is_ascii_digit() {
            let start = i;
            let mut seen_dot = false;
            let mut seen_e = false;
            if ch == '-' {
                i += 1;
            }
            while i < b.len() {
                let c = b[i] as char;
                if c.is_ascii_digit() {
                    i += 1;
                } else if c == '.' && !seen_dot && !seen_e {
                    seen_dot = true;
                    i += 1;
                } else if (c == 'e' || c == 'E') && !seen_e {
                    seen_e = true;
                    i += 1;
                    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            toks.push(Tok::Num(d[start..i].parse().unwrap_or(0.0), d[start..i].to_string()));
        } else {
            i += 1;
        }
    }
    toks
}

#[derive(Clone, Debug)]
enum Tok {
    Cmd(char),
    Num(f64, String),
}

struct Reader {
    toks: Vec<Tok>,
    i: usize,
}

impl Reader {
    fn has_num(&self) -> bool {
        matches!(self.toks.get(self.i), Some(Tok::Num(..)))
    }
    fn num(&mut self) -> f64 {
        match self.toks.get(self.i) {
            Some(Tok::Num(n, _)) => {
                let n = *n;
                self.i += 1;
                n
            }
            _ => 0.0,
        }
    }
    /// Arc flags may be packed without separators ("a2 2 0 011 1"): a token like "011" is
    /// flag 0 followed by "11", which is read again for the next flag.
    fn flag(&mut self) -> f64 {
        let Some(Tok::Num(_, raw)) = self.toks.get(self.i).cloned() else { return 0.0 };
        let mut chars = raw.chars();
        let first = chars.next().unwrap_or('0');
        let rest: String = chars.collect();
        if rest.is_empty() || !(first == '0' || first == '1') {
            self.i += 1;
            return if first == '1' { 1.0 } else { 0.0 };
        }
        self.toks[self.i] = Tok::Num(rest.parse().unwrap_or(0.0), rest);
        if first == '1' { 1.0 } else { 0.0 }
    }
}

fn parse_path(d: &str) -> Vec<Seg> {
    let mut r = Reader { toks: tokenize(d), i: 0 };
    let mut segs = Vec::new();
    let (mut x, mut y) = (0.0f64, 0.0f64);
    let (mut sx, mut sy) = (0.0, 0.0);
    let mut last_ctrl: Option<(f64, f64)> = None;
    let mut cmd = 'M';
    while r.i < r.toks.len() {
        if let Some(Tok::Cmd(c)) = r.toks.get(r.i) {
            cmd = *c;
            r.i += 1;
            if cmd == 'z' || cmd == 'Z' {
                segs.push(Seg::Close);
                x = sx;
                y = sy;
                last_ctrl = None;
                continue;
            }
        } else if !r.has_num() {
            r.i += 1;
            continue;
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            'M' => {
                x = ox + r.num();
                y = oy + r.num();
                sx = x;
                sy = y;
                segs.push(Seg::Move(x, y));
                cmd = if rel { 'l' } else { 'L' };
                last_ctrl = None;
            }
            'L' => {
                x = ox + r.num();
                y = oy + r.num();
                segs.push(Seg::Line(x, y));
                last_ctrl = None;
            }
            'H' => {
                x = if rel { x + r.num() } else { r.num() };
                segs.push(Seg::Line(x, y));
                last_ctrl = None;
            }
            'V' => {
                y = if rel { y + r.num() } else { r.num() };
                segs.push(Seg::Line(x, y));
                last_ctrl = None;
            }
            'C' => {
                let (x1, y1, x2, y2) = (ox + r.num(), oy + r.num(), ox + r.num(), oy + r.num());
                x = ox + r.num();
                y = oy + r.num();
                segs.push(Seg::Cubic(x1, y1, x2, y2, x, y));
                last_ctrl = Some((x2, y2));
            }
            'S' => {
                let (x1, y1) = last_ctrl.map(|(cx, cy)| (2.0 * x - cx, 2.0 * y - cy)).unwrap_or((x, y));
                let (x2, y2) = (ox + r.num(), oy + r.num());
                x = ox + r.num();
                y = oy + r.num();
                segs.push(Seg::Cubic(x1, y1, x2, y2, x, y));
                last_ctrl = Some((x2, y2));
            }
            'Q' => {
                let (qx, qy) = (ox + r.num(), oy + r.num());
                let (ex, ey) = (ox + r.num(), oy + r.num());
                segs.push(quad(x, y, qx, qy, ex, ey));
                x = ex;
                y = ey;
                last_ctrl = None;
            }
            'A' => {
                let (rx, ry, rot) = (r.num(), r.num(), r.num());
                let (large, sweep) = (r.flag(), r.flag());
                let (ex, ey) = (ox + r.num(), oy + r.num());
                arc(&mut segs, x, y, rx, ry, rot, large != 0.0, sweep != 0.0, ex, ey);
                x = ex;
                y = ey;
                last_ctrl = None;
            }
            _ => {
                r.i += 1;
            }
        }
    }
    segs
}

fn quad(x0: f64, y0: f64, qx: f64, qy: f64, x: f64, y: f64) -> Seg {
    Seg::Cubic(x0 + 2.0 / 3.0 * (qx - x0), y0 + 2.0 / 3.0 * (qy - y0), x + 2.0 / 3.0 * (qx - x), y + 2.0 / 3.0 * (qy - y), x, y)
}

/// SVG endpoint arc -> cubic Béziers (SVG spec, appendix F.6).
#[allow(clippy::too_many_arguments)]
fn arc(segs: &mut Vec<Seg>, x1: f64, y1: f64, rx: f64, ry: f64, rot_deg: f64, large: bool, sweep: bool, x2: f64, y2: f64) {
    if (x1 - x2).abs() < 1e-9 && (y1 - y2).abs() < 1e-9 {
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < 1e-9 || ry < 1e-9 {
        segs.push(Seg::Line(x2, y2));
        return;
    }
    let phi = rot_deg.to_radians();
    let (cp, sp) = (phi.cos(), phi.sin());
    let dx = (x1 - x2) / 2.0;
    let dy = (y1 - y2) / 2.0;
    let x1p = cp * dx + sp * dy;
    let y1p = -sp * dx + cp * dy;
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = if den == 0.0 { 0.0 } else { (num / den).max(0.0).sqrt() };
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * y1p / ry;
    let cyp = -coef * ry * x1p / rx;
    let cx = cp * cxp - sp * cyp + (x1 + x2) / 2.0;
    let cy = sp * cxp + cp * cyp + (y1 + y2) / 2.0;
    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| {
        
        (ux * vy - uy * vx).atan2(ux * vx + uy * vy)
    };
    let t1 = angle(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dt = angle((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry);
    if !sweep && dt > 0.0 {
        dt -= std::f64::consts::TAU;
    } else if sweep && dt < 0.0 {
        dt += std::f64::consts::TAU;
    }
    let n = (dt.abs() / (std::f64::consts::FRAC_PI_2)).ceil().max(1.0) as usize;
    let step = dt / n as f64;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let point = |t: f64| {
        let (ct, st) = (t.cos(), t.sin());
        (cx + rx * ct * cp - ry * st * sp, cy + rx * ct * sp + ry * st * cp)
    };
    let deriv = |t: f64| {
        let (ct, st) = (t.cos(), t.sin());
        (-rx * st * cp - ry * ct * sp, -rx * st * sp + ry * ct * cp)
    };
    let mut t = t1;
    for i in 0..n {
        let t2 = t + step;
        let (p0x, p0y) = point(t);
        let (p3x, p3y) = if i == n - 1 { (x2, y2) } else { point(t2) };
        let (d0x, d0y) = deriv(t);
        let (d3x, d3y) = deriv(t2);
        segs.push(Seg::Cubic(p0x + k * d0x, p0y + k * d0y, p3x - k * d3x, p3y - k * d3y, p3x, p3y));
        t = t2;
    }
}

fn draw_shape(cr: &cairo::Context, s: &Shape) {
    cr.new_path();
    match s {
        Shape::Path(segs) => {
            for seg in segs {
                match *seg {
                    Seg::Move(x, y) => cr.move_to(x, y),
                    Seg::Line(x, y) => cr.line_to(x, y),
                    Seg::Cubic(a, b, c, d, e, f) => cr.curve_to(a, b, c, d, e, f),
                    Seg::Close => cr.close_path(),
                }
            }
        }
        Shape::Circle(cx, cy, r) => {
            cr.new_sub_path();
            cr.arc(*cx, *cy, *r, 0.0, std::f64::consts::TAU);
        }
        Shape::Ellipse(cx, cy, rx, ry) => {
            cr.save().ok();
            cr.translate(*cx, *cy);
            cr.scale(*rx, *ry);
            cr.new_sub_path();
            cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
            cr.restore().ok();
        }
        Shape::Rect(x, y, w, h, r) => {
            let r = r.min(w / 2.0).min(h / 2.0);
            if r <= 0.0 {
                cr.rectangle(*x, *y, *w, *h);
            } else {
                use std::f64::consts::{FRAC_PI_2, PI};
                cr.new_sub_path();
                cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
                cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
                cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
                cr.arc(x + r, y + r, r, PI, 1.5 * PI);
                cr.close_path();
            }
        }
        Shape::Lines(pts, closed) => {
            for (i, (x, y)) in pts.iter().enumerate() {
                if i == 0 { cr.move_to(*x, *y) } else { cr.line_to(*x, *y) }
            }
            if *closed {
                cr.close_path();
            }
        }
    }
}

/// Render an icon to a texture (used for the window's fallback icon and tests).
#[allow(dead_code)]
pub fn bounds_ok(name: &str) -> bool {
    let Some(src) = svg(name) else { return false };
    let shapes = parse(src);
    let Ok(surface) = cairo::ImageSurface::create(cairo::Format::ARgb32, 24, 24) else { return false };
    let Ok(cr) = cairo::Context::new(&surface) else { return false };
    cr.set_line_width(1.75);
    let mut ok = !shapes.is_empty();
    for s in &shapes {
        draw_shape(&cr, s);
        if let Ok((x0, y0, x1, y1)) = cr.stroke_extents() {
            ok &= x0 > -1.0 && y0 > -1.0 && x1 < 25.0 && y1 < 25.0;
        }
        cr.new_path();
    }
    let _ = glib::MainContext::default();
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses_inside_the_viewbox() {
        for (name, _) in ICONS {
            assert!(bounds_ok(name), "{name} draws outside 24x24 or is empty");
        }
    }

    #[test]
    fn packed_arc_flags() {
        // "a2 2 0 011 1" == rx 2, ry 2, rot 0, large 0, sweep 1, x 1, y 1
        let segs = parse_path("M0 0a2 2 0 011 1");
        match segs.last().unwrap() {
            Seg::Cubic(_, _, _, _, x, y) => assert!((x - 1.0).abs() < 1e-9 && (y - 1.0).abs() < 1e-9),
            s => panic!("unexpected {s:?}"),
        }
    }

    #[test]
    fn relative_commands() {
        let segs = parse_path("m6 14 1.5-2.9h2v1z");
        assert_eq!(segs[0], Seg::Move(6.0, 14.0));
        assert_eq!(segs[1], Seg::Line(7.5, 11.1));
        assert_eq!(segs[2], Seg::Line(9.5, 11.1));
        assert_eq!(segs[3], Seg::Line(9.5, 12.1));
        assert_eq!(segs[4], Seg::Close);
    }
}
