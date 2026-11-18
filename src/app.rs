//! Window, bars and the command dispatcher. Every user action goes through `App::run(id)`.

use crate::commands::{self, COMMANDS};
use crate::config::{self, Config};
use crate::icons;
use crate::keymap::{self, Keymap};
use crate::layout::{Fit, Opts};
use crate::pdf::{Doc, OpenError, Target};
use crate::web;
use crate::render::Recolor;
use crate::state::{self, Position};
use crate::style::{self, Palette};
use crate::theme;
use crate::ui::{self, Item, Modal, Toast};
use crate::view::{Change, Colors, DocView, Place};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Top-bar layout; mirrors allwrite's grouped icon buttons.
const LEFT: &[&str] = &["back", "forward"];
const RIGHT: &[(&str, &[&str])] = &[
    ("zoom", &["zoom-out", "#zoom", "zoom-in"]),
    ("view", &["fit-width", "fit-page", "continuous", "dual", "night"]),
    ("tools", &["outline", "find", "highlight"]),
    ("web", &["reload", "open-external"]),
    ("app", &["open", "palette", "settings"]),
];

/// What the window shows. The top bar, palette and commands adapt to it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Empty,
    Pdf,
    Html,
}

/// Commands that only make sense for a PDF (hidden from the top bar and palette for web pages).
const PDF_ONLY: &[&str] = &[
    "goto-page", "outline", "next-page", "prev-page", "first-page", "last-page", "links", "scroll-down", "scroll-up",
    "scroll-left", "scroll-right", "screen-down", "screen-up", "fit-width", "fit-page", "continuous", "dual", "cover",
    "rotate-right", "rotate-left", "presentation", "highlight", "unhighlight", "undo", "redo", "highlights",
];
/// Commands that only make sense for a web page.
const HTML_ONLY: &[&str] = &["open-external"];

fn applies(id: &str, kind: Kind) -> bool {
    match kind {
        Kind::Html => !PDF_ONLY.contains(&id),
        _ => !HTML_ONLY.contains(&id),
    }
}

const WEB_ZOOMS: &[f64] = &[0.3, 0.5, 0.67, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0];

/// Theme slots offered as highlight colors (settings and the highlight button's menu).
/// Names come from the actual color (`style::color_name`): pywal slots follow the wallpaper.
const HL_COLORS: &[&str] = &["color1", "color2", "color3", "color4", "color5", "color6", "accent"];

