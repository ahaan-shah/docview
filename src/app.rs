//! Window, bars and the command dispatcher. Every user action goes through `App::run(id)`.

use crate::commands::{self, COMMANDS};
use crate::config::{self, Config};
use crate::icons;
use crate::keymap::{self, Keymap};
use crate::layout::{Fit, Opts};
use crate::pdf::{Doc, OpenError, Target};
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
    ("app", &["open", "palette", "settings"]),
];

pub struct App {
    pub window: gtk::ApplicationWindow,
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
        let left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
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
            let g = gtk::Box::new(gtk::Orientation::Horizontal, 0);
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
        let handle = gtk::WindowHandle::new();
        handle.set_child(Some(&bar));

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
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&scroller, Some("doc"));
        stack.set_vexpand(true);

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
            palette: RefCell::new(Palette::from_theme(&theme::current(Default::default(), &Default::default()))),
            portal: &PORTAL,
            monitors: RefCell::new(Vec::new()),
            reload_timer: RefCell::new(None),
            hover: RefCell::new(None),
            focus_mode: Cell::new(false),
            find_timer: RefCell::new(None),
        });
        app.wire(&prev, &next, &close);
        app.apply_theme();
        app.watch_theme();
        app.apply_view_config();
        app.refresh_tooltips();
        app.build_empty();
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
                    a.view.find_step(1);
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
        // Narrow windows drop button groups, like allwrite's media queries.
        {
            let w = Rc::downgrade(self);
            self.window.connect_default_width_notify(move |_| {
                if let Some(a) = w.upgrade() {
                    a.responsive();
                }
            });
        }
    }

    fn responsive(&self) {
        let w = self.window.width().max(self.window.default_width());
        for (name, g) in &self.groups {
            let show = match *name {
                "view" => w > 760,
                "tools" => w > 560,
                "zoom" => w > 440,
                _ => true,
            };
            g.set_visible(show);
        }
        self.pagebox.set_visible(w > 360);
    }

    fn on_key(self: &Rc<Self>, c: &gtk::EventControllerKey, kv: gdk::Key, code: u32, state: gdk::ModifierType) -> glib::Propagation {
        let unshifted = gtk::prelude::WidgetExt::display(&self.window)
            .map_keycode(code)
            .and_then(|v| v.into_iter().find(|(k, _)| k.group() == c.group() as i32 && k.level() == 0).map(|(_, kv)| kv));
        let Some(combo) = keymap::combo_from_event(kv, unshifted, state) else { return glib::Propagation::Proceed };
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
                self.view.find_step(if combo == "Shift+Enter" || combo == "Up" { -1 } else { 1 });
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
        let theme = theme::current(*self.portal.lock().unwrap(), &self.config.borrow().theme);
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
        let recolor = night.then(|| Recolor { bg: pal.bg, fg: pal.fg });
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
                    if matches!(ev, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created | gio::FileMonitorEvent::MovedIn | gio::FileMonitorEvent::Renamed) {
                        if let Some(a) = w.upgrade() {
                            a.apply_theme();
                        }
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

    /// Top-bar buttons that show as active, for the dev hooks.
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
        match Doc::open(&path, password.as_deref()) {
            Ok(doc) => {
                self.save_position();
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
                let place = pos.map(|p| Place { page: p.page, y: p.offset });
                self.view.set_doc(Some(doc), place);
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
            ) {
                if let Some(a) = w.upgrade() {
                    a.schedule_reload();
                }
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
                    let _ = std::process::Command::new(std::env::current_exe().unwrap_or_else(|_| "docview".into())).arg(&p).spawn();
                } else {
                    self.toast.show(&format!("Not found: {}", home_short(&p)));
                }
            }
        }
    }

    fn refresh(&self) {
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
        let hint = gtk::Label::new(Some(&format!("Open a PDF with {key}, or drop one here")));
        hint.add_css_class("hint");
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
        self.view.clear_find();
        self.view.grab_focus();
        self.refresh();
    }

    fn start_find(&self) {
        self.view.find(&self.find_entry.text(), self.find_case.is_active());
    }

    // ---------------------------------------------------------------- commands

    pub fn run(self: &Rc<Self>, id: &str) {
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
            "focus" => {
                let on = !self.focus_mode.get();
                self.focus_mode.set(on);
                if on { self.window.add_css_class("focus-mode") } else { self.window.remove_css_class("focus-mode") }
                self.refresh();
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
                if self.findbar.reveals_child() {
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
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("PDF documents"));
        filter.add_mime_type("application/pdf");
        filter.add_suffix("pdf");
        let all = gtk::FileFilter::new();
        all.set_name(Some("All files"));
        all.add_pattern("*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        filters.append(&all);
        dialog.set_filters(Some(&filters));
        if let Some(dir) = self.view.doc().and_then(|d| d.path.parent().map(Path::to_path_buf)) {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let w = Rc::downgrade(self);
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |res| {
            if let (Some(a), Ok(f)) = (w.upgrade(), res) {
                if let Some(p) = f.path() {
                    a.open_path(&p, None);
                }
            }
        });
    }

    fn pick_recent(self: &Rc<Self>) {
        let cur = self.view.doc().map(|d| d.path.clone());
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

    fn pick_zoom(self: &Rc<Self>) {
        let mut items = Vec::new();
        for (label, fit) in [("Fit width", Fit::Width), ("Fit page", Fit::Page)] {
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
                    a.view.set_zoom(z as f64 / 100.0, None);
                    a.refresh();
                }
            }));
        }
        let w = Rc::downgrade(self);
        let free: Rc<dyn Fn(&str) -> bool> = Rc::new(move |t: &str| {
            let Some(a) = w.upgrade() else { return false };
            match t.trim().trim_end_matches('%').trim().parse::<f64>() {
                Ok(z) if z > 0.0 => {
                    a.view.set_zoom(z / 100.0, None);
                    a.refresh();
                    true
                }
                _ => false,
            }
        });
        ui::open_picker(&self.modal, items, "Zoom (type a percentage)…", Some(free), 60);
    }

    fn pick_highlight_color(self: &Rc<Self>) {
        let names = [("Yellow", "color3"), ("Green", "color2"), ("Red", "color1"), ("Blue", "color4"), ("Purple", "color5"), ("Cyan", "color6"), ("Orange", "color11"), ("Accent", "accent")];
        let current = self.config.borrow().app.highlight.clone();
        let items = names
            .iter()
            .map(|(label, slot)| {
                let w = Rc::downgrade(self);
                let slot = slot.to_string();
                let hint = if slot == current { Some("current".to_string()) } else { Some(slot.clone()) };
                Item::new(*label, hint, move || {
                    if let Some(a) = w.upgrade() {
                        a.config.borrow_mut().app.highlight = slot.clone();
                        a.save_config();
                        a.apply_view_colors();
                    }
                })
            })
            .collect();
        ui::open_picker(&self.modal, items, "Highlight color…", None, 60);
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
        for c in COMMANDS.iter().filter(|c| !c.hidden) {
            let w = Rc::downgrade(self);
            let id = c.id;
            items.push(Item::command(c.label, km.first_key(c), move || {
                if let Some(a) = w.upgrade() {
                    a.run(id);
                }
            }));
        }
        let cur = self.view.doc().map(|d| d.path.clone());
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
        let panel = ui::shortcuts_panel(sections, &note);
        drop(km);
        self.modal.open(&panel, None);
        panel.grab_focus();
    }

    fn settings(self: &Rc<Self>) {
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        panel.add_css_class("settings");
        panel.set_size_request(400, -1);
        let title = gtk::Label::new(Some("SETTINGS"));
        title.add_css_class("heading");
        title.set_xalign(0.0);
        panel.append(&title);
        let c = self.config.borrow().clone();
        let switch_row = |label: &str, on: bool, f: Box<dyn Fn(&Rc<App>, bool)>| {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            row.add_css_class("setting");
            let l = gtk::Label::new(Some(label));
            l.set_xalign(0.0);
            l.set_hexpand(true);
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
            row.append(&l);
            row.append(&s);
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
        panel.append(&switch_row("Reopen where I left off", c.app.restore_position, Box::new(|a, v| {
            a.config.borrow_mut().app.restore_position = v;
        })));
        // Default fit
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        row.add_css_class("setting");
        let l = gtk::Label::new(Some("New documents open at"));
        l.set_xalign(0.0);
        l.set_hexpand(true);
        let dd = gtk::DropDown::from_strings(&["Fit width", "Fit page", "100%"]);
        dd.set_selected(match c.view.fit.as_str() { "width" => 0, "page" => 1, _ => 2 });
        dd.set_valign(gtk::Align::Center);
        {
            let w = Rc::downgrade(self);
            dd.connect_selected_notify(move |d| {
                if let Some(a) = w.upgrade() {
                    let mut c = a.config.borrow_mut();
                    c.view.fit = ["width", "page", "none"][d.selected().min(2) as usize].into();
                    c.view.zoom = 1.0;
                    drop(c);
                    a.save_config();
                }
            });
        }
        row.append(&l);
        row.append(&dd);
        panel.append(&row);
        // Highlight color
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        row.add_css_class("setting");
        let l = gtk::Label::new(Some("Highlight color"));
        l.set_xalign(0.0);
        l.set_hexpand(true);
        let b = gtk::Button::with_label(&c.app.highlight);
        b.add_css_class("btn");
        b.set_valign(gtk::Align::Center);
        {
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(a) = w.upgrade() {
                    a.modal.close();
                    a.pick_highlight_color();
                }
            });
        }
        row.append(&l);
        row.append(&b);
        panel.append(&row);
        let note = gtk::Label::new(Some(&format!(
            "Colors follow your pywal palette. More options (keys, theme slots, UI font, cache size) in {}",
            config::path().map(|p| home_short(&p)).unwrap_or_default()
        )));
        note.add_css_class("settings-note");
        note.set_wrap(true);
        note.set_xalign(0.0);
        note.set_max_width_chars(48);
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
