//! The document view: a GtkScrollable widget that lays pages out (layout.rs), draws cached
//! tiles from the render thread (render.rs) and handles selection, links, search and
//! highlights. Nothing here polls: drawing happens on scroll, resize and tile arrival.

use crate::layout::{self, Bx, Fit, Opts, MARGIN, PX_PER_PT};
use crate::pdf::{self, Doc, Rect, SelStyle, Target};
use crate::render::{self, Job, Recolor, Rendered, Renderer, TileKey, TILE};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// What changed, for the top bar / status bar / app.
#[derive(Clone, Debug)]
pub enum Change {
    /// Page, zoom, fit or layout mode changed.
    View,
    Selection,
    Find,
    /// Hovered link target, shown in the status bar.
    Hover(Option<String>),
    Message(String),
    /// A highlight was added or removed (file saved).
    Annotations,
    /// External file link clicked.
    OpenFile(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub bg: gdk::RGBA,
    pub page: gdk::RGBA,
    pub border: gdk::RGBA,
    pub selection: gdk::RGBA,
    pub find: gdk::RGBA,
    pub find_current: gdk::RGBA,
}

impl Default for Colors {
    fn default() -> Self {
        let c = |r: f32, g: f32, b: f32, a: f32| gdk::RGBA::new(r, g, b, a);
        Colors {
            bg: c(0.1, 0.1, 0.12, 1.0),
            page: c(1.0, 1.0, 1.0, 1.0),
            border: c(0.2, 0.2, 0.24, 1.0),
            selection: c(0.38, 0.69, 0.94, 0.32),
            find: c(0.9, 0.75, 0.4, 0.35),
            find_current: c(0.38, 0.69, 0.94, 0.5),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocPoint {
    pub page: usize,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug)]
struct Selection {
    start: DocPoint,
    end: DocPoint,
    style: SelStyle,
}

/// A saved place (for back / forward and remembered positions).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub page: usize,
    /// Points from the page top at the top of the view.
    pub y: f64,
}

#[derive(Clone, Debug)]
enum Edit {
    Added { page: usize, rects: Vec<Rect>, color: (u8, u8, u8) },
    Removed { page: usize, items: Vec<(Vec<Rect>, (u8, u8, u8))> },
}

struct Tile {
    tex: gdk::Texture,
    bytes: usize,
    used: u64,
    generation: u64,
}

#[derive(Default)]
struct Find {
    needle: String,
    case: bool,
    per_page: Vec<Vec<Rect>>,
    searched: usize,
    total: usize,
    current: Option<(usize, usize)>,
    start_page: usize,
    running: bool,
}

#[derive(Default)]
struct Drag {
    start: (f64, f64),
    /// Last pointer position in widget coordinates.
    last: (f64, f64),
    moved: bool,
    style: Option<SelStyle>,
    tick: Option<gtk::TickCallbackId>,
    on_link: Option<Target>,
}

pub struct State {
    pub doc: Option<Rc<Doc>>,
    pub zoom: f64,
    pub fit: Fit,
    pub opts: Opts,
    pub night: Option<Recolor>,
    pub colors: Colors,
    /// Clockwise rotation of every page, in degrees.
    pub rotation: u32,
    /// Page sizes in points as displayed (rotated).
    sizes: Vec<(f64, f64)>,
    layout: layout::Layout,
    cur_row: usize,
    view_w: f64,
    view_h: f64,
    tiles: HashMap<TileKey, Tile>,
    tile_bytes: usize,
    cache_limit: usize,
    frame: u64,
    generation: u64,
    /// Tiles of a page rendered before this generation are redrawn (after an annotation edit).
    page_gen: HashMap<u32, u64>,
    last_jobs: Vec<Job>,
    sel: Option<Selection>,
    sel_rects: Vec<(usize, Vec<Rect>)>,
    find: Find,
    links: HashMap<usize, Rc<Vec<pdf::Link>>>,
    back: Vec<Place>,
    forward: Vec<Place>,
    drag: Drag,
    pan: Option<(f64, f64)>,
    zoom_start: f64,
    flip_accum: f64,
    flip_block: bool,
    /// Place to restore once the widget has a size.
    pending_place: Option<Place>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    last_click: Option<DocPoint>,
    hover_link: Option<String>,
    pub highlight_rgb: (u8, u8, u8),
    /// (mtime, len) of the file right after our own save, so the watcher can ignore it.
    pub own_save: Option<(std::time::SystemTime, u64)>,
}

impl Default for State {
    fn default() -> Self {
        State {
            doc: None,
            zoom: 1.0,
            fit: Fit::Width,
            opts: Opts { continuous: true, dual: false, cover: true },
            night: None,
            colors: Colors::default(),
            rotation: 0,
            sizes: Vec::new(),
            layout: layout::Layout::default(),
            cur_row: 0,
            view_w: 0.0,
            view_h: 0.0,
            tiles: HashMap::new(),
            tile_bytes: 0,
            cache_limit: 48 << 20,
            frame: 0,
            generation: 0,
            page_gen: HashMap::new(),
            last_jobs: Vec::new(),
            sel: None,
            sel_rects: Vec::new(),
            find: Find::default(),
            links: HashMap::new(),
            back: Vec::new(),
            forward: Vec::new(),
            drag: Drag::default(),
            pan: None,
            zoom_start: 1.0,
            flip_accum: 0.0,
            flip_block: false,
            pending_place: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_click: None,
            hover_link: None,
            highlight_rgb: (224, 175, 104),
            own_save: None,
        }
    }
}

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::DocView)]
    pub struct DocView {
        #[property(get, set = Self::set_hadj, override_interface = gtk::Scrollable, nullable)]
        pub hadjustment: RefCell<Option<gtk::Adjustment>>,
        #[property(get, set = Self::set_vadj, override_interface = gtk::Scrollable, nullable)]
        pub vadjustment: RefCell<Option<gtk::Adjustment>>,
        #[property(get, set, override_interface = gtk::Scrollable)]
        pub hscroll_policy: Cell<gtk::ScrollablePolicy>,
        #[property(get, set, override_interface = gtk::Scrollable)]
        pub vscroll_policy: Cell<gtk::ScrollablePolicy>,
        pub st: RefCell<State>,
        pub renderer: OnceCell<Renderer>,
        pub listener: RefCell<Option<Rc<dyn Fn(Change)>>>,
        pub find_gen: Arc<AtomicU64>,
        pub adj_handlers: RefCell<Vec<(gtk::Adjustment, glib::SignalHandlerId)>>,
        pub trim_timer: RefCell<Option<glib::SourceId>>,
    }

    impl Default for DocView {
        fn default() -> Self {
            DocView {
                hadjustment: RefCell::new(None),
                vadjustment: RefCell::new(None),
                hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                st: RefCell::new(State::default()),
                renderer: OnceCell::new(),
                listener: RefCell::new(None),
                find_gen: Arc::new(AtomicU64::new(0)),
                adj_handlers: RefCell::new(Vec::new()),
                trim_timer: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DocView {
        const NAME: &'static str = "DocView";
        type Type = super::DocView;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Scrollable,);

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("docview");
        }
    }

    impl DocView {
        fn set_hadj(&self, adj: Option<gtk::Adjustment>) {
            self.hook_adj(&adj);
            self.hadjustment.replace(adj);
            self.obj().configure_adjustments();
        }
        fn set_vadj(&self, adj: Option<gtk::Adjustment>) {
            self.hook_adj(&adj);
            self.vadjustment.replace(adj);
            self.obj().configure_adjustments();
        }
        fn hook_adj(&self, adj: &Option<gtk::Adjustment>) {
            let Some(adj) = adj else { return };
            let weak = self.obj().downgrade();
            let id = adj.connect_value_changed(move |_| {
                if let Some(v) = weak.upgrade() {
                    v.on_scrolled();
                }
            });
            self.adj_handlers.borrow_mut().push((adj.clone(), id));
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for DocView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.add_css_class("docview-area");
            let weak = obj.downgrade();
            let renderer = Renderer::new(move |r| {
                if let Some(v) = weak.upgrade() {
                    v.on_rendered(r);
                }
            });
            let _ = self.renderer.set(renderer);
            obj.setup_input();
        }

        fn dispose(&self) {
            for (adj, id) in self.adj_handlers.take() {
                adj.disconnect(id);
            }
        }
    }

    impl WidgetImpl for DocView {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().on_resize(width as f64, height as f64);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }

    impl ScrollableImpl for DocView {}
}