pub struct App {
    pub window: gtk::ApplicationWindow,
    topbar: gtk::WindowHandle,
    bar: gtk::Box,
    bar_clip: gtk::ScrolledWindow,
    responsive_pending: Cell<bool>,
    statusbar: gtk::Box,
    view: DocView,
    stack: gtk::Stack,
    filename: gtk::Label,
    page_entry: gtk::Entry,
    page_total: gtk::Label,
    pagebox: gtk::Box,
    zoom_btn: gtk::Button,
    buttons: HashMap<&'static str, gtk::Button>,
    groups: Vec<(&'static str, gtk::Box)>,
    status_left: gtk::Label,
    status_right: gtk::Label,
    findbar: gtk::Revealer,
    find_entry: gtk::Entry,
    find_case: gtk::ToggleButton,
    find_count: gtk::Label,
    empty: gtk::Box,
    modal: Rc<Modal>,
    toast: Rc<Toast>,
    css: gtk::CssProvider,
    config: RefCell<Config>,
    state: RefCell<state::State>,
    keymap: RefCell<Keymap>,
    palette: RefCell<Palette>,
    portal: &'static std::sync::Mutex<theme::PortalState>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    reload_timer: RefCell<Option<glib::SourceId>>,
    hover: RefCell<Option<String>>,
    focus_mode: Cell<bool>,
    find_timer: RefCell<Option<glib::SourceId>>,
    /// View settings to restore when presentation mode ends.
    presenting: RefCell<Option<(Opts, Fit, f64)>>,
    kind: Cell<Kind>,
    /// The WebKit view, while a web page is shown. Dropped (and its process with it) when a
    /// PDF is opened again.
    web: RefCell<Option<Rc<web::Web>>>,
    /// The HTML file being shown (follows local links).
    web_path: RefCell<Option<PathBuf>>,
    /// Web find: matches counted, -1 while unknown.
    web_matches: Cell<i64>,
}

static PORTAL: std::sync::Mutex<theme::PortalState> =
    std::sync::Mutex::new(theme::PortalState { scheme: None, accent: None });

fn icon_button(id: &'static str) -> gtk::Button {
    let cmd = commands::get(id).expect("command");
    let b = gtk::Button::new();
    b.add_css_class("icon-btn");
    b.set_child(Some(&icons::icon(cmd.icon.unwrap_or("file"), 17)));
    b.set_focus_on_click(false);
    b.set_can_focus(false);
    b
}

fn home_short(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    if let Some(home) = dirs::home_dir() {
        let h = home.to_string_lossy().to_string();
        if let Some(rest) = s.strip_prefix(&h) {
            return format!("~{rest}");
        }
    }
    s
}

impl App {
    pub fn new(gapp: &gtk::Application) -> Rc<App> {
        let config = config::load();
        let window = gtk::ApplicationWindow::new(gapp);
        window.add_css_class("docview");
        window.set_title(Some("docview"));
        window.set_icon_name(Some("docview"));
        window.set_default_size(1000, 800);
        window.set_decorated(false);

        // ---- top bar
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("topbar");
        let mut buttons = HashMap::new();
        let left = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        left.add_css_class("group");
        for id in LEFT {
            let b = icon_button(id);
            left.append(&b);
            buttons.insert(*id, b);
        }
        bar.append(&left);
        let filename = gtk::Label::new(None);
        filename.add_css_class("filename");
        filename.set_ellipsize(gtk::pango::EllipsizeMode::End);
        filename.set_xalign(0.0);
        filename.set_hexpand(true);
        filename.set_max_width_chars(1);
        filename.set_margin_start(4);
        bar.append(&filename);

        let pagebox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pagebox.add_css_class("pagebox");
        pagebox.add_css_class("group");
        pagebox.set_valign(gtk::Align::Center);
        let page_entry = gtk::Entry::new();
        page_entry.add_css_class("page-entry");
        page_entry.set_width_chars(4);
        page_entry.set_max_width_chars(6);
        gtk::prelude::EntryExt::set_alignment(&page_entry, 0.5);
        page_entry.set_valign(gtk::Align::Center);
        let page_total = gtk::Label::new(Some("/ 0"));
        pagebox.append(&page_entry);
        pagebox.append(&page_total);
        bar.append(&pagebox);

        let zoom_btn = gtk::Button::with_label("100%");
        zoom_btn.add_css_class("zoom-btn");
        zoom_btn.set_focus_on_click(false);
        zoom_btn.set_can_focus(false);
        let mut groups = Vec::new();
        for (group, ids) in RIGHT {
            let g = gtk::Box::new(gtk::Orientation::Horizontal, 3);
            g.add_css_class("group");
            g.set_valign(gtk::Align::Center);
            for id in *ids {
                if *id == "#zoom" {
                    g.append(&zoom_btn);
                    continue;
                }
                let b = icon_button(id);
                g.append(&b);
                buttons.insert(*id, b);
            }
            bar.append(&g);
            groups.push((*group, g));
        }
        // The bar never sets the window's minimum width: a window that asks for more than a
        // tiling compositor gives it waits for a resize that never comes, and GTK stops
        // drawing. Groups that do not fit are hidden instead (`responsive`).
        let bar_clip = gtk::ScrolledWindow::new();
        bar_clip.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
        bar_clip.set_propagate_natural_height(true);
        bar_clip.set_child(Some(&bar));
        let handle = gtk::WindowHandle::new();
        handle.set_child(Some(&bar_clip));

        // ---- document area
        let view = DocView::new();
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&view));
        scroller.set_vexpand(true);
        scroller.set_hexpand(true);
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 6);
        empty.add_css_class("empty");
        empty.set_halign(gtk::Align::Center);
        empty.set_valign(gtk::Align::Center);
        let stack = gtk::Stack::new();
        // Scrollable, so a long recent-files list never sets the window's minimum size.
        let empty_scroll = gtk::ScrolledWindow::new();
        empty_scroll.set_child(Some(&empty));
        stack.add_named(&empty_scroll, Some("empty"));
        stack.add_named(&scroller, Some("doc"));
        stack.set_vexpand(true);
        stack.set_hhomogeneous(false);
        stack.set_vhomogeneous(false);

        // ---- find bar
        let fb = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fb.add_css_class("findbar");
        let find_entry = gtk::Entry::new();
        find_entry.set_placeholder_text(Some("Find"));
        let find_case = gtk::ToggleButton::new();
        find_case.set_child(Some(&icons::icon("case", 17)));
        find_case.add_css_class("icon-btn");
        find_case.set_tooltip_text(Some("Match case"));
        find_case.set_focus_on_click(false);
        let prev = gtk::Button::new();
        prev.set_child(Some(&icons::icon("up", 17)));
        prev.add_css_class("icon-btn");
        prev.set_focus_on_click(false);
        let next = gtk::Button::new();
        next.set_child(Some(&icons::icon("down", 17)));
        next.add_css_class("icon-btn");
        next.set_focus_on_click(false);
        let find_count = gtk::Label::new(None);
        find_count.add_css_class("count");
        find_count.set_hexpand(true);
        find_count.set_xalign(0.0);
        let close = gtk::Button::new();
        close.set_child(Some(&icons::icon("close", 15)));
        close.add_css_class("icon-btn");
        close.set_focus_on_click(false);
        for w in [find_entry.upcast_ref::<gtk::Widget>(), find_case.upcast_ref(), prev.upcast_ref(), next.upcast_ref(), find_count.upcast_ref(), close.upcast_ref()] {
            fb.append(w);
        }
        let findbar = gtk::Revealer::new();
        findbar.set_child(Some(&fb));
        findbar.set_transition_type(gtk::RevealerTransitionType::None);
        // Hidden, not just unrevealed: an unrevealed Revealer without a transition keeps its height.
        findbar.set_visible(false);

        // ---- status bar
        let status = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status.add_css_class("statusbar");
        let status_left = gtk::Label::new(None);
        status_left.set_hexpand(true);
        status_left.set_xalign(0.0);
        status_left.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let status_right = gtk::Label::new(None);
        status.append(&status_left);
        status.append(&status_right);

        let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        col.append(&handle);
        col.append(&stack);
        col.append(&findbar);
        col.append(&status);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&col));
        let modal = Modal::new();
        let toast = Toast::new();
        overlay.add_overlay(&toast.label);
        overlay.add_overlay(&modal.scrim);
        window.set_child(Some(&overlay));
        modal.return_focus.replace(Some(view.clone().upcast()));

        let css = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(&gtk::prelude::WidgetExt::display(&window), &css, gtk::STYLE_PROVIDER_PRIORITY_USER);

        let keymap = Keymap::new(&config.keys);
        let app = Rc::new(App {
            window,
            topbar: handle,
            bar,
            bar_clip,
            responsive_pending: Cell::new(false),
            statusbar: status,
            view,
            stack,
            filename,
            page_entry,
            page_total,
            pagebox,
            zoom_btn,
            buttons,
            groups,
            status_left,
            status_right,
            findbar,
            find_entry,
            find_case,
            find_count,
            empty,
            modal,
            toast,
            css,
            config: RefCell::new(config),
            state: RefCell::new(state::load()),
            keymap: RefCell::new(keymap),
            palette: RefCell::new(Palette::from_theme(&theme::current("system", Default::default(), &Default::default()))),
            portal: &PORTAL,
            monitors: RefCell::new(Vec::new()),
            reload_timer: RefCell::new(None),
            hover: RefCell::new(None),
            focus_mode: Cell::new(false),
            find_timer: RefCell::new(None),
            presenting: RefCell::new(None),
            kind: Cell::new(Kind::Empty),
            web: RefCell::new(None),
            web_path: RefCell::new(None),
            web_matches: Cell::new(-1),
        });
        app.wire(&prev, &next, &close);
        app.apply_theme();
        app.watch_theme();
        app.apply_view_config();
        app.refresh_tooltips();
        app.build_empty();
        app.apply_bars();
        app.apply_kind();
        app.refresh();
        app
    }

    // ---------------------------------------------------------------- wiring

    fn wire(self: &Rc<Self>, prev: &gtk::Button, next: &gtk::Button, close: &gtk::Button) {
        for (id, b) in &self.buttons {
            let (w, id) = (Rc::downgrade(self), *id);
            b.connect_clicked(move |_| {
                if let Some(a) = w.upgrade() {
                    a.run(id);
                }
            });
        }
        // Right-click on the highlight button: pick the color (left click uses the last one).
        if let Some(b) = self.buttons.get("highlight") {
            let w = Rc::downgrade(self);
            let click = gtk::GestureClick::new();
            click.set_button(gdk::BUTTON_SECONDARY);
            click.connect_pressed(move |g, _, _, _| {
                g.set_state(gtk::EventSequenceState::Claimed);
                if let (Some(a), Some(b)) = (w.upgrade(), g.widget()) {
                    a.highlight_menu(&b);
                }
            });
            b.add_controller(click);
            b.set_tooltip_text(Some("Highlight (right-click for color)"));
        }
        {
            let w = Rc::downgrade(self);
            self.zoom_btn.connect_clicked(move |_| {
                if let Some(a) = w.upgrade() {
                    a.run("zoom-pick");
                }
            });
        }
        // View -> bars
        {
            let w = Rc::downgrade(self);
            self.view.connect_change(move |c| {
                if let Some(a) = w.upgrade() {
                    a.on_view_change(c);
                }
            });
        }
        // Page entry
        {
            let w = Rc::downgrade(self);
            self.page_entry.connect_activate(move |e| {
                let Some(a) = w.upgrade() else { return };
                let target = a.view.doc().and_then(|d| d.find_page(&e.text()));
                match target {
                    Some(p) => a.view.go_to_page(p),
                    None => a.toast.show("No such page"),
                }
                a.view.grab_focus();
                a.refresh();
            });
        }
        {
            let w = Rc::downgrade(self);
            let fc = gtk::EventControllerFocus::new();
            fc.connect_leave(move |_| {
                if let Some(a) = w.upgrade() {
                    a.refresh();
                }
            });
            self.page_entry.add_controller(fc);
        }
        // Find bar
        {
            let w = Rc::downgrade(self);
            self.find_entry.connect_changed(move |_| {
                let Some(a) = w.upgrade() else { return };
                // Searching is on a worker thread, but debounce typing a little anyway.
                if let Some(t) = a.find_timer.take() {
                    t.remove();
                }
                let w2 = Rc::downgrade(&a);
                let id = glib::timeout_add_local_once(std::time::Duration::from_millis(180), move || {
                    if let Some(a) = w2.upgrade() {
                        a.find_timer.take();
                        a.start_find();
                    }
                });
                a.find_timer.replace(Some(id));
            });
        }
        {
            let w = Rc::downgrade(self);
            self.find_entry.connect_activate(move |_| {
                let Some(a) = w.upgrade() else { return };
                if a.find_timer.borrow().is_some() {
                    if let Some(t) = a.find_timer.take() {
                        t.remove();
                    }
                    a.start_find();
                } else {
                    a.find_step(1);
                }
            });
        }
        {
            let w = Rc::downgrade(self);
            self.find_case.connect_toggled(move |_| {
                if let Some(a) = w.upgrade() {
                    a.start_find();
                }
            });
        }
        for (b, id) in [(prev, "find-prev"), (next, "find-next"), (close, "escape")] {
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(a) = w.upgrade() {
                    if id == "escape" {
                        a.close_find();
                    } else {
                        a.run(id);
                    }
                }
            });
        }
        // Keys: one handler in the capture phase, like allwrite's global keydown.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let w = Rc::downgrade(self);
            keys.connect_key_pressed(move |c, kv, code, state| {
                let Some(a) = w.upgrade() else { return glib::Propagation::Proceed };
                a.on_key(c, kv, code, state)
            });
        }
        self.window.add_controller(keys);
        if crate::debug() {
            let c = gtk::GestureClick::new();
            c.set_button(0);
            c.set_propagation_phase(gtk::PropagationPhase::Capture);
            let win = self.window.clone();
            c.connect_pressed(move |_, n, x, y| {
                let target = win.pick(x, y, gtk::PickFlags::DEFAULT).map(|w| w.type_().name().to_string());
                eprintln!("press n={n} at {x:.0},{y:.0} on {target:?}");
            });
            self.window.add_controller(c);
        }
        // Drag and drop files onto the window.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        {
            let w = Rc::downgrade(self);
            drop.connect_drop(move |_, value, _, _| {
                let Some(a) = w.upgrade() else { return false };
                let Ok(list) = value.get::<gdk::FileList>() else { return false };
                if let Some(path) = list.files().first().and_then(|f| f.path()) {
                    a.open_path(&path, None);
                    return true;
                }
                false
            });
        }
        self.window.add_controller(drop);
        // Remember the position on close.
        {
            let w = Rc::downgrade(self);
            self.window.connect_close_request(move |_| {
                if let Some(a) = w.upgrade() {
                    a.save_position();
                }
                glib::Propagation::Proceed
            });
        }
        // Narrow windows drop button groups, like allwrite's media queries. The bar's
        // viewport width changes on every resize; act on it after layout, never during.
        {
            let w = Rc::downgrade(self);
            self.bar_clip.hadjustment().connect_changed(move |_| {
                if let Some(a) = w.upgrade() {
                    a.queue_responsive();
                }
            });
        }
    }

    fn queue_responsive(self: &Rc<Self>) {
        if self.responsive_pending.replace(true) {
            return;
        }
        let w = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(a) = w.upgrade() {
                a.responsive_pending.set(false);
                a.responsive();
            }
        });
    }

    /// Show as many button groups as fit, dropping the least needed first.
    fn responsive(&self) {
        let avail = self.bar_clip.width();
        if avail <= 0 {
            return;
        }
        let html = self.is_web();
        // Least needed first; the bool says whether it belongs to this kind of document.
        let order: [(gtk::Widget, bool); 6] = [
            (self.group("view").clone().upcast(), true),
            (self.group("web").clone().upcast(), html),
            (self.group("tools").clone().upcast(), true),
            (self.group("zoom").clone().upcast(), true),
            (self.pagebox.clone().upcast(), !html),
            (self.buttons["forward"].clone().upcast(), true),
        ];
        for (w, ok) in &order {
            w.set_visible(*ok);
        }
        for (w, ok) in &order {
            // The filename is ellipsized; leave it room for a few characters.
            if self.bar.measure(gtk::Orientation::Horizontal, -1).1 + 60 <= avail {
                break;
            }
            if *ok {
                w.set_visible(false);
            }
        }
    }

    fn group(&self, name: &str) -> &gtk::Box {
        &self.groups.iter().find(|(n, _)| *n == name).expect("group").1
    }

    /// Top bar and status bar visibility from focus mode and the `topbar` setting.
    fn apply_bars(&self) {
        let focus = self.focus_mode.get();
        self.topbar.set_visible(!focus && self.config.borrow().app.topbar);
        self.statusbar.set_visible(!focus);
    }

    fn on_key(self: &Rc<Self>, c: &gtk::EventControllerKey, kv: gdk::Key, code: u32, state: gdk::ModifierType) -> glib::Propagation {
        let unshifted = gtk::prelude::WidgetExt::display(&self.window)
            .map_keycode(code)
            .and_then(|v| v.into_iter().find(|(k, _)| k.group() == c.group() as i32 && k.level() == 0).map(|(_, kv)| kv));
        let Some(combo) = keymap::combo_from_event(kv, unshifted, state) else { return glib::Propagation::Proceed };
        if crate::debug() {
            eprintln!("key {combo} -> {:?}", self.keymap.borrow().lookup(&combo));
        }
        if self.modal.is_open() {
            if combo == "Escape" {
                self.modal.close();
                return glib::Propagation::Stop;
            }
            return glib::Propagation::Proceed;
        }
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window);
        let in_text = focus.as_ref().map(|f| f.is::<gtk::Text>() || f.is::<gtk::Editable>()).unwrap_or(false);
        if in_text {
            let in_find = focus.as_ref().map(|f| f.is_ancestor(&self.find_entry)).unwrap_or(false);
            if combo == "Escape" {
                if in_find {
                    self.close_find();
                } else {
                    self.view.grab_focus();
                    self.refresh();
                }
                return glib::Propagation::Stop;
            }
            if in_find && (combo == "Shift+Enter" || combo == "Down" || combo == "Up") {
                self.find_step(if combo == "Shift+Enter" || combo == "Up" { -1 } else { 1 });
                return glib::Propagation::Stop;
            }
            // Plain typing and the usual text editing keys stay with the entry.
            let editing = [
                "Ctrl+C", "Ctrl+V", "Ctrl+X", "Ctrl+A", "Ctrl+Z", "Ctrl+Shift+Z", "Ctrl+Y", "Ctrl+Left", "Ctrl+Right",
                "Ctrl+Backspace", "Ctrl+Delete", "Ctrl+Shift+Left", "Ctrl+Shift+Right",
            ];
            let plain = !combo.starts_with("Ctrl+") && !combo.starts_with("Alt+") && !(combo.starts_with('F') && combo.len() <= 3);
            if plain || editing.contains(&combo.as_str()) {
                return glib::Propagation::Proceed;
            }
        }
        if self.is_web() {
            // A web page gets typing, scrolling and editing keys, like in a browser; docview
            // keeps its Ctrl / Alt / F-key commands.
            let plain = !combo.starts_with("Ctrl+") && !combo.starts_with("Alt+") && !(combo.starts_with('F') && combo.len() <= 3);
            let editing = ["Ctrl+C", "Ctrl+V", "Ctrl+X", "Ctrl+A", "Ctrl+Z", "Ctrl+Shift+Z", "Ctrl+Y"];
            let ours = combo == "Escape" && (self.findbar.is_visible() || self.focus_mode.get() || self.window.is_fullscreen());
            if (plain && !ours) || editing.contains(&combo.as_str()) {
                return glib::Propagation::Proceed;
            }
        }
        let id = self.keymap.borrow().lookup(&combo);
        match id {
            Some(id) => {
                self.run(id);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    }

    // ---------------------------------------------------------------- theme

    fn apply_theme(&self) {
        let theme = {
            let c = self.config.borrow();
            theme::current(&c.app.mode, *self.portal.lock().unwrap(), &c.theme)
        };
        let pal = Palette::from_theme(&theme);
        self.css.load_from_string(&pal.css(&self.config.borrow().app.font));
        self.palette.replace(pal);
        self.apply_view_colors();
    }

    fn apply_view_colors(&self) {
        let pal = self.palette.borrow().clone();
        let f = |c: style::Rgb, a: f32| gdk::RGBA::new(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, a);
        let night = self.config.borrow().view.night;
        let colors = Colors {
            bg: f(pal.bg, 1.0),
            page: f([255, 255, 255], 1.0),
            border: f(pal.border, 1.0),
            selection: f(pal.accent, 0.3),
            find: f(pal.c[3], 0.4),
            find_current: f(pal.accent, 0.45),
        };
        if let Some(w) = self.web() {
            w.set_night(night);
        }
        let recolor = night.then_some(Recolor { bg: pal.bg, fg: pal.fg });
        self.view.set_colors(colors, recolor);
        let hl = self.config.borrow().app.highlight.clone();
        let rgb = pal.resolve(&hl).unwrap_or(pal.c[3]);
        self.view.set_highlight_color((rgb[0], rgb[1], rgb[2]));
    }

    fn watch_theme(self: &Rc<Self>) {
        // pywal: follow colors.json (wal rewrites it on every wallpaper change).
        if let Some(dir) = theme::wal_dir() {
            let file = gio::File::for_path(&dir);
            if let Ok(m) = file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
                let w = Rc::downgrade(self);
                m.connect_changed(move |_, f, other, ev| {
                    let hit = |f: Option<&gio::File>| f.and_then(|f| f.basename()).map(|n| n == Path::new("colors.json")).unwrap_or(false);
                    if !(hit(Some(f)) || hit(other)) {
                        return;
                    }
                    if matches!(ev, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created | gio::FileMonitorEvent::MovedIn | gio::FileMonitorEvent::Renamed)
                        && let Some(a) = w.upgrade() {
                            a.apply_theme();
                        }
                });
                self.monitors.borrow_mut().push(m);
            }
        }
        // XDG portal (dark / light, accent) only matters without pywal.
        if theme::wal_file().map(|p| !p.exists()).unwrap_or(true) {
            let w = Rc::downgrade(self);
            let ctx = glib::MainContext::default();
            theme::init_portal(self.portal, move || {
                let w = w.clone();
                ctx.invoke_local(move || {
                    if let Some(a) = w.upgrade() {
                        a.apply_theme();
                    }
                });
            });
            self.apply_theme();
        }
    }

    fn apply_view_config(&self) {
        let c = self.config.borrow().clone();
        self.view.set_cache_limit(c.app.cache_mb);
        self.view.set_opts(Opts { continuous: c.view.continuous, dual: c.view.dual, cover: c.view.cover });
        match Fit::parse(&c.view.fit) {
            Fit::None => self.view.set_zoom(c.view.zoom, None),
            f => self.view.set_fit(f),
        }
    }

    fn save_config(&self) {
        if let Err(e) = config::save(&self.config.borrow()) {
            self.toast.show(&format!("Could not save settings: {e}"));
        }
    }

    /// Dev hook: pointer gesture on the document view (view coordinates).
    pub fn simulate(&self, n: i32, from: (f64, f64), to: Option<(f64, f64)>) {
        self.view.simulate(n, from, to);
        self.refresh();
    }

    /// Top-bar buttons that show as active, for the dev hooks.
    /// Window size vs. the minimum its content asks for (a minimum larger than the tile the
    /// compositor gives the window stalls drawing), for DOCVIEW_DEBUG.
    pub fn debug_sizes(&self) -> String {
        let (mw, _) = (self.window.measure(gtk::Orientation::Horizontal, -1).0, ());
        let mh = self.window.measure(gtk::Orientation::Vertical, -1).0;
        let bar = self.bar.measure(gtk::Orientation::Horizontal, -1);
        let hidden: Vec<&str> = self.groups.iter().filter(|(_, g)| !g.is_visible()).map(|(n, _)| *n).collect();
        format!(
            "window {}x{} min {mw}x{mh}; bar min/nat {}/{} in {}; hidden groups {hidden:?}",
            self.window.width(),
            self.window.height(),
            bar.0,
            bar.1,
            self.bar_clip.width()
        ) + &format!(
            "; heights: top {} stack {} find {} status {}",
            self.topbar.height(),
            self.stack.height(),
            self.findbar.height(),
            self.statusbar.height()
        )
    }

    pub fn active_buttons(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.buttons.iter().filter(|(_, b)| b.has_css_class("active")).map(|(id, _)| *id).collect();
        v.sort();
        v
    }

    /// Theme background, for the snapshot dev hook.
    pub fn window_bg(&self) -> Option<gdk::RGBA> {
        let c = self.palette.borrow().bg;
        Some(gdk::RGBA::new(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0))
    }

    // ---------------------------------------------------------------- documents

    pub fn open_path(self: &Rc<Self>, path: &Path, password: Option<String>) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if web::is_html(&path) {
            self.open_html(&path);
            return;
        }
        match Doc::open(&path, password.as_deref()) {
            Ok(doc) => {
                self.save_position();
                self.drop_web();
                self.set_kind(Kind::Pdf);
                let pos = if self.config.borrow().app.restore_position { self.state.borrow().position(&path).cloned() } else { None };
                if let Some(p) = &pos {
                    let opts = Opts { continuous: p.continuous, dual: p.dual, cover: self.view.opts().cover };
                    self.view.set_opts(opts);
                    match (Fit::parse(&p.fit), p.zoom) {
                        (Fit::None, Some(z)) => {
                            self.view.set_fit(Fit::None);
                            self.view.set_zoom(z, None);
                        }
                        (f, _) => self.view.set_fit(if f == Fit::None { Fit::Width } else { f }),
                    }
                } else {
                    // New document: back to the configured defaults.
                    self.apply_view_config();
                }
                let rotation = pos.as_ref().map(|p| p.rotation).unwrap_or(0);
                let place = pos.map(|p| Place { page: p.page, y: p.offset });
                self.view.set_doc(Some(doc), place);
                let delta = rotation as i32 - self.view.rotation() as i32;
                if delta != 0 {
                    self.view.rotate(delta);
                }
                self.state.borrow_mut().touch_recent(&path);
                self.state.borrow().save();
                self.stack.set_visible_child_name("doc");
                self.watch_file(&path);
                self.view.grab_focus();
                if self.findbar.reveals_child() && !self.find_entry.text().is_empty() {
                    self.start_find();
                }
                self.refresh();
            }
            Err(OpenError::NeedsPassword) | Err(OpenError::BadPassword) => {
                let wrong = password.is_some();
                let w = Rc::downgrade(self);
                let p = path.clone();
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let msg = if wrong { format!("Wrong password for {name}. Try again:") } else { format!("{name} is password protected.") };
                ui::prompt(
                    &self.modal,
                    &msg,
                    true,
                    Rc::new(move |pw| {
                        if let Some(a) = w.upgrade() {
                            a.open_path(&p, Some(pw));
                        }
                    }),
                );
            }
            Err(e) => self.toast.show(&format!("Could not open {}: {e}", home_short(&path))),
        }
    }

    // ---------------------------------------------------------------- web pages

    fn is_web(&self) -> bool {
        self.kind.get() == Kind::Html
    }

    fn web(&self) -> Option<Rc<web::Web>> {
        self.web.borrow().clone()
    }

    /// The file on screen, PDF or HTML.
    fn current_path(&self) -> Option<PathBuf> {
        match self.kind.get() {
            Kind::Html => self.web_path.borrow().clone(),
            _ => self.view.doc().map(|d| d.path.clone()),
        }
    }

    fn set_kind(self: &Rc<Self>, kind: Kind) {
        if self.kind.replace(kind) != kind {
            self.apply_kind();
            self.queue_responsive();
        }
    }

    /// Show the top-bar buttons that apply to what is on screen.
    fn apply_kind(&self) {
        let kind = self.kind.get();
        for (id, b) in &self.buttons {
            b.set_visible(applies(id, kind));
        }
        let html = kind == Kind::Html;
        self.group("web").set_visible(html);
        self.pagebox.set_visible(!html);
        let focus: gtk::Widget = match self.web() {
            Some(w) if html => w.widget.clone(),
            _ => self.view.clone().upcast(),
        };
        self.modal.return_focus.replace(Some(focus));
    }

    fn set_zoom(&self, z: f64) {
        match self.web() {
            Some(w) => w.set_zoom(z),
            None => self.view.set_zoom(z, None),
        }
        self.refresh();
    }

    /// Create the web view on first use. None when WebKitGTK is not installed.
    fn ensure_web(self: &Rc<Self>) -> Option<Rc<web::Web>> {
        if let Some(w) = self.web() {
            return Some(w);
        }
        let w = Rc::new(web::Web::new()?);
        self.stack.add_named(&w.widget, Some("web"));
        let a = Rc::downgrade(self);
        w.connect_changed(move || {
            if let Some(a) = a.upgrade() {
                a.on_web_change();
            }
        });
        let a = Rc::downgrade(self);
        w.connect_find(move |n| {
            if let Some(a) = a.upgrade() {
                a.web_matches.set(n.map(i64::from).unwrap_or(0));
                a.refresh_find();
            }
        });
        let a = Rc::downgrade(self);
        w.connect_hover(move |link| {
            if let Some(a) = a.upgrade() {
                a.hover.replace(link);
                a.refresh_status();
            }
        });
        // A link to a local PDF opens in a docview window, like a PDF link to another file.
        let a = Rc::downgrade(self);
        w.connect_links(move |uri| {
            let Some(a) = a.upgrade() else { return false };
            let Some(p) = uri.starts_with("file:").then(|| gio::File::for_uri(uri).path()).flatten() else { return false };
            if web::is_html(&p) || !p.is_file() {
                return false;
            }
            a.on_view_change(Change::OpenFile(p.to_string_lossy().to_string()));
            true
        }, {
            // Web links never load in docview: the browser opens them.
            let a = Rc::downgrade(self);
            move |uri: &str| {
                if let Some(a) = a.upgrade() {
                    crate::launch_uri(&a.window, uri);
                    a.toast.show("Opened in your browser");
                }
            }
        });
        // Ctrl + wheel zooms, as on a PDF.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let a = Rc::downgrade(self);
        scroll.connect_scroll(move |c, _, dy| {
            let Some(a) = a.upgrade() else { return glib::Propagation::Proceed };
            if !c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) || dy == 0.0 {
                return glib::Propagation::Proceed;
            }
            a.run(if dy < 0.0 { "zoom-in" } else { "zoom-out" });
            glib::Propagation::Stop
        });
        w.widget.add_controller(scroll);
        w.set_night(self.config.borrow().view.night);
        self.web.replace(Some(w.clone()));
        Some(w)
    }

    /// Back to PDFs: destroy the web view, which ends its web process.
    fn drop_web(&self) {
        if let Some(w) = self.web.take() {
            self.stack.remove(&w.widget);
            self.web_path.replace(None);
            self.hover.replace(None);
        }
    }

    fn open_html(self: &Rc<Self>, path: &Path) {
        let Some(w) = self.ensure_web() else {
            self.toast.show("Web pages need WebKitGTK 6 (package webkitgtk-6.0 or libwebkitgtk-6.0-4)");
            return;
        };
        self.save_position();
        self.view.set_doc(None, None);
        if self.findbar.is_visible() {
            self.close_find();
        }
        self.web_path.replace(Some(path.to_path_buf()));
        w.load_file(path);
        {
            let mut st = self.state.borrow_mut();
            st.touch_recent(path);
            st.save();
        }
        self.stack.set_visible_child_name("web");
        self.set_kind(Kind::Html);
        self.apply_kind();
        self.watch_file(path);
        w.widget.grab_focus();
        self.refresh();
    }

    fn on_web_change(self: &Rc<Self>) {
        let Some(w) = self.web() else { return };
        // Following a link to another local page: watch that file instead.
        if let Some(p) = w.path()
            && self.web_path.borrow().as_ref() != Some(&p)
            && p.is_file() {
                self.web_path.replace(Some(p.clone()));
                self.watch_file(&p);
            }
        self.refresh();
    }

    fn refresh_web(&self) {
        let Some(w) = self.web() else { return };
        let path = self.web_path.borrow().clone();
        let file = path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string());
        let name = w.title().or(file).unwrap_or_else(|| w.uri().unwrap_or_default());
        self.filename.set_text(&name);
        let tip = path.as_deref().map(home_short).or_else(|| w.uri());
        self.filename.set_tooltip_text(tip.as_deref());
        self.window.set_title(Some(&format!("{name} - docview")));
        self.zoom_btn.set_label(&format!("{}%", (w.zoom() * 100.0).round()));
        self.zoom_btn.set_sensitive(true);
        for (id, b) in &self.buttons {
            let on = match *id {
                "night" => self.config.borrow().view.night,
                "find" => self.findbar.reveals_child(),
                _ => false,
            };
            if on { b.add_css_class("active") } else { b.remove_css_class("active") }
            b.set_sensitive(match *id {
                "back" => w.can_go_back(),
                "forward" => w.can_go_forward(),
                _ => true,
            });
        }
        self.refresh_status();
    }

    /// Commands for a web page. False: not handled here, so `run` does the usual thing
    /// (window, theme and app commands are the same for both kinds).
    fn run_web(self: &Rc<Self>, id: &str) -> bool {
        let Some(w) = self.web() else { return false };
        match id {
            "reload" => self.reload(true),
            "open-external" => {
                if let Some(uri) = w.uri() {
                    crate::launch_uri(&self.window, &uri);
                }
            }
            "print" => w.print(self.window.upcast_ref()),
            "copy-path" => {
                let text = self.web_path.borrow().as_ref().map(|p| p.to_string_lossy().to_string()).or_else(|| w.uri());
                if let Some(t) = text {
                    self.view.clipboard().set_text(&t);
                    self.toast.show("Path copied");
                }
            }
            "properties" => {
                let mut lines = Vec::new();
                if let Some(t) = w.title() {
                    lines.push(format!("Title:  {t}"));
                }
                if let Some(p) = self.web_path.borrow().as_ref() {
                    if let Ok(m) = std::fs::metadata(p) {
                        lines.push(format!("File size:  {:.1} KB", m.len() as f64 / 1024.0));
                    }
                    lines.push(format!("Location:  {}", home_short(p)));
                } else if let Some(u) = w.uri() {
                    lines.push(format!("Address:  {u}"));
                }
                ui::ask(&self.modal, &lines.join("\n"), vec![("Close", true, Rc::new(|| {}))]);
            }
            "back" => w.back(),
            "forward" => w.forward(),
            "zoom-in" | "zoom-out" => {
                let z = w.zoom();
                let next = if id == "zoom-in" {
                    WEB_ZOOMS.iter().copied().find(|s| *s > z + 0.001).unwrap_or(5.0)
                } else {
                    WEB_ZOOMS.iter().rev().copied().find(|s| *s < z - 0.001).unwrap_or(0.3)
                };
                w.set_zoom(next);
            }
            "zoom-reset" => w.set_zoom(1.0),
            "zoom-pick" => self.pick_zoom(),
            "find" => self.open_find(),
            "find-next" => {
                if self.findbar.is_visible() { w.find_step(1) } else { self.open_find() }
            }
            "find-prev" => w.find_step(-1),
            "copy" => w.editing("Copy"),
            "select-all" => w.editing("SelectAll"),
            id if !applies(id, Kind::Html) => {
                let label = commands::get(id).map(|c| c.label.trim_end_matches('…')).unwrap_or(id);
                self.toast.show(&format!("{label}: only for PDFs"));
            }
            _ => return false,
        }
        true
    }

    fn save_position(&self) {
        let Some(doc) = self.view.doc() else { return };
        let Some(place) = self.view.place() else { return };
        let opts = self.view.opts();
        let fit = self.view.fit();
        let pos = Position {
            page: place.page,
            offset: place.y,
            zoom: (fit == Fit::None).then(|| self.view.zoom()),
            fit: fit.name().into(),
            dual: opts.dual,
            continuous: opts.continuous,
            rotation: self.view.rotation(),
            at: 0,
        };
        let mut st = self.state.borrow_mut();
        st.set_position(&doc.path, pos);
        st.save();
    }

    fn watch_file(self: &Rc<Self>, path: &Path) {
        let dir = gio::File::for_path(path.parent().unwrap_or(Path::new("/")));
        let Ok(m) = dir.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) else { return };
        let w = Rc::downgrade(self);
        let target = path.to_path_buf();
        m.connect_changed(move |_, f, other, ev| {
            let hit = |f: Option<&gio::File>| f.and_then(|f| f.path()).map(|p| p == target).unwrap_or(false);
            if !(hit(Some(f)) || hit(other)) {
                return;
            }
            if matches!(
                ev,
                gio::FileMonitorEvent::ChangesDoneHint
                    | gio::FileMonitorEvent::Created
                    | gio::FileMonitorEvent::MovedIn
                    | gio::FileMonitorEvent::Renamed
                    | gio::FileMonitorEvent::Changed
            )
                && let Some(a) = w.upgrade() {
                    a.schedule_reload();
                }
        });
        unsafe { m.set_data("docview-file", true) };
        // One file watcher at a time; the theme watcher stays.
        let mut ms = self.monitors.borrow_mut();
        ms.retain(|m| unsafe { m.data::<bool>("docview-file").is_none() });
        ms.push(m);
    }

    fn schedule_reload(self: &Rc<Self>) {
        if let Some(t) = self.reload_timer.take() {
            t.remove();
        }
        let w = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(350), move || {
            if let Some(a) = w.upgrade() {
                a.reload_timer.take();
                a.reload(false);
            }
        });
        self.reload_timer.replace(Some(id));
    }

    fn reload(&self, forced: bool) {
        if let Some(w) = self.web() {
            w.reload();
            if forced {
                self.toast.show("Reloaded");
            }
            return;
        }
        let Some(doc) = self.view.doc() else { return };
        let meta = std::fs::metadata(&doc.path).ok();
        let stamp = meta.as_ref().and_then(|m| Some((m.modified().ok()?, m.len())));
        if !forced {
            if stamp.is_none() {
                return; // being rewritten; the next event retries
            }
            let own = self.view.own_save();
            if own.is_some() && own == stamp {
                return;
            }
        }
        match Doc::open(&doc.path, doc.password.as_deref()) {
            Ok(d) => {
                self.view.reload(d);
                if forced {
                    self.toast.show("Reloaded");
                }
                self.refresh();
            }
            Err(e) => {
                if forced {
                    self.toast.show(&format!("Could not reload: {e}"));
                }
            }
        }
    }

    // ---------------------------------------------------------------- bars

    fn on_view_change(self: &Rc<Self>, c: Change) {
        match c {
            Change::View | Change::Selection | Change::Annotations => self.refresh(),
            Change::Find => self.refresh_find(),
            Change::Hover(h) => {
                self.hover.replace(h);
                self.refresh_status();
            }
            Change::Message(m) => self.toast.show(&m),
            Change::OpenFile(f) => {
                let base = self.view.doc().and_then(|d| d.path.parent().map(Path::to_path_buf)).unwrap_or_default();
                let p = if Path::new(&f).is_absolute() { PathBuf::from(&f) } else { base.join(&f) };
                if p.exists() {
                    crate::spawn_self(&p);
                } else {
                    self.toast.show(&format!("Not found: {}", home_short(&p)));
                }
            }
        }
    }

    fn refresh(&self) {
        if self.is_web() {
            self.refresh_web();
            return;
        }
        let doc = self.view.doc();
        let has = doc.is_some();
        match &doc {
            Some(d) => {
                let name = d.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.filename.set_text(&name);
                self.filename.set_tooltip_text(Some(&home_short(&d.path)));
                self.window.set_title(Some(&format!("{name} - docview")));
            }
            None => {
                self.filename.set_text("");
                self.window.set_title(Some("docview"));
            }
        }
        let n = self.view.n_pages();
        let cur = self.view.current_page();
        if !self.page_entry.has_focus() && !self.page_entry.is_focus() && !self.page_entry_focused() {
            let label = if has { self.view.page_label(cur).unwrap_or_default() } else { String::new() };
            self.page_entry.set_text(&label);
        }
        self.page_total.set_text(&format!("/ {n}"));
        self.pagebox.set_sensitive(has);
        self.zoom_btn.set_label(&format!("{}%", (self.view.zoom() * 100.0).round()));
        self.zoom_btn.set_sensitive(has);
        let fit = self.view.fit();
        let o = self.view.opts();
        let active = |id: &str| match id {
            "fit-width" => fit == Fit::Width,
            "fit-page" => fit == Fit::Page,
            "continuous" => o.continuous,
            "dual" => o.dual,
            "night" => self.config.borrow().view.night,
            "find" => self.findbar.reveals_child(),
            "focus" => self.focus_mode.get(),
            _ => false,
        };
        for (id, b) in &self.buttons {
            if has && active(id) { b.add_css_class("active") } else { b.remove_css_class("active") }
        }
        if let Some(b) = self.buttons.get("back") {
            b.set_sensitive(self.view.can_go_back());
        }
        if let Some(b) = self.buttons.get("forward") {
            b.set_sensitive(self.view.can_go_forward());
        }
        for id in ["zoom-in", "zoom-out", "fit-width", "fit-page", "continuous", "dual", "outline", "find", "highlight"] {
            if let Some(b) = self.buttons.get(id) {
                b.set_sensitive(has);
            }
        }
        self.refresh_status();
    }

    fn page_entry_focused(&self) -> bool {
        gtk::prelude::GtkWindowExt::focus(&self.window).map(|f| f.is_ancestor(&self.page_entry) || f == self.page_entry.clone().upcast::<gtk::Widget>()).unwrap_or(false)
    }

    fn refresh_status(&self) {
        if let Some(w) = self.web() {
            let left = self.hover.borrow().clone().unwrap_or_else(|| if w.is_loading() { "Loading…".into() } else { w.title().unwrap_or_default() });
            self.status_left.set_text(&left);
            let mut parts = vec![format!("{}%", (w.zoom() * 100.0).round())];
            if self.config.borrow().view.night {
                parts.push("night".into());
            }
            self.status_right.set_text(&parts.join(" · "));
            return;
        }
        let Some(doc) = self.view.doc() else {
            self.status_left.set_text("");
            self.status_right.set_text("");
            return;
        };
        let left = if let Some(h) = self.hover.borrow().clone() {
            h
        } else {
            let mut f: Vec<String> = Vec::new();
            if let Some(t) = &doc.title {
                f.push(t.clone());
            }
            if self.view.has_selection() {
                let n = self.view.selected_text().split_whitespace().count();
                f.push(format!("{n} word{} selected", if n == 1 { "" } else { "s" }));
            }
            f.join(" · ")
        };
        self.status_left.set_text(&left);
        let cur = self.view.current_page();
        let n = doc.n_pages();
        let label = &doc.pages.get(cur).map(|p| p.label.clone()).unwrap_or_default();
        let page = if *label != (cur + 1).to_string() { format!("page {label} ({} of {n})", cur + 1) } else { format!("page {} of {n}", cur + 1) };
        let mut parts = vec![page, format!("{}%", (self.view.zoom() * 100.0).round())];
        let o = self.view.opts();
        if o.dual {
            parts.push("dual".into());
        }
        if !o.continuous {
            parts.push("single".into());
        }
        if self.config.borrow().view.night {
            parts.push("night".into());
        }
        self.status_right.set_text(&parts.join(" · "));
    }

    fn refresh_find(&self) {
        if self.is_web() {
            let n = self.web_matches.get();
            let text = match n {
                _ if self.find_entry.text().is_empty() => String::new(),
                -1 => "Searching…".into(),
                0 => "No matches".into(),
                1 => "1 match".into(),
                n if n >= 1000 => "1000+ matches".into(),
                n => format!("{n} matches"),
            };
            self.find_count.set_text(&text);
            return;
        }
        let (cur, total, running) = self.view.find_status();
        let text = if self.find_entry.text().is_empty() {
            String::new()
        } else if total == 0 {
            if running { "Searching…".into() } else { "No matches".into() }
        } else {
            let more = if running { "+" } else { "" };
            match cur {
                Some(c) => format!("{c} of {total}{more}"),
                None => format!("{total}{more} matches"),
            }
        };
        self.find_count.set_text(&text);
    }

    fn refresh_tooltips(&self) {
        let km = self.keymap.borrow();
        for (id, b) in &self.buttons {
            let cmd = commands::get(id).unwrap();
            let label = cmd.label.trim_end_matches('…');
            let tip = match km.first_key(cmd) {
                Some(k) => format!("{label} ({k})"),
                None => label.to_string(),
            };
            b.set_tooltip_text(Some(&tip));
        }
        let z = commands::get("zoom-pick").unwrap();
        self.zoom_btn.set_tooltip_text(Some(&format!("Zoom ({})", km.first_key(z).unwrap_or_default())));
        let g = commands::get("goto-page").unwrap();
        self.page_entry.set_tooltip_text(Some(&format!("Go to page ({})", km.first_key(g).unwrap_or_default())));
    }

    fn build_empty(self: &Rc<Self>) {
        while let Some(c) = self.empty.first_child() {
            self.empty.remove(&c);
        }
        let title = gtk::Label::new(Some("No document open"));
        title.add_css_class("title");
        let km = self.keymap.borrow();
        let key = km.first_key(commands::get("open").unwrap()).unwrap_or_default();
        let hint = gtk::Label::new(Some(&format!("Open a PDF or web page with {key}, or drop one here")));
        hint.add_css_class("hint");
        hint.set_wrap(true);
        hint.set_justify(gtk::Justification::Center);
        hint.set_margin_bottom(14);
        self.empty.append(&title);
        self.empty.append(&hint);
        for p in self.state.borrow().recent_existing().into_iter().take(6) {
            let b = gtk::Button::new();
            b.add_css_class("recent");
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            let name = gtk::Label::new(Some(&p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
            name.set_xalign(0.0);
            name.set_hexpand(true);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.set_max_width_chars(40);
            let dir = gtk::Label::new(Some(&p.parent().map(home_short).unwrap_or_default()));
            dir.add_css_class("path");
            dir.set_ellipsize(gtk::pango::EllipsizeMode::Start);
            dir.set_max_width_chars(36);
            row.append(&name);
            row.append(&dir);
            b.set_child(Some(&row));
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(a) = w.upgrade() {
                    a.open_path(&p, None);
                }
            });
            self.empty.append(&b);
        }
    }

    // ---------------------------------------------------------------- find

    fn open_find(&self) {
        self.findbar.set_reveal_child(true);
        self.findbar.set_visible(true);
        if self.view.has_selection() {
            let t = self.view.selected_text();
            if !t.is_empty() && t.len() < 200 && !t.contains('\n') {
                self.find_entry.set_text(&t);
            }
        }
        self.find_entry.grab_focus();
        self.find_entry.select_region(0, -1);
        self.refresh();
    }

    fn close_find(&self) {
        self.findbar.set_reveal_child(false);
        self.findbar.set_visible(false);
        if let Some(w) = self.web() {
            w.find_finish();
            w.widget.grab_focus();
        } else {
            self.view.clear_find();
            self.view.grab_focus();
        }
        self.refresh();
    }

    fn start_find(&self) {
        if let Some(w) = self.web() {
            self.web_matches.set(-1);
            w.find(&self.find_entry.text(), self.find_case.is_active());
            self.refresh_find();
            return;
        }
        self.view.find(&self.find_entry.text(), self.find_case.is_active());
    }

    fn find_step(&self, dir: i32) {
        match self.web() {
            Some(w) => w.find_step(dir),
            None => self.view.find_step(dir),
        }
    }

    // ---------------------------------------------------------------- commands

    pub fn run(self: &Rc<Self>, id: &str) {
        if crate::debug() {
            eprintln!("run {id}");
        }
        if self.is_web() && self.run_web(id) {
            self.refresh();
            return;
        }
        let has = self.view.doc().is_some();
        let v = &self.view;
        match id {
            "open" => self.choose_file(),
            "open-recent" => self.pick_recent(),
            "reload" => self.reload(true),
            "print" if has => self.print(),
            "copy-path" if has => {
                if let Some(d) = v.doc() {
                    v.clipboard().set_text(&d.path.to_string_lossy());
                    self.toast.show("Path copied");
                }
            }
            "properties" if has => self.properties(),
            "close" => {
                self.save_position();
                self.window.close();
            }
            "goto-page" if has => {
                self.page_entry.grab_focus();
                self.page_entry.select_region(0, -1);
            }
            "outline" if has => self.pick_outline(),
            "next-page" => v.next_page(1),
            "prev-page" => v.next_page(-1),
            "first-page" if has => v.go_to_page(0),
            "last-page" if has => v.go_to_page(v.n_pages().saturating_sub(1)),
            "back" => {
                v.history_back();
            }
            "forward" => {
                v.history_forward();
            }
            "scroll-down" => v.scroll_by(0.0, 48.0),
            "scroll-up" => v.scroll_by(0.0, -48.0),
            "scroll-left" => v.scroll_by(-48.0, 0.0),
            "scroll-right" => v.scroll_by(48.0, 0.0),
            "screen-down" => v.scroll_screens(1.0),
            "screen-up" => v.scroll_screens(-1.0),
            "zoom-in" => v.zoom_step(1),
            "zoom-out" => v.zoom_step(-1),
            "zoom-reset" => v.zoom_step(0),
            "zoom-pick" if has => self.pick_zoom(),
            "fit-width" => v.set_fit(if v.fit() == Fit::Width { Fit::None } else { Fit::Width }),
            "fit-page" => v.set_fit(if v.fit() == Fit::Page { Fit::None } else { Fit::Page }),
            "continuous" | "dual" | "cover" => {
                let mut o = v.opts();
                match id {
                    "continuous" => o.continuous = !o.continuous,
                    "dual" => o.dual = !o.dual,
                    _ => o.cover = !o.cover,
                }
                v.set_opts(o);
                {
                    let mut c = self.config.borrow_mut();
                    c.view.continuous = o.continuous;
                    c.view.dual = o.dual;
                    c.view.cover = o.cover;
                }
                self.save_config();
            }
            "night" => {
                let on = !self.config.borrow().view.night;
                self.config.borrow_mut().view.night = on;
                self.save_config();
                self.apply_view_colors();
                self.refresh();
            }
            "rotate-right" if has => v.rotate(90),
            "rotate-left" if has => v.rotate(-90),
            "presentation" if has => self.toggle_presentation(),
            "links" if has => self.pick_link(),
            "focus" => {
                let on = !self.focus_mode.get();
                self.focus_mode.set(on);
                self.apply_bars();
            }
            "topbar" => {
                if self.focus_mode.get() {
                    self.focus_mode.set(false);
                    self.config.borrow_mut().app.topbar = true;
                } else {
                    let on = !self.config.borrow().app.topbar;
                    self.config.borrow_mut().app.topbar = on;
                }
                self.save_config();
                self.apply_bars();
            }
            "theme-mode" => {
                let next = match self.config.borrow().app.mode.as_str() {
                    "system" => "light",
                    "light" => "dark",
                    _ => "system",
                };
                self.config.borrow_mut().app.mode = next.into();
                self.save_config();
                self.apply_theme();
                self.toast.show(&format!("Color mode: {next}"));
            }
            "fullscreen" => {
                if self.window.is_fullscreen() { self.window.unfullscreen() } else { self.window.fullscreen() }
            }
            "find" if has => self.open_find(),
            "find-next" if has => {
                if !self.findbar.reveals_child() {
                    self.open_find();
                } else {
                    v.find_step(1);
                }
            }
            "find-prev" if has => v.find_step(-1),
            "copy" => {
                if v.copy() {
                    self.toast.show("Copied");
                }
            }
            "select-all" if has => v.select_page(),
            "highlight" if has => {
                if !v.highlight_selection() {
                    self.toast.show("Select some text to highlight");
                }
            }
            "unhighlight" if has => {
                if !v.remove_highlight() {
                    self.toast.show("No highlight here: click one or select over it first");
                }
            }
            "highlight-color" => self.pick_highlight_color(),
            "undo" => {
                if !v.undo(false) {
                    self.toast.show("Nothing to undo");
                }
            }
            "redo" => {
                if !v.undo(true) {
                    self.toast.show("Nothing to redo");
                }
            }
            "highlights" if has => self.pick_highlight(),
            "escape" => {
                if self.presenting.borrow().is_some() {
                    self.toggle_presentation();
                } else if self.findbar.reveals_child() {
                    self.close_find();
                } else if !v.clear_selection() && self.focus_mode.get() {
                    self.run("focus");
                } else if self.window.is_fullscreen() {
                    self.window.unfullscreen();
                }
            }
            "palette" => self.palette(),
            "settings" => self.settings(),
            "shortcuts" => self.shortcuts(),
            _ => {}
        }
        if id != "close" {
            self.refresh();
        }
    }

    fn choose_file(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::new();
        dialog.set_title("Open document");
        let docs = gtk::FileFilter::new();
        docs.set_name(Some("Documents (PDF, HTML)"));
        let pdf = gtk::FileFilter::new();
        pdf.set_name(Some("PDF documents"));
        let html = gtk::FileFilter::new();
        html.set_name(Some("Web pages"));
        for (f, mimes, exts) in [
            (&pdf, &["application/pdf"][..], &["pdf"][..]),
            (&html, &["text/html", "application/xhtml+xml"][..], &["html", "htm", "xhtml"][..]),
        ] {
            for m in mimes {
                f.add_mime_type(m);
                docs.add_mime_type(m);
            }
            for e in exts {
                f.add_suffix(e);
                docs.add_suffix(e);
            }
        }
        let all = gtk::FileFilter::new();
        all.set_name(Some("All files"));
        all.add_pattern("*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        for f in [&docs, &pdf, &html, &all] {
            filters.append(f);
        }
        dialog.set_filters(Some(&filters));
        if let Some(dir) = self.current_path().and_then(|p| p.parent().map(Path::to_path_buf)) {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let w = Rc::downgrade(self);
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |res| {
            if let (Some(a), Ok(f)) = (w.upgrade(), res)
                && let Some(p) = f.path() {
                    a.open_path(&p, None);
                }
        });
    }

    fn pick_recent(self: &Rc<Self>) {
        let cur = self.current_path();
        let items: Vec<Item> = self
            .state
            .borrow()
            .recent_existing()
            .into_iter()
            .filter(|p| Some(p) != cur.as_ref())
            .map(|p| {
                let w = Rc::downgrade(self);
                let label = home_short(&p);
                Item::new(label, None, move || {
                    if let Some(a) = w.upgrade() {
                        a.open_path(&p, None);
                    }
                })
            })
            .collect();
        if items.is_empty() {
            self.toast.show("No recent documents");
            return;
        }
        ui::open_picker(&self.modal, items, "Open recent…", None, 60);
    }

    fn pick_outline(self: &Rc<Self>) {
        let Some(doc) = self.view.doc() else { return };
        let outline = doc.outline();
        if outline.is_empty() {
            self.toast.show("This document has no outline");
            return;
        }
        let items: Vec<Item> = outline
            .into_iter()
            .map(|o| {
                let hint = match &o.target {
                    Some(Target::Page { page, .. }) => doc.pages.get(*page).map(|p| p.label.clone()),
                    _ => None,
                };
                let w = Rc::downgrade(self);
                let label = format!("{}{}", "\u{2003}".repeat(o.depth), if o.title.is_empty() { "(untitled)" } else { &o.title });
                let t = o.target.clone();
                Item::new(label, hint, move || {
                    if let (Some(a), Some(t)) = (w.upgrade(), t.clone()) {
                        a.view.go_to_target(t);
                        a.refresh();
                    }
                })
            })
            .collect();
        ui::open_picker(&self.modal, items, "Go to section…", None, 400);
    }

    /// Fullscreen, no bars, one page at a time fitted to the screen. Again (or Escape) restores.
    fn toggle_presentation(self: &Rc<Self>) {
        let saved = self.presenting.take();
        match saved {
            Some((opts, fit, zoom)) => {
                self.window.unfullscreen();
                if self.focus_mode.get() {
                    self.run("focus");
                }
                self.view.set_opts(opts);
                if fit == Fit::None {
                    self.view.set_zoom(zoom, None);
                } else {
                    self.view.set_fit(fit);
                }
            }
            None => {
                self.presenting.replace(Some((self.view.opts(), self.view.fit(), self.view.zoom())));
                self.window.fullscreen();
                if !self.focus_mode.get() {
                    self.run("focus");
                }
                let mut o = self.view.opts();
                o.continuous = false;
                o.dual = false;
                self.view.set_opts(o);
                self.view.set_fit(Fit::Page);
                self.toast.show("Presentation: Space / arrows to move, Esc to leave");
            }
        }
        self.refresh();
    }

    fn pick_link(self: &Rc<Self>) {
        let links = self.view.visible_links();
        if links.is_empty() {
            self.toast.show("No links on this page");
            return;
        }
        let doc = self.view.doc().unwrap();
        let items = links
            .into_iter()
            .map(|(_, link, text)| {
                let target_desc = match &link.target {
                    Target::Uri(u) => u.clone(),
                    Target::File(f) => f.clone(),
                    Target::Page { page, .. } => format!("page {}", doc.pages.get(*page).map(|p| p.label.clone()).unwrap_or_default()),
                };
                let (label, hint) = if text.is_empty() { (target_desc, None) } else { (text, Some(target_desc)) };
                let w = Rc::downgrade(self);
                let t = link.target.clone();
                Item::new(label, hint, move || {
                    if let Some(a) = w.upgrade() {
                        a.view.go_to_target(t.clone());
                        a.refresh();
                    }
                })
            })
            .collect();
        ui::open_picker(&self.modal, items, "Follow a link…", None, 200);
    }

    fn pick_zoom(self: &Rc<Self>) {
        let mut items = Vec::new();
        let fits: &[(&str, Fit)] = if self.is_web() { &[] } else { &[("Fit width", Fit::Width), ("Fit page", Fit::Page)] };
        for &(label, fit) in fits {
            let w = Rc::downgrade(self);
            items.push(Item::new(label, None, move || {
                if let Some(a) = w.upgrade() {
                    a.view.set_fit(fit);
                    a.refresh();
                }
            }));
        }
        for z in [50, 75, 100, 125, 150, 200, 300, 400] {
            let w = Rc::downgrade(self);
            items.push(Item::new(format!("{z}%"), None, move || {
                if let Some(a) = w.upgrade() {
                    a.set_zoom(z as f64 / 100.0);
                }
            }));
        }
        let w = Rc::downgrade(self);
        let free: Rc<dyn Fn(&str) -> bool> = Rc::new(move |t: &str| {
            let Some(a) = w.upgrade() else { return false };
            match t.trim().trim_end_matches('%').trim().parse::<f64>() {
                Ok(z) if z > 0.0 => {
                    a.set_zoom(z / 100.0);
                    true
                }
                _ => false,
            }
        });
        ui::open_picker(&self.modal, items, "Zoom (type a percentage)…", Some(free), 60);
    }

    fn pick_highlight_color(self: &Rc<Self>) {
        let current = self.config.borrow().app.highlight.clone();
        let pal = self.palette.borrow().clone();
        let items = HL_COLORS
            .iter()
            .map(|slot| {
                let w = Rc::downgrade(self);
                let rgb = pal.resolve(slot).unwrap_or(pal.c[3]);
                let label = format!("{}  {}", style::color_name(rgb), style::hex(rgb));
                let slot = slot.to_string();
                let hint = (slot == current).then(|| "current".to_string());
                Item::new(label, hint, move || {
                    if let Some(a) = w.upgrade() {
                        a.set_highlight_color(&slot);
                    }
                })
            })
            .collect();
        ui::open_picker(&self.modal, items, "Highlight color…", None, 60);
    }

    fn set_highlight_color(&self, slot: &str) {
        self.config.borrow_mut().app.highlight = slot.to_string();
        self.save_config();
        self.apply_view_colors();
    }

    /// Name of the configured highlight color, from the color itself ("Orange").
    fn highlight_name(&self) -> String {
        let cur = self.config.borrow().app.highlight.clone();
        let pal = self.palette.borrow();
        style::color_name(pal.resolve(&cur).unwrap_or(pal.c[3])).to_string()
    }

    /// A row of round color swatches; `on_pick` runs after the color is saved.
    fn color_swatches(self: &Rc<Self>, on_pick: Rc<dyn Fn(&Rc<App>)>) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.add_css_class("swatches");
        row.set_valign(gtk::Align::Center);
        let pal = self.palette.borrow().clone();
        let current = self.config.borrow().app.highlight.clone();
        let mut all = Vec::new();
        let mut slots: Vec<&'static str> = Vec::new();
        let mut seen: Vec<style::Rgb> = Vec::new();
        for slot in HL_COLORS {
            let rgb = pal.resolve(slot).unwrap_or(pal.c[3]);
            // The accent often equals one of the palette colors; show each color once.
            if seen.contains(&rgb) && *slot != current {
                continue;
            }
            seen.push(rgb);
            slots.push(slot);
            let name = format!("{}  {}", style::color_name(rgb), style::hex(rgb));
            let dot = gtk::DrawingArea::new();
            dot.set_content_width(16);
            dot.set_content_height(16);
            dot.set_draw_func(move |_, cr, w, h| {
                let r = (w.min(h) as f64) / 2.0;
                cr.arc(w as f64 / 2.0, h as f64 / 2.0, r, 0.0, std::f64::consts::TAU);
                cr.set_source_rgb(rgb[0] as f64 / 255.0, rgb[1] as f64 / 255.0, rgb[2] as f64 / 255.0);
                let _ = cr.fill();
            });
            let b = gtk::Button::new();
            b.add_css_class("swatch");
            b.set_child(Some(&dot));
            b.set_tooltip_text(Some(&name));
            b.set_valign(gtk::Align::Center);
            b.set_focus_on_click(false);
            if *slot == current {
                b.add_css_class("active");
            }
            row.append(&b);
            all.push(b);
        }
        for (i, b) in all.iter().enumerate() {
            let w = Rc::downgrade(self);
            let (others, on_pick, slot) = (all.clone(), on_pick.clone(), slots[i]);
            b.connect_clicked(move |_| {
                let Some(a) = w.upgrade() else { return };
                for (j, o) in others.iter().enumerate() {
                    if i == j { o.add_css_class("active") } else { o.remove_css_class("active") }
                }
                a.set_highlight_color(slot);
                on_pick(&a);
            });
        }
        row
    }

    /// Color menu under the highlight button. Picking a color also highlights the selection.
    fn highlight_menu(self: &Rc<Self>, anchor: &gtk::Widget) {
        let pop = gtk::Popover::new();
        pop.add_css_class("hl-menu");
        pop.set_has_arrow(false);
        pop.set_position(gtk::PositionType::Bottom);
        pop.set_parent(anchor);
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let title = gtk::Label::new(Some("Highlight color"));
        title.add_css_class("menu-title");
        title.set_xalign(0.0);
        box_.append(&title);
        let p = pop.clone();
        box_.append(&self.color_swatches(Rc::new(move |a| {
            p.popdown();
            // Apply to the current selection (or search match) right away, quietly if none.
            if a.view.doc().is_some() {
                a.view.highlight_selection();
            }
        })));
        pop.set_child(Some(&box_));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }

    fn pick_highlight(self: &Rc<Self>) {
        let all = self.view.all_highlights();
        if all.is_empty() {
            self.toast.show("No highlights in this document");
            return;
        }
        let doc = self.view.doc().unwrap();
        let items = all
            .into_iter()
            .map(|(page, rect, text)| {
                let w = Rc::downgrade(self);
                let label = if text.is_empty() { "(highlight)".to_string() } else { text.chars().take(120).collect() };
                Item::new(label, doc.pages.get(page).map(|p| p.label.clone()), move || {
                    if let Some(a) = w.upgrade() {
                        a.view.reveal_rect(page, rect);
                        a.refresh();
                    }
                })
            })
            .collect();
        ui::open_picker(&self.modal, items, "Go to highlight…", None, 500);
    }

    fn palette(self: &Rc<Self>) {
        let km = self.keymap.borrow();
        let mut items = Vec::new();
        let kind = self.kind.get();
        for c in COMMANDS.iter().filter(|c| !c.hidden && applies(c.id, kind)) {
            let w = Rc::downgrade(self);
            let id = c.id;
            items.push(Item::command(c.label, km.first_key(c), move || {
                if let Some(a) = w.upgrade() {
                    a.run(id);
                }
            }));
        }
        let cur = self.current_path();
        for p in self.state.borrow().recent_existing().into_iter().filter(|p| Some(p) != cur.as_ref()).take(10) {
            let w = Rc::downgrade(self);
            items.push(Item::new(format!("Recent: {}", home_short(&p)), None, move || {
                if let Some(a) = w.upgrade() {
                    a.open_path(&p, None);
                }
            }));
        }
        drop(km);
        ui::open_picker(&self.modal, items, "Type a command…", None, 80);
    }

    fn shortcuts(self: &Rc<Self>) {
        let km = self.keymap.borrow();
        let mut sections: Vec<(&'static str, Vec<(String, String)>)> = Vec::new();
        for c in COMMANDS {
            let keys = km.keys_for(c);
            if keys.is_empty() {
                continue;
            }
            let row = (c.label.trim_end_matches('…').to_string(), keys.join(" or "));
            match sections.iter_mut().find(|s| s.0 == c.group) {
                Some(s) => s.1.push(row),
                None => sections.push((c.group, vec![row])),
            }
        }
        let mouse = vec![
            ("Select text".to_string(), "Drag".to_string()),
            ("Select word / line".to_string(), "Double-click or Triple-click".to_string()),
            ("Extend selection".to_string(), "Shift+Click".to_string()),
            ("Zoom".to_string(), "Ctrl+Wheel or Pinch".to_string()),
            ("Pan".to_string(), "Middle-drag".to_string()),
            ("Follow a link".to_string(), "Click".to_string()),
        ];
        sections.push(("Mouse", mouse));
        let note = format!(
            "Change any binding in {} under [keys], e.g. highlight = \"Ctrl+H\". Several keys: \"H, Ctrl+H\".",
            config::path().map(|p| home_short(&p)).unwrap_or_default()
        );
        // Leave room for the panel's top offset, title and note.
        let max_h = (self.window.height() as f64 * 0.88) as i32 - 150;
        let panel = ui::shortcuts_panel(sections, &note, max_h, self.window.width() - 32);
        drop(km);
        self.modal.open(&panel, None);
        panel.grab_focus();
    }

    fn settings(self: &Rc<Self>) {
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 2);
        panel.add_css_class("settings");
        panel.set_size_request(500, -1);
        let title = gtk::Label::new(Some("SETTINGS"));
        title.add_css_class("heading");
        title.set_xalign(0.0);
        panel.append(&title);
        let c = self.config.borrow().clone();
        // Each row highlights as a whole while hovered or while focus is inside it.
        let new_row = |label: &str| {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            row.add_css_class("setting");
            let l = gtk::Label::new(Some(label));
            l.set_xalign(0.0);
            l.set_hexpand(true);
            row.append(&l);
            row
        };
        let switch_row = |label: &str, on: bool, f: Box<dyn Fn(&Rc<App>, bool)>| {
            let row = new_row(label);
            let s = gtk::Switch::new();
            s.set_active(on);
            s.set_valign(gtk::Align::Center);
            let w = Rc::downgrade(self);
            s.connect_state_set(move |_, v| {
                if let Some(a) = w.upgrade() {
                    f(&a, v);
                    a.save_config();
                    a.refresh();
                }
                glib::Propagation::Proceed
            });
            // A click anywhere on the row flips the switch.
            let click = gtk::GestureClick::new();
            let sw = s.clone();
            click.connect_released(move |g, _, x, y| {
                let Some(row) = g.widget() else { return };
                let on_switch = row.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|p| p == sw || p.is_ancestor(&sw));
                if !on_switch {
                    sw.set_active(!sw.is_active());
                    sw.grab_focus();
                }
            });
            row.add_controller(click);
            row.append(&s);
            row
        };
        // Segmented choice: one toggle per option, Left / Right moves between them.
        let choice_row = |label: &str, opts: &[&str], active: usize, f: Rc<dyn Fn(&Rc<App>, usize)>| {
            let row = new_row(label);
            let seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            seg.add_css_class("segmented");
            seg.set_valign(gtk::Align::Center);
            let mut first: Option<gtk::ToggleButton> = None;
            for (i, o) in opts.iter().enumerate() {
                let t = gtk::ToggleButton::with_label(o);
                t.add_css_class("seg");
                t.set_focus_on_click(false);
                if let Some(g) = &first {
                    t.set_group(Some(g));
                } else {
                    first = Some(t.clone());
                }
                t.set_active(i == active);
                let (w, f) = (Rc::downgrade(self), f.clone());
                t.connect_toggled(move |t| {
                    if let (true, Some(a)) = (t.is_active(), w.upgrade()) {
                        f(&a, i);
                    }
                });
                seg.append(&t);
            }
            row.append(&seg);
            row
        };
        let set_opts = |a: &Rc<App>| {
            let c = a.config.borrow().view.clone();
            a.view.set_opts(Opts { continuous: c.continuous, dual: c.dual, cover: c.cover });
        };
        panel.append(&switch_row("Continuous scrolling", c.view.continuous, Box::new(move |a, v| {
            a.config.borrow_mut().view.continuous = v;
            set_opts(a);
        })));
        panel.append(&switch_row("Dual pages", c.view.dual, Box::new(move |a, v| {
            a.config.borrow_mut().view.dual = v;
            set_opts(a);
        })));
        panel.append(&switch_row("Cover page on its own", c.view.cover, Box::new(move |a, v| {
            a.config.borrow_mut().view.cover = v;
            set_opts(a);
        })));
        panel.append(&switch_row("Night mode", c.view.night, Box::new(|a, v| {
            a.config.borrow_mut().view.night = v;
            a.apply_view_colors();
        })));
        panel.append(&switch_row("Show the top bar", c.app.topbar, Box::new(|a, v| {
            a.config.borrow_mut().app.topbar = v;
            a.apply_bars();
        })));
        panel.append(&switch_row("Reopen where I left off", c.app.restore_position, Box::new(|a, v| {
            a.config.borrow_mut().app.restore_position = v;
        })));
        let modes = ["system", "light", "dark"];
        let mode_row = choice_row(
            "Colors",
            &["System", "Light", "Dark"],
            modes.iter().position(|m| *m == c.app.mode).unwrap_or(0),
            Rc::new(move |a, i| {
                a.config.borrow_mut().app.mode = modes[i].into();
                a.save_config();
                a.apply_theme();
            }),
        );
        mode_row.set_tooltip_text(Some("System follows pywal, or your desktop's light / dark setting"));
        panel.append(&mode_row);
        panel.append(&choice_row(
            "New documents open at",
            &["Fit width", "Fit page", "100%"],
            match c.view.fit.as_str() { "width" => 0, "page" => 1, _ => 2 },
            Rc::new(|a, i| {
                let mut c = a.config.borrow_mut();
                c.view.fit = ["width", "page", "none"][i].into();
                c.view.zoom = 1.0;
                drop(c);
                a.save_config();
            }),
        ));
        // Highlight color: the actual colors, with the chosen one's name.
        let row = new_row("Highlight color");
        let name = gtk::Label::new(Some(&self.highlight_name()));
        name.add_css_class("value");
        row.append(&name);
        let n = name.clone();
        row.append(&self.color_swatches(Rc::new(move |a| n.set_text(&a.highlight_name()))));
        panel.append(&row);
        let note = gtk::Label::new(Some(&format!(
            "More options in {}",
            config::path().map(|p| home_short(&p)).unwrap_or_default()
        )));
        note.add_css_class("settings-note");
        note.set_xalign(0.0);
        note.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        panel.append(&note);
        self.modal.open(&panel, None);
        panel.child_focus(gtk::DirectionType::TabForward);
    }

    fn properties(self: &Rc<Self>) {
        let Some(d) = self.view.doc() else { return };
        let mut lines: Vec<(String, String)> = Vec::new();
        let get = |o: Option<glib::GString>| o.map(|s| s.to_string()).filter(|s| !s.trim().is_empty());
        if let Some(t) = &d.title {
            lines.push(("Title".into(), t.clone()));
        }
        if let Some(a) = get(d.inner.author()) {
            lines.push(("Author".into(), a));
        }
        if let Some(s) = get(d.inner.subject()) {
            lines.push(("Subject".into(), s));
        }
        lines.push(("Pages".into(), d.n_pages().to_string()));
        if let Some(p) = d.pages.first() {
            lines.push(("Page size".into(), format!("{:.0} × {:.0} mm", p.w / 72.0 * 25.4, p.h / 72.0 * 25.4)));
        }
        if let Some(v) = get(d.inner.pdf_version_string()) {
            lines.push(("Format".into(), v));
        }
        if let Some(p) = get(d.inner.producer()) {
            lines.push(("Producer".into(), p));
        }
        if let Some(c) = get(d.inner.creator()) {
            lines.push(("Creator".into(), c));
        }
        if let Ok(m) = std::fs::metadata(&d.path) {
            lines.push(("File size".into(), format!("{:.1} MB", m.len() as f64 / 1_048_576.0)));
        }
        lines.push(("Location".into(), home_short(&d.path)));
        let text = lines.iter().map(|(k, v)| format!("{k}:  {v}")).collect::<Vec<_>>().join("\n");
        ui::ask(&self.modal, &text, vec![("Close", true, Rc::new(|| {}))]);
    }

    fn print(self: &Rc<Self>) {
        let Some(doc) = self.view.doc() else { return };
        let op = gtk::PrintOperation::new();
        op.set_n_pages(doc.n_pages() as i32);
        op.set_use_full_page(false);
        op.set_embed_page_setup(true);
        let d = doc.clone();
        op.connect_draw_page(move |_, ctx, page| {
            let Some(p) = d.inner.page(page) else { return };
            let (pw, ph) = p.size();
            let cr = ctx.cairo_context();
            let s = (ctx.width() / pw).min(ctx.height() / ph);
            cr.scale(s, s);
            p.render_for_printing(&cr);
        });
        if let Err(e) = op.run(gtk::PrintOperationAction::PrintDialog, Some(&self.window)) {
            self.toast.show(&format!("Print failed: {e}"));
        }
    }
}
