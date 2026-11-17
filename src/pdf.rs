//! Thin wrapper around poppler-glib. Everything outside this file works in page points
//! with a top-left origin (`Rect`), so another engine could slot in behind the same API.

use poppler::ffi;
use gtk::gio::prelude::*;
use gtk::glib::object::ObjectType;
use std::ffi::CStr;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        Rect { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) }
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }
    pub fn w(&self) -> f64 {
        self.x1 - self.x0
    }
    pub fn h(&self) -> f64 {
        self.y1 - self.y0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Page index and optional position (points from the page's top-left).
    Page { page: usize, x: Option<f64>, y: Option<f64> },
    Uri(String),
    /// Another file, opened in a new window.
    File(String),
}

#[derive(Clone, Debug)]
pub struct Link {
    pub rect: Rect,
    pub target: Target,
}

#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub title: String,
    pub depth: usize,
    pub target: Option<Target>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SelStyle {
    Glyph,
    Word,
    Line,
}

impl SelStyle {
    fn poppler(self) -> poppler::SelectionStyle {
        match self {
            SelStyle::Glyph => poppler::SelectionStyle::Glyph,
            SelStyle::Word => poppler::SelectionStyle::Word,
            SelStyle::Line => poppler::SelectionStyle::Line,
        }
    }
}

#[derive(Debug)]
pub enum OpenError {
    NeedsPassword,
    BadPassword,
    Other(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            OpenError::NeedsPassword => write!(f, "this document is password protected"),
            OpenError::BadPassword => write!(f, "wrong password"),
            OpenError::Other(s) => write!(f, "{s}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PageInfo {
    pub w: f64,
    pub h: f64,
    pub label: String,
}

pub struct Doc {
    pub inner: poppler::Document,
    pub path: PathBuf,
    pub password: Option<String>,
    pub pages: Vec<PageInfo>,
    pub title: Option<String>,
}

pub fn uri_for(path: &Path) -> String {
    gtk::gio::File::for_path(path).uri().to_string()
}

/// Open a document. Used by the main thread and by the worker threads (each has its own copy:
/// poppler objects are not thread safe).
pub fn open_raw(path: &Path, password: Option<&str>) -> Result<poppler::Document, OpenError> {
    poppler::Document::from_file(&uri_for(path), password).map_err(|e| {
        if e.matches(poppler::Error::Encrypted) {
            if password.is_some() { OpenError::BadPassword } else { OpenError::NeedsPassword }
        } else {
            OpenError::Other(e.message().to_string())
        }
    })
}

impl Doc {
    pub fn open(path: &Path, password: Option<&str>) -> Result<Doc, OpenError> {
        let inner = open_raw(path, password)?;
        let n = inner.n_pages().max(0);
        let mut pages = Vec::with_capacity(n as usize);
        for i in 0..n {
            let (w, h, label) = match inner.page(i) {
                Some(p) => {
                    let (w, h) = p.size();
                    (w, h, p.label().map(|l| l.to_string()).unwrap_or_default())
                }
                None => (612.0, 792.0, String::new()),
            };
            let label = if label.is_empty() { (i + 1).to_string() } else { label };
            pages.push(PageInfo { w: w.max(1.0), h: h.max(1.0), label });
        }
        let title = inner.title().map(|t| t.to_string()).filter(|t| !t.trim().is_empty());
        Ok(Doc { inner, path: path.to_path_buf(), password: password.map(String::from), pages, title })
    }

    pub fn n_pages(&self) -> usize {
        self.pages.len()
    }

    fn page(&self, i: usize) -> Option<poppler::Page> {
        self.inner.page(i as i32)
    }

    /// Page index for a typed label ("iv") or number ("12").
    pub fn find_page(&self, s: &str) -> Option<usize> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if let Some(i) = self.pages.iter().position(|p| p.label == s) {
            return Some(i);
        }
        let lower = s.to_lowercase();
        if let Some(i) = self.pages.iter().position(|p| p.label.to_lowercase() == lower) {
            return Some(i);
        }
        let n: usize = s.parse().ok()?;
        (n >= 1).then(|| (n - 1).min(self.n_pages().saturating_sub(1)))
    }

    pub fn links(&self, page: usize) -> Vec<Link> {
        let Some(p) = self.page(page) else { return Vec::new() };
        let h = self.pages[page].h;
        let mut out = Vec::new();
        unsafe {
            let list = ffi::poppler_page_get_link_mapping(p.as_ptr());
            let mut node = list;
            while !node.is_null() {
                let m = (*node).data as *const ffi::PopplerLinkMapping;
                if !m.is_null() {
                    let a = (*m).area;
                    if let Some(target) = self.action_target((*m).action) {
                        out.push(Link { rect: Rect::new(a.x1, h - a.y2, a.x2, h - a.y1), target });
                    }
                }
                node = (*node).next;
            }
            ffi::poppler_page_free_link_mapping(list);
        }
        out
    }

    unsafe fn action_target(&self, action: *const ffi::PopplerAction) -> Option<Target> {
        if action.is_null() {
            return None;
        }
        unsafe {
            match (*action).type_ {
                ffi::POPPLER_ACTION_GOTO_DEST => self.dest_target((*action).goto_dest.dest),
                ffi::POPPLER_ACTION_URI => cstr((*action).uri.uri).map(Target::Uri),
                ffi::POPPLER_ACTION_GOTO_REMOTE => cstr((*action).goto_remote.file_name).map(Target::File),
                ffi::POPPLER_ACTION_LAUNCH => cstr((*action).launch.file_name).map(Target::File),
                ffi::POPPLER_ACTION_NAMED => {
                    let name = cstr((*action).named.named_dest)?;
                    match name.as_str() {
                        "FirstPage" => Some(Target::Page { page: 0, x: None, y: None }),
                        "LastPage" => Some(Target::Page { page: self.n_pages().saturating_sub(1), x: None, y: None }),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
    }

    unsafe fn dest_target(&self, dest: *const ffi::PopplerDest) -> Option<Target> {
        if dest.is_null() {
            return None;
        }
        unsafe {
            if (*dest).type_ == ffi::POPPLER_DEST_NAMED {
                let name = (*dest).named_dest;
                if name.is_null() {
                    return None;
                }
                let found = ffi::poppler_document_find_dest(self.inner.as_ptr(), name);
                if found.is_null() {
                    return None;
                }
                let t = self.resolved_dest(found);
                ffi::poppler_dest_free(found);
                return t;
            }
            self.resolved_dest(dest)
        }
    }

    unsafe fn resolved_dest(&self, d: *const ffi::PopplerDest) -> Option<Target> {
        unsafe {
            let page = ((*d).page_num - 1).max(0) as usize;
            if page >= self.n_pages() {
                return None;
            }
            let h = self.pages[page].h;
            let (x, y) = match (*d).type_ {
                ffi::POPPLER_DEST_XYZ | ffi::POPPLER_DEST_FITH | ffi::POPPLER_DEST_FITBH => {
                    // change_left/change_top are bit fields poppler packs after change_left;
                    // the values are 0 when unset, which reads as the page top. Good enough.
                    let top = (*d).top;
                    let y = if top > 0.0 && top <= h + 1.0 { Some((h - top).max(0.0)) } else { None };
                    let x = if (*d).type_ == ffi::POPPLER_DEST_XYZ && (*d).left > 0.0 { Some((*d).left) } else { None };
                    (x, y)
                }
                ffi::POPPLER_DEST_FITR => (Some((*d).left), Some((h - (*d).top).max(0.0))),
                _ => (None, None),
            };
            Some(Target::Page { page, x, y })
        }
    }

    pub fn outline(&self) -> Vec<OutlineItem> {
        let mut out = Vec::new();
        unsafe {
            let iter = ffi::poppler_index_iter_new(self.inner.as_ptr());
            if !iter.is_null() {
                self.walk_index(iter, 0, &mut out);
                ffi::poppler_index_iter_free(iter);
            }
        }
        out
    }

    unsafe fn walk_index(&self, iter: *mut ffi::PopplerIndexIter, depth: usize, out: &mut Vec<OutlineItem>) {
        unsafe {
            loop {
                let action = ffi::poppler_index_iter_get_action(iter);
                if !action.is_null() {
                    let title = cstr((*action).any.title).unwrap_or_default();
                    let target = self.action_target(action);
                    out.push(OutlineItem { title: title.split_whitespace().collect::<Vec<_>>().join(" "), depth, target });
                    ffi::poppler_action_free(action);
                }
                let child = ffi::poppler_index_iter_get_child(iter);
                if !child.is_null() {
                    if depth < 12 {
                        self.walk_index(child, depth + 1, out);
                    }
                    ffi::poppler_index_iter_free(child);
                }
                if ffi::poppler_index_iter_next(iter) == 0 {
                    break;
                }
            }
        }
    }

    /// Selection rectangles (points, top-left origin) between two points on one page.
    pub fn selection_rects(&self, page: usize, a: (f64, f64), b: (f64, f64), style: SelStyle) -> Vec<Rect> {
        let Some(p) = self.page(page) else { return Vec::new() };
        let mut r = poppler::Rectangle::new();
        r.set_x1(a.0);
        r.set_y1(a.1);
        r.set_x2(b.0);
        r.set_y2(b.1);
        // Region coordinates are integers: ask at 8x and divide for sub-point precision.
        const S: f64 = 8.0;
        let Some(region) = p.selected_region(S, style.poppler(), &mut r) else { return Vec::new() };
        let mut rects = Vec::new();
        for i in 0..region.num_rectangles() {
            let c = region.rectangle(i);
            rects.push(Rect::new(
                c.x() as f64 / S,
                c.y() as f64 / S,
                (c.x() + c.width()) as f64 / S,
                (c.y() + c.height()) as f64 / S,
            ));
        }
        merge_lines(rects)
    }

    pub fn selected_text(&self, page: usize, a: (f64, f64), b: (f64, f64), style: SelStyle) -> String {
        let Some(p) = self.page(page) else { return String::new() };
        let mut r = poppler::Rectangle::new();
        r.set_x1(a.0);
        r.set_y1(a.1);
        r.set_x2(b.0);
        r.set_y2(b.1);
        p.selected_text(style.poppler(), &mut r).map(|s| s.to_string()).unwrap_or_default()
    }

    pub fn page_text(&self, page: usize) -> String {
        self.page(page).and_then(|p| p.text()).map(|s| s.to_string()).unwrap_or_default()
    }

    /// Highlight annotations on a page: (bounding rect, quads).
    pub fn highlights(&self, page: usize) -> Vec<(Rect, Vec<Rect>)> {
        let Some(p) = self.page(page) else { return Vec::new() };
        let h = self.pages[page].h;
        let mut out = Vec::new();
        unsafe {
            let list = ffi::poppler_page_get_annot_mapping(p.as_ptr());
            let mut node = list;
            while !node.is_null() {
                let m = (*node).data as *const ffi::PopplerAnnotMapping;
                if !m.is_null() && ffi::poppler_annot_get_annot_type((*m).annot) == ffi::POPPLER_ANNOT_HIGHLIGHT {
                    let a = (*m).area;
                    let quads = annot_quads((*m).annot as *mut ffi::PopplerAnnotTextMarkup, h);
                    out.push((Rect::new(a.x1, h - a.y2, a.x2, h - a.y1), quads));
                }
                node = (*node).next;
            }
            ffi::poppler_page_free_annot_mapping(list);
        }
        out
    }

    /// Add a highlight annotation covering `rects` (points, top-left origin).
    pub fn add_highlight(&self, page: usize, rects: &[Rect], rgb: (u8, u8, u8)) -> bool {
        if rects.is_empty() {
            return false;
        }
        let Some(p) = self.page(page) else { return false };
        let h = self.pages[page].h;
        let mut bound = rects[0];
        for r in rects {
            bound = Rect::new(bound.x0.min(r.x0), bound.y0.min(r.y0), bound.x1.max(r.x1), bound.y1.max(r.y1));
        }
        unsafe {
            let mut rect = ffi::PopplerRectangle { x1: bound.x0, y1: h - bound.y1, x2: bound.x1, y2: h - bound.y0 };
            let quads = gtk::glib::ffi::g_array_sized_new(0, 0, std::mem::size_of::<ffi::PopplerQuadrilateral>() as u32, rects.len() as u32);
            for r in rects {
                let pt = |x: f64, y: f64| ffi::PopplerPoint { x, y: h - y };
                // PDF order: top-left, top-right, bottom-left, bottom-right.
                let q = ffi::PopplerQuadrilateral { p1: pt(r.x0, r.y0), p2: pt(r.x1, r.y0), p3: pt(r.x0, r.y1), p4: pt(r.x1, r.y1) };
                gtk::glib::ffi::g_array_append_vals(quads, &q as *const _ as *const _, 1);
            }
            let annot = ffi::poppler_annot_text_markup_new_highlight(self.inner.as_ptr(), &mut rect, quads);
            gtk::glib::ffi::g_array_unref(quads);
            if annot.is_null() {
                return false;
            }
            let mut color = ffi::PopplerColor {
                red: rgb.0 as u16 * 257,
                green: rgb.1 as u16 * 257,
                blue: rgb.2 as u16 * 257,
            };
            ffi::poppler_annot_set_color(annot, &mut color);
            ffi::poppler_annot_set_flags(annot, ffi::POPPLER_ANNOT_FLAG_PRINT);
            ffi::poppler_page_add_annot(p.as_ptr(), annot);
            gtk::glib::gobject_ffi::g_object_unref(annot as *mut _);
        }
        true
    }

    /// Remove highlight annotations that overlap `area`. Returns what was removed (quads and
    /// color) so the change can be undone.
    pub fn remove_highlights(&self, page: usize, area: Rect) -> Vec<(Vec<Rect>, (u8, u8, u8))> {
        let Some(p) = self.page(page) else { return Vec::new() };
        let h = self.pages[page].h;
        let mut doomed = Vec::new();
        unsafe {
            let list = ffi::poppler_page_get_annot_mapping(p.as_ptr());
            let mut node = list;
            while !node.is_null() {
                let m = (*node).data as *const ffi::PopplerAnnotMapping;
                if !m.is_null() && ffi::poppler_annot_get_annot_type((*m).annot) == ffi::POPPLER_ANNOT_HIGHLIGHT {
                    let quads = annot_quads((*m).annot as *mut ffi::PopplerAnnotTextMarkup, h);
                    let a = (*m).area;
                    let whole = Rect::new(a.x1, h - a.y2, a.x2, h - a.y1);
                    let hit = if quads.is_empty() { whole.intersects(&area) } else { quads.iter().any(|q| q.intersects(&area)) };
                    if hit {
                        gtk::glib::gobject_ffi::g_object_ref((*m).annot as *mut _);
                        let color = annot_color((*m).annot);
                        let quads = if quads.is_empty() { vec![whole] } else { quads };
                        doomed.push(((*m).annot, quads, color));
                    }
                }
                node = (*node).next;
            }
            ffi::poppler_page_free_annot_mapping(list);
            for (a, _, _) in &doomed {
                ffi::poppler_page_remove_annot(p.as_ptr(), *a);
                gtk::glib::gobject_ffi::g_object_unref(*a as *mut _);
            }
        }
        doomed.into_iter().map(|(_, q, c)| (q, c)).collect()
    }

    /// Save to a temporary file next to the original, then rename over it. The open document
    /// keeps reading the old inode, so it stays valid.
    pub fn save_atomic(&self) -> Result<(), String> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let name = self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let tmp = dir.join(format!(".{name}.docview-{}.tmp", std::process::id()));
        self.inner.save(&uri_for(&tmp)).map_err(|e| e.message().to_string())?;
        if let Ok(meta) = std::fs::metadata(&self.path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    }
}

unsafe fn annot_color(annot: *mut ffi::PopplerAnnot) -> (u8, u8, u8) {
    unsafe {
        let c = ffi::poppler_annot_get_color(annot);
        if c.is_null() {
            return (255, 220, 0);
        }
        let out = (((*c).red / 257) as u8, ((*c).green / 257) as u8, ((*c).blue / 257) as u8);
        gtk::glib::ffi::g_free(c as *mut _);
        out
    }
}

unsafe fn annot_quads(annot: *mut ffi::PopplerAnnotTextMarkup, h: f64) -> Vec<Rect> {
    let mut out = Vec::new();
    unsafe {
        let arr = ffi::poppler_annot_text_markup_get_quadrilaterals(annot);
        if arr.is_null() {
            return out;
        }
        let n = (*arr).len as usize;
        let data = (*arr).data as *const ffi::PopplerQuadrilateral;
        for i in 0..n {
            let q = *data.add(i);
            let xs = [q.p1.x, q.p2.x, q.p3.x, q.p4.x];
            let ys = [q.p1.y, q.p2.y, q.p3.y, q.p4.y];
            let (x0, x1) = (xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max));
            let (y0, y1) = (ys.iter().cloned().fold(f64::MAX, f64::min), ys.iter().cloned().fold(f64::MIN, f64::max));
            out.push(Rect::new(x0, h - y1, x1, h - y0));
        }
        gtk::glib::ffi::g_array_unref(arr);
    }
    out
}

unsafe fn cstr(p: *const std::ffi::c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

/// Selected regions come back as many small boxes; join boxes that sit on the same line.
pub fn merge_lines(mut rects: Vec<Rect>) -> Vec<Rect> {
    rects.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap().then(a.x0.partial_cmp(&b.x0).unwrap()));
    let mut out: Vec<Rect> = Vec::new();
    for r in rects {
        if let Some(last) = out.last_mut() {
            let overlap = (last.y1.min(r.y1) - last.y0.max(r.y0)).max(0.0);
            let same_line = overlap > 0.5 * r.h().min(last.h());
            if same_line && r.x0 <= last.x1 + 2.0 {
                *last = Rect::new(last.x0.min(r.x0), last.y0.min(r.y0), last.x1.max(r.x1), last.y1.max(r.y1));
                continue;
            }
        }
        out.push(r);
    }
    out
}

/// Search one page. Returns match rectangles in points, top-left origin.
pub fn find_on_page(doc: &poppler::Document, page: usize, needle: &str, case_sensitive: bool) -> Vec<Rect> {
    let Some(p) = doc.page(page as i32) else { return Vec::new() };
    let (_, h) = p.size();
    let mut flags = poppler::FindFlags::MULTILINE;
    if case_sensitive {
        flags |= poppler::FindFlags::CASE_SENSITIVE;
    }
    p.find_text_with_options(needle, flags)
        .into_iter()
        .map(|r| Rect::new(r.x1(), h - r.y2(), r.x2(), h - r.y1()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_boxes_on_one_line() {
        let r = merge_lines(vec![
            Rect::new(10.0, 10.0, 20.0, 20.0),
            Rect::new(20.5, 10.0, 40.0, 20.0),
            Rect::new(10.0, 22.0, 30.0, 32.0),
        ]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], Rect::new(10.0, 10.0, 40.0, 20.0));
    }

    #[test]
    fn rect_is_normalized() {
        let r = Rect::new(5.0, 9.0, 1.0, 2.0);
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (1.0, 2.0, 5.0, 9.0));
        assert!(r.contains(3.0, 3.0));
    }
}
