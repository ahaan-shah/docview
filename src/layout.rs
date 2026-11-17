//! Page placement. Pure functions of page sizes and view settings, in logical pixels.

pub const MARGIN: f64 = 16.0;
pub const GAP: f64 = 12.0;
/// Logical pixels per point at 100% zoom (96 dpi screen, 72 points per inch).
pub const PX_PER_PT: f64 = 96.0 / 72.0;
pub const MIN_ZOOM: f64 = 0.1;
pub const MAX_ZOOM: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    None,
    Width,
    Page,
}

impl Fit {
    pub fn parse(s: &str) -> Fit {
        match s {
            "width" => Fit::Width,
            "page" => Fit::Page,
            _ => Fit::None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Fit::None => "none",
            Fit::Width => "width",
            Fit::Page => "page",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bx {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Bx {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opts {
    pub continuous: bool,
    pub dual: bool,
    pub cover: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// One box per page; None for pages not shown (single-page mode).
    pub boxes: Vec<Option<Bx>>,
    pub rows: Vec<Vec<usize>>,
    pub row_of: Vec<usize>,
    pub w: f64,
    pub h: f64,
}

pub fn rows(n: usize, dual: bool, cover: bool) -> Vec<Vec<usize>> {
    if !dual {
        return (0..n).map(|i| vec![i]).collect();
    }
    let mut rows = Vec::new();
    let mut i = 0;
    if cover && n > 0 {
        rows.push(vec![0]);
        i = 1;
    }
    while i < n {
        if i + 1 < n { rows.push(vec![i, i + 1]) } else { rows.push(vec![i]) }
        i += 2;
    }
    rows
}

/// A typical page size (the median), so one odd page does not decide the fit zoom.
pub fn typical_size(pages: &[(f64, f64)]) -> (f64, f64) {
    if pages.is_empty() {
        return (612.0, 792.0);
    }
    let mut ws: Vec<f64> = pages.iter().map(|p| p.0).collect();
    let mut hs: Vec<f64> = pages.iter().map(|p| p.1).collect();
    ws.sort_by(|a, b| a.partial_cmp(b).unwrap());
    hs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (ws[ws.len() / 2], hs[hs.len() / 2])
}

/// Zoom (1.0 = 100%) that makes a typical page (or spread) fit the view.
pub fn fit_zoom(pages: &[(f64, f64)], fit: Fit, dual: bool, view_w: f64, view_h: f64, current: f64) -> f64 {
    let (pw, ph) = typical_size(pages);
    let cols = if dual { 2.0 } else { 1.0 };
    let avail_w = (view_w - 2.0 * MARGIN - if dual { GAP } else { 0.0 }).max(50.0);
    let avail_h = (view_h - 2.0 * MARGIN).max(50.0);
    let by_w = avail_w / (cols * pw * PX_PER_PT);
    let z = match fit {
        Fit::None => current,
        Fit::Width => by_w,
        Fit::Page => by_w.min(avail_h / (ph * PX_PER_PT)),
    };
    z.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// `pages` are page sizes in points as displayed (after rotation).
pub fn compute(pages: &[(f64, f64)], zoom: f64, o: Opts, cur_row: usize, view_w: f64, view_h: f64) -> Layout {
    let ppp = zoom * PX_PER_PT;
    let rows = rows(pages.len(), o.dual, o.cover);
    let mut row_of = vec![0; pages.len()];
    for (r, row) in rows.iter().enumerate() {
        for &p in row {
            row_of[p] = r;
        }
    }
    let size = |p: usize| (pages[p].0 * ppp, pages[p].1 * ppp);
    // Half-width of each column in dual mode: spreads line up on a common spine.
    let half = if o.dual { pages.iter().map(|p| p.0 * ppp).fold(0.0, f64::max) } else { 0.0 };
    let row_w = |row: &Vec<usize>| -> f64 {
        if o.dual { 2.0 * half + GAP } else { size(row[0]).0 }
    };
    let shown: Vec<usize> = if o.continuous || rows.is_empty() {
        (0..rows.len()).collect()
    } else {
        vec![cur_row.min(rows.len() - 1)]
    };
    let max_w = shown.iter().map(|&r| row_w(&rows[r])).fold(0.0, f64::max);
    let w = (max_w + 2.0 * MARGIN).max(view_w);
    let center = (w / 2.0).floor();
    let mut boxes = vec![None; pages.len()];
    let mut y = MARGIN;
    if !o.continuous {
        if let Some(&r) = shown.first() {
            let rh = rows[r].iter().map(|&p| size(p).1).fold(0.0, f64::max);
            y = ((view_h - rh) / 2.0).max(MARGIN).floor();
        }
    }
    for &r in &shown {
        let row = &rows[r];
        let rh = row.iter().map(|&p| size(p).1).fold(0.0, f64::max);
        if o.dual {
            for (i, &p) in row.iter().enumerate() {
                let (pw, ph) = size(p);
                // Right column: the second page of a pair, or a lone cover page.
                let right = if row.len() == 2 { i == 1 } else { o.cover && p == 0 };
                let x = if right { center + GAP / 2.0 } else { center - GAP / 2.0 - pw };
                boxes[p] = Some(Bx { x: x.round(), y: (y + (rh - ph) / 2.0).round(), w: pw, h: ph });
            }
        } else {
            let p = row[0];
            let (pw, ph) = size(p);
            boxes[p] = Some(Bx { x: (center - pw / 2.0).round(), y: y.round(), w: pw, h: ph });
        }
        y += rh + GAP;
    }
    let content_h = y - GAP + MARGIN;
    Layout { boxes, rows, row_of, w, h: content_h.max(view_h) }
}

/// Unrotated page point (points, top-left origin) -> point on the page as displayed with a
/// clockwise rotation of `rot` degrees. `pw`, `ph` are the unrotated page size.
pub fn rot_point(rot: u32, pw: f64, ph: f64, x: f64, y: f64) -> (f64, f64) {
    match rot {
        90 => (ph - y, x),
        180 => (pw - x, ph - y),
        270 => (y, pw - x),
        _ => (x, y),
    }
}

/// Inverse of `rot_point`.
pub fn unrot_point(rot: u32, pw: f64, ph: f64, x: f64, y: f64) -> (f64, f64) {
    match rot {
        90 => (y, ph - x),
        180 => (pw - x, ph - y),
        270 => (pw - y, x),
        _ => (x, y),
    }
}

pub fn rot_rect(rot: u32, pw: f64, ph: f64, r: &crate::pdf::Rect) -> crate::pdf::Rect {
    let (ax, ay) = rot_point(rot, pw, ph, r.x0, r.y0);
    let (bx, by) = rot_point(rot, pw, ph, r.x1, r.y1);
    crate::pdf::Rect::new(ax, ay, bx, by)
}

/// Page size as displayed.
pub fn rot_size(rot: u32, w: f64, h: f64) -> (f64, f64) {
    if rot == 90 || rot == 270 { (h, w) } else { (w, h) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages(n: usize) -> Vec<(f64, f64)> {
        vec![(600.0, 800.0); n]
    }

    const CONT: Opts = Opts { continuous: true, dual: false, cover: false };

    #[test]
    fn rotation_roundtrips() {
        for rot in [0, 90, 180, 270] {
            let (dx, dy) = rot_point(rot, 600.0, 800.0, 10.0, 20.0);
            let (dw, dh) = rot_size(rot, 600.0, 800.0);
            assert!(dx >= 0.0 && dx <= dw && dy >= 0.0 && dy <= dh, "{rot}");
            let (x, y) = unrot_point(rot, 600.0, 800.0, dx, dy);
            assert!((x - 10.0).abs() < 1e-9 && (y - 20.0).abs() < 1e-9, "{rot}");
        }
        // 90° clockwise: the page's top-left corner ends up at the top-right.
        assert_eq!(rot_point(90, 600.0, 800.0, 0.0, 0.0), (800.0, 0.0));
    }

    #[test]
    fn rows_for_modes() {
        assert_eq!(rows(3, false, false), vec![vec![0], vec![1], vec![2]]);
        assert_eq!(rows(4, true, false), vec![vec![0, 1], vec![2, 3]]);
        assert_eq!(rows(4, true, true), vec![vec![0], vec![1, 2], vec![3]]);
        assert!(rows(0, true, true).is_empty());
    }

    #[test]
    fn continuous_stacks_pages() {
        let l = compute(&pages(3), 1.0, CONT, 0, 1000.0, 600.0);
        let b0 = l.boxes[0].unwrap();
        let b1 = l.boxes[1].unwrap();
        assert_eq!(b0.y, MARGIN);
        assert_eq!(b1.y, (MARGIN + 800.0 * PX_PER_PT + GAP).round());
        assert!(l.h > 3.0 * 800.0 * PX_PER_PT);
        // centered horizontally
        assert!((b0.x + b0.w / 2.0 - l.w / 2.0).abs() <= 1.0);
    }

    #[test]
    fn single_page_mode_shows_one_row() {
        let o = Opts { continuous: false, dual: false, cover: false };
        let l = compute(&pages(3), 0.5, o, 1, 1000.0, 2000.0);
        assert!(l.boxes[0].is_none() && l.boxes[2].is_none());
        assert!(l.boxes[1].is_some());
        assert_eq!(l.h, 2000.0);
    }

    #[test]
    fn dual_cover_puts_first_page_on_the_right() {
        let o = Opts { continuous: true, dual: true, cover: true };
        let l = compute(&pages(3), 1.0, o, 0, 1000.0, 600.0);
        let b0 = l.boxes[0].unwrap();
        let (b1, b2) = (l.boxes[1].unwrap(), l.boxes[2].unwrap());
        assert!(b0.x > l.w / 2.0);
        assert!(b1.x < l.w / 2.0 && b2.x > l.w / 2.0);
        assert_eq!(b1.y, b2.y);
    }

    #[test]
    fn fit_width_fills_the_view() {
        let p = pages(2);
        let z = fit_zoom(&p, Fit::Width, false, 1000.0, 500.0, 1.0);
        let l = compute(&p, z, CONT, 0, 1000.0, 500.0);
        let b = l.boxes[0].unwrap();
        assert!((b.w - (1000.0 - 2.0 * MARGIN)).abs() < 1.0);
        let zp = fit_zoom(&p, Fit::Page, false, 1000.0, 500.0, 1.0);
        assert!(zp < z);
        assert!((800.0 * zp * PX_PER_PT - (500.0 - 2.0 * MARGIN)).abs() < 1.0);
    }
}