glib::wrapper! {
    pub struct DocView(ObjectSubclass<imp::DocView>)
        @extends gtk::Widget,
        @implements gtk::Scrollable, gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for DocView {
    fn default() -> Self {
        glib::Object::new()
    }
}

fn rgba(c: gdk::RGBA, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(c.red(), c.green(), c.blue(), alpha)
}

impl DocView {
    pub fn new() -> DocView {
        DocView::default()
    }

    fn st(&self) -> std::cell::RefMut<'_, State> {
        self.imp().st.borrow_mut()
    }

    fn sr(&self) -> std::cell::Ref<'_, State> {
        self.imp().st.borrow()
    }

    pub fn connect_change(&self, f: impl Fn(Change) + 'static) {
        self.imp().listener.replace(Some(Rc::new(f)));
    }

    fn emit(&self, c: Change) {
        let l = self.imp().listener.borrow().clone();
        if let Some(l) = l {
            l(c);
        }
    }

    fn renderer(&self) -> &Renderer {
        self.imp().renderer.get().expect("renderer")
    }

    fn hadj(&self) -> Option<gtk::Adjustment> {
        self.imp().hadjustment.borrow().clone()
    }

    fn vadj(&self) -> Option<gtk::Adjustment> {
        self.imp().vadjustment.borrow().clone()
    }

    fn scroll_pos(&self) -> (f64, f64) {
        (self.hadj().map(|a| a.value()).unwrap_or(0.0), self.vadj().map(|a| a.value()).unwrap_or(0.0))
    }

    fn device_scale(&self) -> f64 {
        self.native().and_then(|n| n.surface()).map(|s| s.scale()).unwrap_or(self.scale_factor() as f64).max(1.0)
    }

    // ---------------------------------------------------------------- document

    pub fn doc(&self) -> Option<Rc<Doc>> {
        self.sr().doc.clone()
    }

    /// Show a document. `place` is where to start (None = first page).
    pub fn set_doc(&self, doc: Option<Doc>, place: Option<Place>) {
        let (path, pw) = doc.as_ref().map(|d| (Some(d.path.clone()), d.password.clone())).unwrap_or((None, None));
        let generation = self.renderer().set_doc(path, pw);
        self.cancel_find();
        {
            let mut st = self.st();
            st.doc = doc.map(Rc::new);
            let rot = st.rotation;
            st.sizes = st.doc.as_ref().map(|d| d.pages.iter().map(|p| layout::rot_size(rot, p.w, p.h)).collect()).unwrap_or_default();
            st.generation = generation;
            st.tiles.clear();
            st.tile_bytes = 0;
            st.page_gen.clear();
            st.last_jobs.clear();
            st.sel = None;
            st.sel_rects.clear();
            st.find = Find::default();
            st.links.clear();
            st.back.clear();
            st.forward.clear();
            st.undo.clear();
            st.redo.clear();
            st.cur_row = 0;
            st.last_click = None;
            st.pending_place = place;
        }
        self.relayout();
        self.apply_pending_place();
        self.emit(Change::View);
        self.emit(Change::Find);
        self.queue_draw();
    }

    /// Re-open after the file changed on disk, keeping the place.
    pub fn reload(&self, doc: Doc) {
        let place = self.place();
        let (zoom, fit) = (self.sr().zoom, self.sr().fit);
        let needle = self.sr().find.needle.clone();
        let case = self.sr().find.case;
        self.set_doc(Some(doc), place);
        {
            let mut st = self.st();
            st.zoom = zoom;
            st.fit = fit;
        }
        self.relayout();
        if let Some(p) = place {
            self.go_to_place(p, false);
        }
        if !needle.is_empty() {
            self.find(&needle, case);
        }
    }

    // ---------------------------------------------------------------- settings

    pub fn set_colors(&self, colors: Colors, night: Option<Recolor>) {
        let changed_night = {
            let mut st = self.st();
            st.colors = colors;
            let changed = st.night != night;
            st.night = night;
            changed
        };
        if changed_night {
            let generation = self.renderer().set_recolor(night);
            let mut st = self.st();
            st.generation = generation;
            st.tiles.clear();
            st.tile_bytes = 0;
            st.last_jobs.clear();
        }
        self.queue_draw();
    }

    pub fn set_cache_limit(&self, mb: u32) {
        self.st().cache_limit = (mb.clamp(16, 1024) as usize) << 20;
    }

    pub fn rotation(&self) -> u32 {
        self.sr().rotation
    }

    /// Rotate all pages by `delta` degrees (multiples of 90), keeping the current page.
    pub fn rotate(&self, delta: i32) {
        let page = self.current_page();
        let rot = ((self.sr().rotation as i32 + delta).rem_euclid(360)) as u32;
        let generation = self.renderer().set_rotation(rot);
        {
            let mut st = self.st();
            st.rotation = rot;
            st.sizes = st.doc.as_ref().map(|d| d.pages.iter().map(|p| layout::rot_size(rot, p.w, p.h)).collect()).unwrap_or_default();
            st.generation = generation;
            st.tiles.clear();
            st.tile_bytes = 0;
            st.last_jobs.clear();
        }
        self.relayout();
        if self.n_pages() > 0 {
            self.go_to_place(Place { page, y: -MARGIN / 2.0 / (self.zoom() * PX_PER_PT) }, false);
        }
        self.emit(Change::View);
    }

    /// Unrotated page rect -> displayed page rect.
    fn disp_rect(&self, st: &State, page: usize, r: &Rect) -> Rect {
        match st.doc.as_ref().and_then(|d| d.pages.get(page)) {
            Some(p) => layout::rot_rect(st.rotation, p.w, p.h, r),
            None => *r,
        }
    }

    pub fn zoom(&self) -> f64 {
        self.sr().zoom
    }

    pub fn fit(&self) -> Fit {
        self.sr().fit
    }

    pub fn opts(&self) -> Opts {
        self.sr().opts
    }

    pub fn set_fit(&self, fit: Fit) {
        let place = self.place();
        self.st().fit = fit;
        self.relayout();
        if let Some(p) = place {
            self.go_to_place(p, false);
        }
        self.emit(Change::View);
    }

    /// Set the zoom, keeping the document point under `anchor` (widget coordinates) in place.
    pub fn set_zoom(&self, zoom: f64, anchor: Option<(f64, f64)>) {
        let zoom = zoom.clamp(layout::MIN_ZOOM, layout::MAX_ZOOM);
        let (vw, vh) = (self.sr().view_w, self.sr().view_h);
        let (ax, ay) = anchor.unwrap_or((vw / 2.0, vh / 2.0));
        let (sx, sy) = self.scroll_pos();
        let hit = self.doc_point(sx + ax, sy + ay, true);
        {
            let mut st = self.st();
            st.zoom = zoom;
            st.fit = Fit::None;
        }
        self.relayout();
        if let Some(p) = hit {
            let (dx, dy) = self.disp_point(&p);
            if let Some(b) = self.sr().layout.boxes.get(p.page).copied().flatten() {
                let ppp = zoom * PX_PER_PT;
                self.set_scroll(b.x + dx * ppp - ax, b.y + dy * ppp - ay);
            }
        }
        self.emit(Change::View);
    }

    pub fn zoom_step(&self, dir: i32) {
        const STEPS: &[f64] = &[0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0, 6.4, 8.0];
        let z = self.zoom();
        let next = if dir > 0 {
            STEPS.iter().copied().find(|s| *s > z + 0.005).unwrap_or(layout::MAX_ZOOM)
        } else if dir < 0 {
            STEPS.iter().rev().copied().find(|s| *s < z - 0.005).unwrap_or(layout::MIN_ZOOM)
        } else {
            1.0
        };
        self.set_zoom(next, None);
    }

    pub fn set_opts(&self, opts: Opts) {
        let place = self.place();
        {
            let mut st = self.st();
            st.opts = opts;
            if let Some(p) = place {
                let rows = layout::rows(st.doc.as_ref().map(|d| d.n_pages()).unwrap_or(0), opts.dual, opts.cover);
                st.cur_row = rows.iter().position(|r| r.contains(&p.page)).unwrap_or(0);
            }
        }
        self.relayout();
        if let Some(p) = place {
            self.go_to_place(p, false);
        }
        self.emit(Change::View);
    }

    // ---------------------------------------------------------------- layout

    fn relayout(&self) {
        {
            let mut st = self.st();
            let (vw, vh) = (st.view_w, st.view_h);
            let Some(doc) = st.doc.clone() else {
                st.layout = layout::Layout::default();
                drop(st);
                self.configure_adjustments();
                return;
            };
            if vw <= 0.0 {
                return;
            }
            if st.fit != Fit::None {
                st.zoom = layout::fit_zoom(&st.sizes, st.fit, st.opts.dual, vw, vh, st.zoom);
            }
            let n_rows = layout::rows(doc.n_pages(), st.opts.dual, st.opts.cover).len();
            st.cur_row = st.cur_row.min(n_rows.saturating_sub(1));
            st.layout = layout::compute(&st.sizes, st.zoom, st.opts, st.cur_row, vw, vh);
        }
        self.configure_adjustments();
        self.queue_draw();
    }

    pub(crate) fn configure_adjustments(&self) {
        let (w, h, vw, vh) = {
            let st = self.sr();
            (st.layout.w, st.layout.h, st.view_w, st.view_h)
        };
        if let Some(a) = self.hadj() {
            let v = a.value().clamp(0.0, (w - vw).max(0.0));
            a.configure(v, 0.0, w.max(vw), 40.0, vw * 0.9, vw);
        }
        if let Some(a) = self.vadj() {
            let v = a.value().clamp(0.0, (h - vh).max(0.0));
            a.configure(v, 0.0, h.max(vh), 48.0, vh * 0.9, vh);
        }
    }

    fn on_resize(&self, w: f64, h: f64) {
        let first = self.sr().view_w <= 0.0;
        let place = if first { None } else { self.place() };
        {
            let mut st = self.st();
            if st.view_w == w && st.view_h == h {
                return;
            }
            st.view_w = w;
            st.view_h = h;
        }
        self.relayout();
        if let Some(p) = place
            && self.sr().fit != Fit::None {
                self.go_to_place(p, false);
            }
        self.apply_pending_place();
        self.emit(Change::View);
    }

    fn apply_pending_place(&self) {
        if self.sr().view_w <= 0.0 || self.sr().doc.is_none() {
            return;
        }
        let p = self.st().pending_place.take();
        if let Some(p) = p {
            self.go_to_place(p, false);
        }
    }

    fn set_scroll(&self, x: f64, y: f64) {
        if let Some(a) = self.hadj() {
            a.set_value(x.clamp(0.0, (a.upper() - a.page_size()).max(0.0)));
        }
        if let Some(a) = self.vadj() {
            a.set_value(y.clamp(0.0, (a.upper() - a.page_size()).max(0.0)));
        }
    }

    fn on_scrolled(&self) {
        self.queue_draw();
        self.emit(Change::View);
    }

    // ---------------------------------------------------------------- positions

    /// Page in the middle of the view (the one with the most visible area).
    pub fn current_page(&self) -> usize {
        let st = self.sr();
        let (sx, sy) = (self.hadj().map(|a| a.value()).unwrap_or(0.0), self.vadj().map(|a| a.value()).unwrap_or(0.0));
        let mut best = (0usize, -1.0f64);
        for (i, b) in st.layout.boxes.iter().enumerate() {
            let Some(b) = b else { continue };
            let vis_h = (b.y + b.h).min(sy + st.view_h) - b.y.max(sy);
            let vis_w = (b.x + b.w).min(sx + st.view_w) - b.x.max(sx);
            if vis_h <= 0.0 || vis_w <= 0.0 {
                if b.y > sy + st.view_h {
                    break;
                }
                continue;
            }
            let area = vis_h * vis_w;
            if area > best.1 + 1.0 {
                best = (i, area);
            }
        }
        best.0
    }

    pub fn n_pages(&self) -> usize {
        self.sr().doc.as_ref().map(|d| d.n_pages()).unwrap_or(0)
    }

    /// The place at the top of the view.
    pub fn place(&self) -> Option<Place> {
        let st = self.sr();
        st.doc.as_ref()?;
        if st.view_w <= 0.0 {
            return st.pending_place;
        }
        let (_, sy) = (0.0, self.vadj().map(|a| a.value()).unwrap_or(0.0));
        let ppp = st.zoom * PX_PER_PT;
        // First page whose bottom is below the view top.
        for (i, b) in st.layout.boxes.iter().enumerate() {
            let Some(b) = b else { continue };
            if b.y + b.h > sy + 1.0 {
                return Some(Place { page: i, y: ((sy - b.y) / ppp).max(-MARGIN / ppp) });
            }
        }
        None
    }

    pub fn go_to_place(&self, p: Place, remember: bool) {
        let n = self.n_pages();
        if n == 0 {
            return;
        }
        if remember {
            self.push_history();
        }
        let page = p.page.min(n - 1);
        self.ensure_row(page);
        let (b, ppp, wide, view_w) = {
            let st = self.sr();
            let Some(b) = st.layout.boxes.get(page).copied().flatten() else { return };
            (b, st.zoom * PX_PER_PT, st.layout.w > st.view_w, st.view_w)
        };
        // When zoomed wider than the window, keep the horizontal position if the target page
        // is already in view; otherwise center it.
        let cur_x = self.scroll_pos().0;
        let centered = b.x + b.w / 2.0 - view_w / 2.0;
        let x = if !wide { 0.0 } else if (cur_x - centered).abs() < view_w { cur_x } else { centered };
        self.set_scroll(x, b.y + p.y * ppp);
        self.emit(Change::View);
    }

    pub fn go_to_page(&self, page: usize) {
        self.go_to_place(Place { page, y: -MARGIN / 2.0 / (self.zoom() * PX_PER_PT) }, true);
    }

    /// Single-page mode: make the row holding `page` the shown one.
    fn ensure_row(&self, page: usize) {
        let need = {
            let st = self.sr();
            if st.opts.continuous || st.layout.row_of.is_empty() {
                false
            } else {
                st.layout.row_of.get(page).copied().unwrap_or(0) != st.cur_row
            }
        };
        if need {
            {
                let mut st = self.st();
                st.cur_row = st.layout.row_of.get(page).copied().unwrap_or(0);
            }
            self.relayout();
        }
    }

    fn push_history(&self) {
        if let Some(p) = self.place() {
            let mut st = self.st();
            if st.back.last() != Some(&p) {
                st.back.push(p);
                if st.back.len() > 100 {
                    st.back.remove(0);
                }
            }
            st.forward.clear();
        }
    }

    pub fn history_back(&self) -> bool {
        let Some(p) = self.st().back.pop() else { return false };
        if let Some(cur) = self.place() {
            self.st().forward.push(cur);
        }
        self.go_to_place(p, false);
        true
    }

    pub fn history_forward(&self) -> bool {
        let Some(p) = self.st().forward.pop() else { return false };
        if let Some(cur) = self.place() {
            self.st().back.push(cur);
        }
        self.go_to_place(p, false);
        true
    }

    pub fn can_go_back(&self) -> bool {
        !self.sr().back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.sr().forward.is_empty()
    }

    pub fn next_page(&self, dir: i32) {
        let n = self.n_pages();
        if n == 0 {
            return;
        }
        let dual = self.sr().opts.dual;
        let cur = self.current_page();
        let row_of = self.sr().layout.row_of.clone();
        let rows = self.sr().layout.rows.clone();
        let target = if dual {
            let r = row_of.get(cur).copied().unwrap_or(0) as i64 + dir as i64;
            rows.get(r.clamp(0, rows.len() as i64 - 1) as usize).and_then(|r| r.first().copied()).unwrap_or(cur)
        } else {
            (cur as i64 + dir as i64).clamp(0, n as i64 - 1) as usize
        };
        self.go_to_place(Place { page: target, y: -MARGIN / 2.0 / (self.zoom() * PX_PER_PT) }, false);
    }

    /// Scroll by a number of lines (dy) or screens. In single-page mode, running past the
    /// edge moves to the next / previous page.
    pub fn scroll_by(&self, dx: f64, dy: f64) {
        let Some(v) = self.vadj() else { return };
        let continuous = self.sr().opts.continuous;
        if !continuous && dy != 0.0 {
            let at_end = v.value() >= v.upper() - v.page_size() - 0.5;
            let at_start = v.value() <= 0.5;
            if dy > 0.0 && at_end {
                self.flip_row(1);
                return;
            }
            if dy < 0.0 && at_start {
                self.flip_row(-1);
                return;
            }
        }
        if let Some(h) = self.hadj() {
            h.set_value((h.value() + dx).clamp(0.0, (h.upper() - h.page_size()).max(0.0)));
        }
        v.set_value((v.value() + dy).clamp(0.0, (v.upper() - v.page_size()).max(0.0)));
    }

    pub fn scroll_screens(&self, n: f64) {
        let h = self.sr().view_h;
        self.scroll_by(0.0, n * (h - 48.0).max(h * 0.5));
    }

    fn flip_row(&self, dir: i32) {
        let (rows, cur) = (self.sr().layout.rows.len(), self.sr().cur_row);
        let next = cur as i64 + dir as i64;
        if next < 0 || next >= rows as i64 {
            return;
        }
        self.st().cur_row = next as usize;
        self.relayout();
        let v = self.vadj();
        if let Some(v) = v {
            v.set_value(if dir > 0 { 0.0 } else { (v.upper() - v.page_size()).max(0.0) });
        }
        self.emit(Change::View);
    }

    pub fn page_label(&self, page: usize) -> Option<String> {
        self.sr().doc.as_ref().and_then(|d| d.pages.get(page).map(|p| p.label.clone()))
    }

    // ---------------------------------------------------------------- coordinates

    /// Document-space point (layout px) to a page point. With `nearest`, points between or
    /// beside pages snap to the closest page (used while selecting).
    fn doc_point(&self, x: f64, y: f64, nearest: bool) -> Option<DocPoint> {
        let st = self.sr();
        let ppp = st.zoom * PX_PER_PT;
        let mut best: Option<(f64, usize, Bx)> = None;
        for (i, b) in st.layout.boxes.iter().enumerate() {
            let Some(b) = b else { continue };
            if b.contains(x, y) {
                return Some(self.page_point(&st, i, (x - b.x) / ppp, (y - b.y) / ppp));
            }
            if nearest {
                let dx = if x < b.x { b.x - x } else if x > b.x + b.w { x - b.x - b.w } else { 0.0 };
                let dy = if y < b.y { b.y - y } else if y > b.y + b.h { y - b.y - b.h } else { 0.0 };
                let d = dx * dx + dy * dy;
                if best.map(|bb| d < bb.0).unwrap_or(true) {
                    best = Some((d, i, *b));
                }
            }
        }
        best.map(|(_, i, b)| self.page_point(&st, i, ((x - b.x) / ppp).clamp(0.0, b.w / ppp), ((y - b.y) / ppp).clamp(0.0, b.h / ppp)))
    }

    /// Displayed page point -> unrotated page point.
    fn page_point(&self, st: &State, page: usize, dx: f64, dy: f64) -> DocPoint {
        let (x, y) = match st.doc.as_ref().and_then(|d| d.pages.get(page)) {
            Some(p) => layout::unrot_point(st.rotation, p.w, p.h, dx, dy),
            None => (dx, dy),
        };
        DocPoint { page, x, y }
    }

    /// Unrotated page point -> displayed page point.
    fn disp_point(&self, p: &DocPoint) -> (f64, f64) {
        let st = self.sr();
        match st.doc.as_ref().and_then(|d| d.pages.get(p.page)) {
            Some(pg) => layout::rot_point(st.rotation, pg.w, pg.h, p.x, p.y),
            None => (p.x, p.y),
        }
    }

    fn widget_point(&self, wx: f64, wy: f64, nearest: bool) -> Option<DocPoint> {
        let (sx, sy) = self.scroll_pos();
        self.doc_point(wx + sx, wy + sy, nearest)
    }

    /// Scroll so a page rectangle is visible, centering it if it is not.
    fn reveal(&self, page: usize, r: &Rect) {
        self.ensure_row(page);
        let r = &{
            let st = self.sr();
            self.disp_rect(&st, page, r)
        };
        let (vw, vh, b, ppp) = {
            let st = self.sr();
            let Some(b) = st.layout.boxes.get(page).copied().flatten() else { return };
            (st.view_w, st.view_h, b, st.zoom * PX_PER_PT)
        };
        let (sx, sy) = self.scroll_pos();
        let (x0, y0) = (b.x + r.x0 * ppp, b.y + r.y0 * ppp);
        let (x1, y1) = (b.x + r.x1 * ppp, b.y + r.y1 * ppp);
        let nx = if x0 < sx || x1 > sx + vw { (x0 + x1) / 2.0 - vw / 2.0 } else { sx };
        let ny = if y0 < sy + 8.0 || y1 > sy + vh - 8.0 { (y0 + y1) / 2.0 - vh / 3.0 } else { sy };
        self.set_scroll(nx, ny);
    }

    // ---------------------------------------------------------------- drawing

    fn draw(&self, snap: &gtk::Snapshot) {
        let s = self.device_scale();
        let (sx, sy) = self.scroll_pos();
        let mut jobs: Vec<Job> = Vec::new();
        let mut prefetch: Vec<Job> = Vec::new();
        let mut previews: Vec<Job> = Vec::new();
        {
            let mut st = self.st();
            st.frame += 1;
            let frame = st.frame;
            let Some(doc) = st.doc.clone() else { return };
            let (vw, vh) = (st.view_w, st.view_h);
            let ppp = st.zoom * PX_PER_PT;
            let rq = render::quantize(ppp * s);
            let rs = rq as f64 / 1000.0;
            let colors = st.colors;
            let page_bg = if let Some(n) = st.night {
                gdk::RGBA::new(n.bg[0] as f32 / 255.0, n.bg[1] as f32 / 255.0, n.bg[2] as f32 / 255.0, 1.0)
            } else {
                colors.page
            };
            let snap_px = |v: f64| (v * s).round() / s;
            let margin = vh * 0.75;
            for (i, b) in st.layout.boxes.clone().iter().enumerate() {
                let Some(b) = b else { continue };
                // Pages near the view get prefetched; only visible ones are drawn.
                if b.y + b.h < sy - margin || b.x + b.w < sx || b.x > sx + vw {
                    continue;
                }
                if b.y > sy + vh + margin {
                    break;
                }
                let visible = b.y + b.h >= sy && b.y <= sy + vh;
                let page = i as u32;
                let min_gen = st.page_gen.get(&page).copied().unwrap_or(0);
                let pkey = TileKey { page, scale: 0, tx: 0, ty: 0 };
                let stale = |t: &Tile| t.generation < min_gen;
                if st.tiles.get(&pkey).map(&stale).unwrap_or(true) {
                    previews.push(Job { page, scale: 0, tiles: vec![(0, 0)] });
                }
                // Visible part of the page, in page device pixels.
                let y_lo = if visible { sy } else { sy - margin };
                let y_hi = if visible { sy + vh } else { sy + vh + margin };
                let full_w = (st.sizes[i].0 * rs).ceil();
                let full_h = (st.sizes[i].1 * rs).ceil();
                let dev = |v: f64, o: f64| ((v - o) / ppp * rs).max(0.0);
                let tx0 = (dev(sx.max(b.x), b.x) / TILE as f64).floor() as u32;
                let tx1 = ((dev((sx + vw).min(b.x + b.w), b.x).min(full_w) - 1.0).max(0.0) / TILE as f64).floor() as u32;
                let ty0 = (dev(y_lo.max(b.y), b.y) / TILE as f64).floor() as u32;
                let ty1 = ((dev(y_hi.min(b.y + b.h), b.y).min(full_h) - 1.0).max(0.0) / TILE as f64).floor() as u32;
                let mut want = Vec::new();
                let mut want_later = Vec::new();
                for ty in ty0..=ty1 {
                    for tx in tx0..=tx1 {
                        let key = TileKey { page, scale: rq, tx, ty };
                        let ok = st.tiles.get(&key).map(|t| !stale(t)).unwrap_or(false);
                        if !ok {
                            // Only rows that intersect the view are urgent.
                            let ty_top = b.y + (ty * TILE) as f64 / rs * ppp;
                            let ty_bot = ty_top + TILE as f64 / rs * ppp;
                            if ty_bot >= sy && ty_top <= sy + vh { want.push((tx, ty)) } else { want_later.push((tx, ty)) }
                        }
                    }
                }
                if !want.is_empty() {
                    jobs.push(Job { page, scale: rq, tiles: want });
                }
                if !want_later.is_empty() {
                    prefetch.push(Job { page, scale: rq, tiles: want_later });
                }
                if !visible {
                    continue;
                }

                // ---- paint the page
                let x = snap_px(b.x - sx);
                let y = snap_px(b.y - sy);
                let rect = graphene::Rect::new(x as f32, y as f32, b.w as f32, b.h as f32);
                let border = graphene::Rect::new(x as f32 - 1.0, y as f32 - 1.0, b.w as f32 + 2.0, b.h as f32 + 2.0);
                snap.append_color(&colors.border, &border);
                snap.append_color(&page_bg, &rect);
                if let Some(t) = st.tiles.get_mut(&pkey) {
                    t.used = frame;
                    snap.append_scaled_texture(&t.tex, gsk::ScalingFilter::Linear, &rect);
                }
                // Tiles from another zoom level stand in until the new ones arrive.
                let have_all = (ty0..=ty1).all(|ty| (tx0..=tx1).all(|tx| st.tiles.contains_key(&TileKey { page, scale: rq, tx, ty })));
                if !have_all {
                    let mut scales: HashMap<u32, usize> = HashMap::new();
                    for k in st.tiles.keys() {
                        if k.page == page && k.scale != 0 && k.scale != rq {
                            *scales.entry(k.scale).or_default() += 1;
                        }
                    }
                    // Prefer the closest scale.
                    if let Some((&fs, _)) = scales.iter().min_by_key(|(sc, _)| (**sc as i64 - rq as i64).abs()) {
                        let frs = fs as f64 / 1000.0;
                        let keys: Vec<TileKey> = st.tiles.keys().filter(|k| k.page == page && k.scale == fs).copied().collect();
                        for k in keys {
                            let t = st.tiles.get_mut(&k).unwrap();
                            t.used = frame;
                            let tw = t.tex.width() as f64 / frs * ppp;
                            let th = t.tex.height() as f64 / frs * ppp;
                            let r = graphene::Rect::new(
                                (x + (k.tx * TILE) as f64 / frs * ppp) as f32,
                                (y + (k.ty * TILE) as f64 / frs * ppp) as f32,
                                tw as f32,
                                th as f32,
                            );
                            snap.append_scaled_texture(&t.tex, gsk::ScalingFilter::Linear, &r);
                        }
                    }
                }
                for ty in ty0..=ty1 {
                    for tx in tx0..=tx1 {
                        if let Some(t) = st.tiles.get_mut(&TileKey { page, scale: rq, tx, ty }) {
                            t.used = frame;
                            let r = graphene::Rect::new(
                                (x + (tx * TILE) as f64 / s) as f32,
                                (y + (ty * TILE) as f64 / s) as f32,
                                (t.tex.width() as f64 / s) as f32,
                                (t.tex.height() as f64 / s) as f32,
                            );
                            snap.append_scaled_texture(&t.tex, gsk::ScalingFilter::Linear, &r);
                        }
                    }
                }

                // ---- overlays: search matches, selection
                let (upw, uph) = (doc.pages[i].w, doc.pages[i].h);
                let rot = st.rotation;
                let to_screen = |r: &Rect| {
                    let r = &layout::rot_rect(rot, upw, uph, r);
                    graphene::Rect::new(
                        (x + r.x0 * ppp) as f32,
                        (y + r.y0 * ppp) as f32,
                        (r.w() * ppp).max(1.0) as f32,
                        (r.h() * ppp).max(1.0) as f32,
                    )
                };
                if let Some(matches) = st.find.per_page.get(i) {
                    for (mi, m) in matches.iter().enumerate() {
                        let current = st.find.current == Some((i, mi));
                        let r = to_screen(m);
                        let rr = gsk::RoundedRect::from_rect(r.inset_r(-1.5, -1.0), 2.0);
                        snap.push_rounded_clip(&rr);
                        snap.append_color(if current { &colors.find_current } else { &colors.find }, rr.bounds());
                        snap.pop();
                        if current {
                            let c = rgba(colors.find_current, 1.0);
                            snap.append_border(&rr, &[1.5; 4], &[c, c, c, c]);
                        }
                    }
                }
                for (p, rects) in &st.sel_rects {
                    if *p != i {
                        continue;
                    }
                    for r in rects {
                        snap.append_color(&colors.selection, &to_screen(r));
                    }
                }
            }
        }
        // Order: visible tiles, previews of nearby pages, then prefetch.
        let mut all = Vec::with_capacity(jobs.len() + previews.len() + prefetch.len());
        // Previews of visible pages first: they are cheap and fill the page instantly.
        all.extend(previews);
        all.extend(jobs);
        all.extend(prefetch);
        let changed = {
            let mut st = self.st();
            if st.last_jobs != all {
                st.last_jobs = all.clone();
                true
            } else {
                false
            }
        };
        if changed {
            self.renderer().request(all);
        }
    }

    /// Once rendering goes quiet, return freed memory to the system. One-shot, not a poll.
    fn schedule_trim(&self) {
        if let Some(t) = self.imp().trim_timer.take() {
            t.remove();
        }
        let weak = self.downgrade();
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            if let Some(v) = weak.upgrade() {
                v.imp().trim_timer.take();
                crate::trim_heap();
            }
        });
        self.imp().trim_timer.replace(Some(id));
    }

    fn on_rendered(&self, r: Rendered) {
        if std::env::var_os("DOCVIEW_DEBUG").is_some() && self.sr().tiles.is_empty() {
            eprintln!("first tile after {:?}", crate::START.get().map(|s| s.elapsed()));
        }
        {
            let mut st = self.st();
            if r.generation != st.generation {
                return;
            }
            let bytes = r.data.len();
            let gbytes = glib::Bytes::from_owned(r.data);
            let tex = gdk::MemoryTexture::new(r.w, r.h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &gbytes, r.w as usize * 4);
            let frame = st.frame;
            let generation = st.generation;
            if let Some(old) = st.tiles.insert(r.key, Tile { tex: tex.upcast(), bytes, used: frame, generation }) {
                st.tile_bytes -= old.bytes;
            }
            st.tile_bytes += bytes;
            // Drop the job from the remembered list so a re-request is not suppressed.
            st.last_jobs.clear();
            if st.tile_bytes > st.cache_limit {
                let mut entries: Vec<(TileKey, u64, usize)> = st.tiles.iter().map(|(k, t)| (*k, t.used, t.bytes)).collect();
                entries.sort_by_key(|e| e.1);
                let limit = st.cache_limit * 3 / 4;
                for (k, used, bytes) in entries {
                    if st.tile_bytes <= limit || used >= frame {
                        break;
                    }
                    st.tiles.remove(&k);
                    st.tile_bytes -= bytes;
                }
            }
        }
        self.queue_draw();
        self.schedule_trim();
    }

    /// Memory held by rendered tiles, for the status bar in debug builds and tests.
    pub fn cache_bytes(&self) -> usize {
        self.sr().tile_bytes
    }

    // ---------------------------------------------------------------- input

    fn setup_input(&self) {
        // Primary button: select text, follow links.
        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        {
            let v = self.downgrade();
            click.connect_pressed(move |g, n, x, y| {
                let Some(v) = v.upgrade() else { return };
                v.grab_focus();
                let shift = g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
                v.on_press(n, x, y, shift);
            });
        }
        {
            let v = self.downgrade();
            drag.connect_drag_begin(move |_, x, y| {
                if let Some(v) = v.upgrade() {
                    let mut st = v.st();
                    st.drag.start = (x, y);
                    st.drag.last = (x, y);
                    st.drag.moved = false;
                }
            });
        }
        {
            let v = self.downgrade();
            drag.connect_drag_update(move |g, dx, dy| {
                let Some(v) = v.upgrade() else { return };
                let (x0, y0) = v.sr().drag.start;
                let (x, y) = (x0 + dx, y0 + dy);
                v.st().drag.last = (x, y);
                if !v.sr().drag.moved && dx.hypot(dy) < 4.0 {
                    return;
                }
                if !v.sr().drag.moved {
                    v.st().drag.moved = true;
                    let _ = g;
                    v.start_autoscroll();
                }
                v.extend_selection_to(x, y);
            });
        }
        {
            let v = self.downgrade();
            drag.connect_drag_end(move |_, dx, dy| {
                let Some(v) = v.upgrade() else { return };
                let moved = v.sr().drag.moved;
                let tick = v.st().drag.tick.take();
                if let Some(t) = tick {
                    t.remove();
                }
                if moved {
                    v.selection_done();
                } else if dx.hypot(dy) < 4.0 {
                    let link = v.st().drag.on_link.take();
                    if let Some(t) = link {
                        v.follow(t);
                    }
                }
                v.st().drag.style = None;
            });
        }
        self.add_controller(click);
        self.add_controller(drag);

        // Middle button: pan.
        let pan = gtk::GestureDrag::new();
        pan.set_button(gdk::BUTTON_MIDDLE);
        {
            let v = self.downgrade();
            pan.connect_drag_begin(move |_, _, _| {
                if let Some(v) = v.upgrade() {
                    let p = v.scroll_pos();
                    v.st().pan = Some(p);
                    v.set_cursor_from_name(Some("grabbing"));
                }
            });
        }
        {
            let v = self.downgrade();
            pan.connect_drag_update(move |_, dx, dy| {
                if let Some(v) = v.upgrade()
                    && let Some((px, py)) = v.sr().pan {
                        v.set_scroll(px - dx, py - dy);
                    }
            });
        }
        {
            let v = self.downgrade();
            pan.connect_drag_end(move |_, _, _| {
                if let Some(v) = v.upgrade() {
                    v.st().pan = None;
                    v.set_cursor_from_name(Some("default"));
                }
            });
        }
        self.add_controller(pan);

        // Ctrl+wheel zoom; page flips in single-page mode.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        {
            let v = self.downgrade();
            scroll.connect_scroll(move |c, _dx, dy| {
                let Some(v) = v.upgrade() else { return glib::Propagation::Proceed };
                let state = c.current_event_state();
                if state.contains(gdk::ModifierType::CONTROL_MASK) {
                    let anchor = c
                        .current_event()
                        .and_then(|e| e.position())
                        .and_then(|(x, y)| {
                            let native = v.native()?;
                            let (ox, oy) = native.surface_transform();
                            let p = native.compute_point(&v, &graphene::Point::new((x - ox) as f32, (y - oy) as f32))?;
                            Some((p.x() as f64, p.y() as f64))
                        });
                    let smooth = c.unit() == gdk::ScrollUnit::Surface;
                    let factor = if smooth { (-dy * 0.01).exp() } else { 1.1f64.powf(-dy) };
                    v.set_zoom(v.zoom() * factor, anchor);
                    return glib::Propagation::Stop;
                }
                if !v.sr().opts.continuous && dy != 0.0 {
                    let Some(a) = v.vadj() else { return glib::Propagation::Proceed };
                    let at_end = a.value() >= a.upper() - a.page_size() - 0.5;
                    let at_start = a.value() <= 0.5;
                    if (dy > 0.0 && at_end) || (dy < 0.0 && at_start) {
                        let smooth = c.unit() == gdk::ScrollUnit::Surface;
                        let mut st = v.st();
                        if st.flip_block {
                            return glib::Propagation::Stop;
                        }
                        st.flip_accum += if smooth { dy / 40.0 } else { dy };
                        if st.flip_accum.abs() >= 1.0 {
                            let dir = st.flip_accum.signum() as i32;
                            st.flip_accum = 0.0;
                            st.flip_block = true;
                            drop(st);
                            v.flip_row(dir);
                            let w = v.downgrade();
                            glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                                if let Some(v) = w.upgrade() {
                                    v.st().flip_block = false;
                                }
                            });
                        }
                        return glib::Propagation::Stop;
                    }
                    v.st().flip_accum = 0.0;
                }
                glib::Propagation::Proceed
            });
        }
        self.add_controller(scroll);

        // Pinch zoom.
        let pinch = gtk::GestureZoom::new();
        {
            let v = self.downgrade();
            pinch.connect_begin(move |_, _| {
                if let Some(v) = v.upgrade() {
                    let z = v.zoom();
                    v.st().zoom_start = z;
                }
            });
        }
        {
            let v = self.downgrade();
            pinch.connect_scale_changed(move |g, scale| {
                if let Some(v) = v.upgrade() {
                    let anchor = g.bounding_box_center();
                    let z = v.sr().zoom_start * scale;
                    v.set_zoom(z, anchor);
                }
            });
        }
        self.add_controller(pinch);

        // Pointer shape and link targets in the status bar.
        let motion = gtk::EventControllerMotion::new();
        {
            let v = self.downgrade();
            motion.connect_motion(move |_, x, y| {
                if let Some(v) = v.upgrade() {
                    v.on_motion(x, y);
                }
            });
        }
        {
            let v = self.downgrade();
            motion.connect_leave(move |_| {
                if let Some(v) = v.upgrade()
                    && v.st().hover_link.take().is_some() {
                        v.emit(Change::Hover(None));
                    }
            });
        }
        self.add_controller(motion);
    }

    /// Dev hook: replay a pointer gesture through the same code paths as the real gestures.
    /// `n` is the click count; `to` makes it a drag.
    pub fn simulate(&self, n: i32, from: (f64, f64), to: Option<(f64, f64)>) {
        self.on_press(n, from.0, from.1, false);
        match to {
            Some((x, y)) => {
                {
                    let mut st = self.st();
                    st.drag.start = from;
                    st.drag.last = (x, y);
                    st.drag.moved = true;
                }
                self.extend_selection_to(x, y);
                self.selection_done();
            }
            None => {
                let link = self.st().drag.on_link.take();
                if let Some(t) = link {
                    self.follow(t);
                }
            }
        }
        self.st().drag.style = None;
    }

    fn links_on(&self, page: usize) -> Rc<Vec<pdf::Link>> {
        if let Some(l) = self.sr().links.get(&page) {
            return l.clone();
        }
        let links = Rc::new(self.doc().map(|d| d.links(page)).unwrap_or_default());
        self.st().links.insert(page, links.clone());
        links
    }

    fn link_at(&self, p: &DocPoint) -> Option<pdf::Link> {
        self.links_on(p.page).iter().find(|l| l.rect.contains(p.x, p.y)).cloned()
    }

    fn on_motion(&self, x: f64, y: f64) {
        if self.sr().pan.is_some() {
            return;
        }
        let p = self.widget_point(x, y, false);
        let link = p.as_ref().and_then(|p| self.link_at(p));
        let cursor = if link.is_some() { "pointer" } else if p.is_some() { "text" } else { "default" };
        self.set_cursor_from_name(Some(cursor));
        let label = link.map(|l| match l.target {
            Target::Uri(u) => u,
            Target::File(f) => f,
            Target::Page { page, .. } => format!("Go to page {}", self.page_label(page).unwrap_or_else(|| (page + 1).to_string())),
        });
        if self.sr().hover_link != label {
            self.st().hover_link = label.clone();
            self.emit(Change::Hover(label));
        }
    }

    fn on_press(&self, n: i32, x: f64, y: f64, shift: bool) {
        let p = self.widget_point(x, y, true);
        let on_link = self.widget_point(x, y, false).and_then(|p| self.link_at(&p)).map(|l| l.target);
        self.st().drag.on_link = if n == 1 { on_link } else { None };
        let Some(p) = p else {
            self.clear_selection();
            return;
        };
        self.st().last_click = Some(p);
        match n {
            1 if shift && self.sr().sel.is_some() => {
                self.extend_selection_to(x, y);
                self.selection_done();
            }
            1 => {
                self.st().drag.style = Some(SelStyle::Glyph);
                self.clear_selection();
                let mut st = self.st();
                st.sel = Some(Selection { start: p, end: p, style: SelStyle::Glyph });
            }
            2 | 3 => {
                let style = if n == 2 { SelStyle::Word } else { SelStyle::Line };
                self.st().drag.style = Some(style);
                self.st().sel = Some(Selection { start: p, end: p, style });
                self.update_sel_rects();
                self.selection_done();
            }
            _ => {}
        }
    }

    fn extend_selection_to(&self, x: f64, y: f64) {
        let Some(p) = self.widget_point(x, y, true) else { return };
        {
            let mut st = self.st();
            let style = st.drag.style.unwrap_or(SelStyle::Glyph);
            match &mut st.sel {
                Some(s) => {
                    s.end = p;
                    s.style = style;
                }
                None => st.sel = Some(Selection { start: p, end: p, style }),
            }
        }
        self.update_sel_rects();
    }

    fn start_autoscroll(&self) {
        let weak = self.downgrade();
        let id = self.add_tick_callback(move |_, _| {
            let Some(v) = weak.upgrade() else { return glib::ControlFlow::Break };
            let (x, y) = v.sr().drag.last;
            let (w, h) = (v.sr().view_w, v.sr().view_h);
            let speed = |d: f64| (d / 3.0).clamp(-40.0, 40.0);
            let dy = if y < 0.0 { speed(y) } else if y > h { speed(y - h) } else { 0.0 };
            let dx = if x < 0.0 { speed(x) } else if x > w { speed(x - w) } else { 0.0 };
            if dx != 0.0 || dy != 0.0 {
                let (sx, sy) = v.scroll_pos();
                v.set_scroll(sx + dx, sy + dy);
                v.extend_selection_to(x, y);
            }
            glib::ControlFlow::Continue
        });
        self.st().drag.tick = Some(id);
    }

    /// Pages and point pairs covered by the selection, in reading order.
    fn sel_spans(&self) -> Vec<(usize, (f64, f64), (f64, f64))> {
        let st = self.sr();
        let (Some(sel), Some(doc)) = (st.sel, st.doc.as_ref()) else { return Vec::new() };
        let (a, b) = if (sel.start.page, sel.start.y, sel.start.x) <= (sel.end.page, sel.end.y, sel.end.x) {
            (sel.start, sel.end)
        } else {
            (sel.end, sel.start)
        };
        let mut spans = Vec::new();
        for page in a.page..=b.page {
            let (w, h) = (doc.pages[page].w, doc.pages[page].h);
            let from = if page == a.page { (a.x, a.y) } else { (0.0, 0.0) };
            let to = if page == b.page { (b.x, b.y) } else { (w, h) };
            spans.push((page, from, to));
        }
        spans
    }

    fn update_sel_rects(&self) {
        let style = self.sr().sel.map(|s| s.style).unwrap_or(SelStyle::Glyph);
        let doc = self.doc();
        let mut out = Vec::new();
        if let Some(doc) = doc {
            let single_point = self.sr().sel.map(|s| s.start == s.end).unwrap_or(true);
            if !(single_point && style == SelStyle::Glyph) {
                for (page, a, b) in self.sel_spans() {
                    let rects = doc.selection_rects(page, a, b, style);
                    if !rects.is_empty() {
                        out.push((page, rects));
                    }
                }
            }
        }
        self.st().sel_rects = out;
        self.queue_draw();
    }

    fn selection_done(&self) {
        let text = self.selected_text();
        if !text.is_empty() {
            // X11 / Wayland primary selection: middle-click paste elsewhere.
            self.primary_clipboard().set_text(&text);
        }
        self.emit(Change::Selection);
    }

    pub fn has_selection(&self) -> bool {
        !self.sr().sel_rects.is_empty()
    }

    pub fn clear_selection(&self) -> bool {
        let had = self.has_selection();
        {
            let mut st = self.st();
            st.sel = None;
            st.sel_rects.clear();
        }
        if had {
            self.queue_draw();
            self.emit(Change::Selection);
        }
        had
    }

    pub fn selected_text(&self) -> String {
        let style = self.sr().sel.map(|s| s.style).unwrap_or(SelStyle::Glyph);
        let Some(doc) = self.doc() else { return String::new() };
        if !self.has_selection() {
            return String::new();
        }
        let parts: Vec<String> = self.sel_spans().into_iter().map(|(p, a, b)| doc.selected_text(p, a, b, style)).collect();
        parts.join("\n").trim_end().to_string()
    }

    pub fn select_page(&self) {
        let Some(doc) = self.doc() else { return };
        let page = self.current_page();
        let (w, h) = (doc.pages[page].w, doc.pages[page].h);
        self.st().sel = Some(Selection {
            start: DocPoint { page, x: 0.0, y: 0.0 },
            end: DocPoint { page, x: w, y: h },
            style: SelStyle::Glyph,
        });
        self.update_sel_rects();
        self.selection_done();
    }

    pub fn copy(&self) -> bool {
        let text = self.selected_text();
        if text.is_empty() {
            return false;
        }
        self.clipboard().set_text(&text);
        true
    }

    fn follow(&self, t: Target) {
        match t {
            Target::Page { page, x, y } => {
                let ppp = self.zoom() * PX_PER_PT;
                let y = match y {
                    Some(y) => self.disp_point(&DocPoint { page, x: x.unwrap_or(0.0), y }).1 - 12.0 / ppp,
                    None => -MARGIN / 2.0 / ppp,
                };
                self.go_to_place(Place { page, y }, true);
            }
            Target::Uri(u) => {
                let root = self.root().and_downcast::<gtk::Window>();
                gtk::UriLauncher::new(&u).launch(root.as_ref(), gio_none(), |_| {});
            }
            Target::File(f) => self.emit(Change::OpenFile(f)),
        }
    }

    pub fn go_to_target(&self, t: Target) {
        self.follow(t);
    }

    // ---------------------------------------------------------------- search

    fn cancel_find(&self) {
        self.imp().find_gen.fetch_add(1, Ordering::SeqCst);
    }

    /// Search the whole document on a worker thread, starting at the current page.
    pub fn find(&self, needle: &str, case: bool) {
        self.cancel_find();
        let generation = self.imp().find_gen.load(Ordering::SeqCst);
        let Some(doc) = self.doc() else { return };
        let n = doc.n_pages();
        let start = self.current_page();
        {
            let mut st = self.st();
            st.find = Find { needle: needle.to_string(), case, per_page: vec![Vec::new(); n], start_page: start, ..Find::default() };
            st.find.running = !needle.is_empty();
        }
        self.queue_draw();
        self.emit(Change::Find);
        if needle.is_empty() {
            return;
        }
        let (path, password) = (doc.path.clone(), doc.password.clone());
        let needle = needle.to_string();
        let flag = self.imp().find_gen.clone();
        let (tx, rx) = async_channel::unbounded::<(usize, Vec<Rect>)>();
        std::thread::Builder::new()
            .name("find".into())
            .spawn(move || {
                let Ok(d) = pdf::open_raw(&path, password.as_deref()) else { return };
                for k in 0..n {
                    if flag.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    let page = (start + k) % n;
                    let rects = pdf::find_on_page(&d, page, &needle, case);
                    if tx.send_blocking((page, rects)).is_err() {
                        return;
                    }
                }
            })
            .ok();
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok((page, rects)) = rx.recv().await {
                let Some(v) = weak.upgrade() else { return };
                if v.imp().find_gen.load(Ordering::SeqCst) != generation {
                    return;
                }
                v.on_find_result(page, rects);
            }
        });
    }

    fn on_find_result(&self, page: usize, rects: Vec<Rect>) {
        let first = {
            let mut st = self.st();
            st.find.searched += 1;
            st.find.total += rects.len();
            let n = st.find.per_page.len();
            if st.find.searched >= n {
                st.find.running = false;
                crate::trim_heap();
            }
            let first = st.find.current.is_none() && !rects.is_empty();
            if page < n {
                st.find.per_page[page] = rects;
            }
            if first {
                st.find.current = Some((page, 0));
            }
            first
        };
        if first {
            self.reveal_current_match();
        }
        self.queue_draw();
        self.emit(Change::Find);
    }

    pub fn find_step(&self, dir: i32) {
        let next = {
            let st = self.sr();
            let n = st.find.per_page.len();
            if n == 0 || st.find.total == 0 {
                return;
            }
            let (cp, ci) = st.find.current.unwrap_or((st.find.start_page, usize::MAX));
            let mut page = cp;
            let mut idx = ci as i64;
            let mut result = None;
            for _ in 0..=n {
                let len = st.find.per_page[page].len() as i64;
                let cand = if idx == usize::MAX as i64 { if dir > 0 { 0 } else { len - 1 } } else { idx + dir as i64 };
                if cand >= 0 && cand < len {
                    result = Some((page, cand as usize));
                    break;
                }
                page = if dir > 0 { (page + 1) % n } else { (page + n - 1) % n };
                idx = usize::MAX as i64;
            }
            result
        };
        if let Some(c) = next {
            self.st().find.current = Some(c);
            self.reveal_current_match();
            self.queue_draw();
            self.emit(Change::Find);
        }
    }

    fn reveal_current_match(&self) {
        let cur = self.sr().find.current;
        if let Some((p, i)) = cur {
            let r = self.sr().find.per_page.get(p).and_then(|v| v.get(i)).copied();
            if let Some(r) = r {
                self.reveal(p, &r);
            }
        }
    }

    pub fn clear_find(&self) {
        self.cancel_find();
        self.st().find = Find::default();
        self.queue_draw();
        self.emit(Change::Find);
    }

    /// (current match number, total, still searching)
    pub fn find_status(&self) -> (Option<usize>, usize, bool) {
        let st = self.sr();
        let cur = st.find.current.map(|(p, i)| st.find.per_page[..p].iter().map(|v| v.len()).sum::<usize>() + i + 1);
        (cur, st.find.total, st.find.running)
    }

    // ---------------------------------------------------------------- highlights

    pub fn own_save(&self) -> Option<(std::time::SystemTime, u64)> {
        self.sr().own_save
    }

    pub fn set_highlight_color(&self, rgb: (u8, u8, u8)) {
        self.st().highlight_rgb = rgb;
    }

    /// Highlight the selection, or the current search match when nothing is selected (so
    /// highlighting works from the keyboard: find, then highlight). False when neither exists.
    pub fn highlight_selection(&self) -> bool {
        let Some(doc) = self.doc() else { return false };
        let mut spans = self.sr().sel_rects.clone();
        if spans.is_empty() {
            let st = self.sr();
            if let Some((p, i)) = st.find.current
                && let Some(r) = st.find.per_page.get(p).and_then(|v| v.get(i)) {
                    // Search boxes hug the glyphs; widen a little to cover the line like a selection.
                    spans.push((p, vec![Rect::new(r.x0 - 0.5, r.y0 - 1.0, r.x1 + 0.5, r.y1 + 1.0)]));
                }
        }
        if spans.is_empty() {
            return false;
        }
        let color = self.sr().highlight_rgb;
        let mut edits = Vec::new();
        for (page, rects) in spans {
            if doc.add_highlight(page, &rects, color) {
                edits.push(Edit::Added { page, rects, color });
            }
        }
        if edits.is_empty() {
            return false;
        }
        self.clear_selection();
        {
            let mut st = self.st();
            st.redo.clear();
            st.undo.extend(edits.iter().cloned());
        }
        let pages: Vec<usize> = edits.iter().map(|e| match e { Edit::Added { page, .. } | Edit::Removed { page, .. } => *page }).collect();
        self.annotations_changed(&pages);
        true
    }

    /// Remove highlights under the selection, or under the last click.
    pub fn remove_highlight(&self) -> bool {
        let Some(doc) = self.doc() else { return false };
        let mut areas: Vec<(usize, Rect)> = Vec::new();
        for (p, rects) in self.sr().sel_rects.iter() {
            for r in rects {
                areas.push((*p, *r));
            }
        }
        if areas.is_empty()
            && let Some(c) = self.sr().last_click {
                areas.push((c.page, Rect::new(c.x - 0.5, c.y - 0.5, c.x + 0.5, c.y + 0.5)));
            }
        let mut edits = Vec::new();
        let mut pages = Vec::new();
        for (page, area) in areas {
            let items = doc.remove_highlights(page, area);
            if !items.is_empty() {
                pages.push(page);
                edits.push(Edit::Removed { page, items });
            }
        }
        if edits.is_empty() {
            return false;
        }
        {
            let mut st = self.st();
            st.redo.clear();
            st.undo.extend(edits);
        }
        self.clear_selection();
        self.annotations_changed(&pages);
        true
    }

    fn apply_edit(&self, doc: &Doc, e: &Edit, reverse: bool) -> usize {
        match (e, reverse) {
            (Edit::Added { page, rects, color }, false) => {
                doc.add_highlight(*page, rects, *color);
                *page
            }
            (Edit::Added { page, rects, .. }, true) => {
                for r in rects {
                    doc.remove_highlights(*page, Rect::new(r.x0 + 0.5, r.y0 + 0.5, r.x1 - 0.5, r.y1 - 0.5));
                }
                *page
            }
            (Edit::Removed { page, items }, false) => {
                for (rects, _) in items {
                    for r in rects {
                        doc.remove_highlights(*page, Rect::new(r.x0 + 0.5, r.y0 + 0.5, r.x1 - 0.5, r.y1 - 0.5));
                    }
                }
                *page
            }
            (Edit::Removed { page, items }, true) => {
                for (rects, color) in items {
                    doc.add_highlight(*page, rects, *color);
                }
                *page
            }
        }
    }

    pub fn undo(&self, redo: bool) -> bool {
        let Some(doc) = self.doc() else { return false };
        let e = if redo { self.st().redo.pop() } else { self.st().undo.pop() };
        let Some(e) = e else { return false };
        let page = self.apply_edit(&doc, &e, !redo);
        if redo { self.st().undo.push(e) } else { self.st().redo.push(e) }
        self.annotations_changed(&[page]);
        true
    }

    /// All highlights in the document, for the "go to highlight" picker: (page, rect, text).
    pub fn all_highlights(&self) -> Vec<(usize, Rect, String)> {
        let Some(doc) = self.doc() else { return Vec::new() };
        let mut out = Vec::new();
        for page in 0..doc.n_pages() {
            for (bound, quads) in doc.highlights(page) {
                let mut text = String::new();
                for q in if quads.is_empty() { vec![bound] } else { quads } {
                    let t = doc.selected_text(page, (q.x0 + 0.5, q.y0 + q.h() / 2.0), (q.x1 - 0.5, q.y0 + q.h() / 2.0), SelStyle::Glyph);
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(t.trim());
                }
                out.push((page, bound, text.split_whitespace().collect::<Vec<_>>().join(" ")));
            }
        }
        out
    }

    /// Links on the visible pages: (page, link, the text under it).
    pub fn visible_links(&self) -> Vec<(usize, pdf::Link, String)> {
        let Some(doc) = self.doc() else { return Vec::new() };
        let (sy, vh) = (self.scroll_pos().1, self.sr().view_h);
        let pages: Vec<usize> = self
            .sr()
            .layout
            .boxes
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.filter(|b| b.y + b.h >= sy && b.y <= sy + vh).map(|_| i))
            .collect();
        let mut out = Vec::new();
        for page in pages {
            for l in self.links_on(page).iter() {
                let mid = (l.rect.y0 + l.rect.y1) / 2.0;
                let text = doc.selected_text(page, (l.rect.x0, mid), (l.rect.x1, mid), SelStyle::Glyph);
                out.push((page, l.clone(), text.split_whitespace().collect::<Vec<_>>().join(" ")));
            }
        }
        out
    }

    pub fn reveal_rect(&self, page: usize, r: Rect) {
        self.push_history();
        self.reveal(page, &r);
    }

    /// Save the document and re-render the touched pages.
    fn annotations_changed(&self, pages: &[usize]) {
        let Some(doc) = self.doc() else { return };
        match doc.save_atomic() {
            Ok(()) => {
                let meta = std::fs::metadata(&doc.path).ok();
                self.st().own_save = meta.and_then(|m| Some((m.modified().ok()?, m.len())));
            }
            Err(e) => self.emit(Change::Message(format!("Could not save highlights: {e}"))),
        }
        // The worker re-opens the saved file; keep showing old tiles until new ones arrive.
        let generation = self.renderer().set_doc(Some(doc.path.clone()), doc.password.clone());
        {
            let mut st = self.st();
            let old_gen = st.generation;
            st.generation = generation;
            for t in st.tiles.values_mut() {
                if t.generation == old_gen {
                    t.generation = generation;
                }
            }
            // Tiles rendered from now on count as fresh for those pages; their old tiles stay on
            // screen, marked stale, until the new ones arrive.
            for p in pages {
                st.page_gen.insert(*p as u32, generation);
                let keys: Vec<TileKey> = st.tiles.keys().filter(|k| k.page == *p as u32).copied().collect();
                for k in keys {
                    if let Some(t) = st.tiles.get_mut(&k) {
                        t.generation = generation - 1;
                    }
                }
            }
            st.last_jobs.clear();
        }
        self.queue_draw();
        self.emit(Change::Annotations);
    }
}

fn gio_none() -> Option<&'static gtk::gio::Cancellable> {
    None
}
