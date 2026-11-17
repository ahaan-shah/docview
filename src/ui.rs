//! Modal layer (palette / picker, dialogs, settings, shortcuts) and toasts, mirroring
//! allwrite's overlay.ts and palette.ts.

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub struct Item {
    pub label: String,
    pub hint: Option<String>,
    /// The hint is a key combo (drawn as key caps) rather than plain text.
    pub is_key: bool,
    pub run: Rc<dyn Fn()>,
}

impl Item {
    pub fn new(label: impl Into<String>, hint: Option<String>, run: impl Fn() + 'static) -> Item {
        Item { label: label.into(), hint, is_key: false, run: Rc::new(run) }
    }
    pub fn command(label: impl Into<String>, keys: Option<String>, run: impl Fn() + 'static) -> Item {
        Item { label: label.into(), hint: keys, is_key: true, run: Rc::new(run) }
    }
}

/// Subsequence match; higher is better, None is no match. Same scoring as allwrite.
pub fn fuzzy_score(query: &str, text: &str) -> Option<f64> {
    if query.is_empty() {
        return Some(0.0);
    }
    let q = query.to_lowercase();
    let t = text.to_lowercase();
    let is_word = |c: Option<char>| c.map(|c| !c.is_alphanumeric()).unwrap_or(true);
    if let Some(idx) = t.find(&q) {
        let before = t[..idx].chars().last();
        let bonus = if idx == 0 || is_word(before) { 50.0 } else { 0.0 };
        return Some(1000.0 - idx as f64 - (t.len() - q.len()) as f64 * 0.01 + bonus);
    }
    let tc: Vec<char> = t.chars().collect();
    let (mut score, mut ti, mut streak) = (0.0, 0usize, 0.0);
    for ch in q.chars() {
        if ch == ' ' {
            continue;
        }
        let found = (ti..tc.len()).find(|&i| tc[i] == ch)?;
        streak = if found == ti { streak + 1.0 } else { 0.0 };
        let boundary = found == 0 || !tc[found - 1].is_alphanumeric();
        score += 1.0 + streak * 2.0 + if boundary { 4.0 } else { 0.0 } - (found - ti) as f64 * 0.05;
        ti = found + 1;
    }
    Some(score)
}

pub fn filter<'a>(items: &'a [Item], query: &str, limit: usize) -> Vec<&'a Item> {
    let q = query.trim();
    if q.is_empty() {
        return items.iter().take(limit).collect();
    }
    let mut scored: Vec<(f64, usize, &Item)> =
        items.iter().enumerate().filter_map(|(i, it)| fuzzy_score(q, &it.label).map(|s| (s, i, it))).collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|x| x.2).collect()
}

/// The modal layer. Lives in the window's overlay; one panel at a time.
pub struct Modal {
    pub scrim: gtk::Box,
    current: RefCell<Option<(gtk::Widget, Option<Box<dyn Fn()>>)>>,
    /// Where focus returns when the panel closes.
    pub return_focus: RefCell<Option<gtk::Widget>>,
}

impl Modal {
    pub fn new() -> Rc<Modal> {
        let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
        scrim.add_css_class("scrim");
        scrim.set_visible(false);
        scrim.set_hexpand(true);
        scrim.set_vexpand(true);
        let m = Rc::new(Modal { scrim, current: RefCell::new(None), return_focus: RefCell::new(None) });
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&m);
        click.connect_pressed(move |g, _, x, y| {
            let Some(m) = weak.upgrade() else { return };
            let inside = m.current.borrow().as_ref().map(|(w, _)| {
                let Some(b) = w.compute_bounds(&m.scrim) else { return false };
                b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
            });
            if inside == Some(false) {
                g.set_state(gtk::EventSequenceState::Claimed);
                m.close();
            }
        });
        m.scrim.add_controller(click);
        m
    }

    pub fn is_open(&self) -> bool {
        self.current.borrow().is_some()
    }

    /// Show a panel. `on_close` runs when it closes (Escape, backdrop click or `close`).
    pub fn open(&self, panel: &impl IsA<gtk::Widget>, on_close: Option<Box<dyn Fn()>>) {
        self.close();
        let panel = panel.as_ref().clone();
        // Never wider than the window (a size request wider than the overlay spills out).
        let win_w = self.scrim.root().map(|r| r.width()).unwrap_or(1000);
        let (req_w, req_h) = panel.size_request();
        if req_w > win_w - 32 {
            panel.set_size_request((win_w - 32).max(200), req_h);
        }
        panel.set_halign(gtk::Align::Center);
        panel.set_valign(gtk::Align::Start);
        panel.set_overflow(gtk::Overflow::Hidden);
        let h = self.scrim.root().map(|r| r.height()).unwrap_or(700);
        panel.set_margin_top((h as f64 * 0.12) as i32);
        self.scrim.append(&panel);
        self.scrim.set_visible(true);
        self.current.replace(Some((panel, on_close)));
    }

    pub fn close(&self) {
        let cur = self.current.take();
        if let Some((w, on_close)) = cur {
            self.scrim.remove(&w);
            self.scrim.set_visible(false);
            if let Some(f) = on_close {
                f();
            }
            if let Some(f) = self.return_focus.borrow().as_ref() {
                f.grab_focus();
            }
        }
    }
}

fn keys_row(keys: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("keys");
    for (i, combo) in keys.split(" or ").enumerate() {
        if i > 0 {
            let or = gtk::Label::new(Some("or"));
            or.add_css_class("or");
            row.append(&or);
        }
        // "Ctrl+Wheel" is two caps; "Double-click" is one.
        for part in combo.split('+') {
            let k = gtk::Label::new(Some(&pretty_key(part)));
            k.add_css_class("kbd");
            row.append(&k);
        }
    }
    row
}

pub fn pretty_key(k: &str) -> String {
    match k {
        "PageUp" => "PgUp".into(),
        "PageDown" => "PgDn".into(),
        "Up" => "↑".into(),
        "Down" => "↓".into(),
        "Left" => "←".into(),
        "Right" => "→".into(),
        "Escape" => "Esc".into(),
        "Backspace" => "⌫".into(),
        "Enter" => "Enter".into(),
        k => k.to_string(),
    }
}

/// Searchable list: the command palette and every picker (recent files, sections, zoom…).
/// `free` handles Enter when the typed text matches nothing (e.g. "150" in the zoom picker).
pub fn open_picker(modal: &Rc<Modal>, items: Vec<Item>, placeholder: &str, free: Option<Rc<dyn Fn(&str) -> bool>>, limit: usize) {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("palette");
    panel.set_size_request(540, -1);
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_propagate_natural_height(true);
    scroller.set_max_content_height(420);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(true);
    scroller.set_child(Some(&list));
    let empty = gtk::Label::new(Some("No matches"));
    empty.add_css_class("palette-empty");
    empty.set_xalign(0.0);
    empty.set_visible(false);
    panel.append(&entry);
    panel.append(&scroller);
    panel.append(&empty);

    let items = Rc::new(items);
    let shown: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let rebuild = {
        let (items, shown, list, empty) = (items.clone(), shown.clone(), list.clone(), empty.clone());
        move |q: &str| {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let base = items.as_ptr();
            let picked: Vec<usize> =
                filter(&items, q, limit).into_iter().map(|it| unsafe { (it as *const Item).offset_from(base) as usize }).collect();
            for &i in &picked {
                let it = &items[i];
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let l = gtk::Label::new(Some(&it.label));
                l.set_xalign(0.0);
                l.set_hexpand(true);
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                row.append(&l);
                if let Some(h) = &it.hint {
                    let k = gtk::Label::new(Some(h));
                    k.add_css_class(if it.is_key { "kbd" } else { "hint" });
                    row.append(&k);
                }
                list.append(&row);
            }
            empty.set_visible(picked.is_empty());
            list.set_visible(!picked.is_empty());
            if let Some(r) = list.row_at_index(0) {
                list.select_row(Some(&r));
            }
            shown.replace(picked);
        }
    };
    rebuild("");
    let choose: Rc<dyn Fn(Option<usize>)> = {
        let (items, shown, modal, entry) = (items.clone(), shown.clone(), Rc::downgrade(modal), entry.clone());
        Rc::new(move |row: Option<usize>| {
            let Some(m) = modal.upgrade() else { return };
            let target = row.and_then(|r| shown.borrow().get(r).copied());
            match target {
                Some(i) => {
                    let run = items[i].run.clone();
                    m.close();
                    run();
                }
                None => {
                    if let Some(f) = &free {
                        let text = entry.text().to_string();
                        m.close();
                        f(&text);
                    }
                }
            }
        })
    };
    {
        let rebuild = rebuild.clone();
        entry.connect_changed(move |e| rebuild(&e.text()));
    }
    {
        let (choose, list) = (choose.clone(), list.clone());
        entry.connect_activate(move |_| {
            let idx = list.selected_row().map(|r| r.index() as usize);
            choose(idx);
        });
    }
    {
        let choose = choose.clone();
        list.connect_row_activated(move |_, row| choose(Some(row.index() as usize)));
    }
    let keys = gtk::EventControllerKey::new();
    {
        let (list, scroller) = (list.clone(), scroller.clone());
        keys.connect_key_pressed(move |_, key, _, state| {
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            let step: i32 = match key {
                gdk::Key::Down | gdk::Key::KP_Down => 1,
                gdk::Key::Up | gdk::Key::KP_Up => -1,
                gdk::Key::Page_Down => 8,
                gdk::Key::Page_Up => -8,
                gdk::Key::n if ctrl => 1,
                gdk::Key::p if ctrl => -1,
                gdk::Key::j if ctrl => 1,
                gdk::Key::k if ctrl => -1,
                _ => return glib::Propagation::Proceed,
            };
            let mut n = 0;
            while list.row_at_index(n).is_some() {
                n += 1;
            }
            if n == 0 {
                return glib::Propagation::Stop;
            }
            let cur = list.selected_row().map(|r| r.index()).unwrap_or(0);
            let next = (cur + step).clamp(0, n - 1);
            if let Some(r) = list.row_at_index(next) {
                list.select_row(Some(&r));
                // keep it in view
                if let Some(b) = r.compute_bounds(&list) {
                    let adj = scroller.vadjustment();
                    let (top, bot) = (b.y() as f64, (b.y() + b.height()) as f64);
                    if top < adj.value() {
                        adj.set_value(top);
                    } else if bot > adj.value() + adj.page_size() {
                        adj.set_value(bot - adj.page_size());
                    }
                }
            }
            glib::Propagation::Stop
        });
    }
    entry.add_controller(keys);
    modal.open(&panel, None);
    entry.grab_focus();
}

/// Message with buttons. `choices` are (label, primary, action).
pub fn ask(modal: &Rc<Modal>, message: &str, choices: Vec<(&str, bool, Rc<dyn Fn()>)>) {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("dialog");
    panel.set_size_request(420, -1);
    let msg = gtk::Label::new(Some(message));
    msg.add_css_class("msg");
    msg.set_wrap(true);
    msg.set_xalign(0.0);
    panel.append(&msg);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("buttons");
    row.set_halign(gtk::Align::End);
    let mut primary = None;
    for (label, is_primary, run) in choices {
        let b = gtk::Button::with_label(label);
        b.add_css_class("btn");
        if is_primary {
            b.add_css_class("primary");
            primary = Some(b.clone());
        }
        let m = Rc::downgrade(modal);
        b.connect_clicked(move |_| {
            if let Some(m) = m.upgrade() {
                m.close();
            }
            run();
        });
        row.append(&b);
    }
    panel.append(&row);
    modal.open(&panel, None);
    if let Some(b) = primary {
        b.grab_focus();
    }
}

/// Ask for a line of text (password prompt). `on_submit` gets the text.
pub fn prompt(modal: &Rc<Modal>, message: &str, password: bool, on_submit: Rc<dyn Fn(String)>) {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("dialog");
    panel.set_size_request(420, -1);
    let msg = gtk::Label::new(Some(message));
    msg.add_css_class("msg");
    msg.set_wrap(true);
    msg.set_xalign(0.0);
    let entry = if password { gtk::PasswordEntry::new().upcast::<gtk::Widget>() } else { gtk::Entry::new().upcast() };
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("buttons");
    row.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("btn");
    let ok = gtk::Button::with_label("Open");
    ok.add_css_class("btn");
    ok.add_css_class("primary");
    row.append(&cancel);
    row.append(&ok);
    panel.append(&msg);
    panel.append(&entry);
    panel.append(&row);
    let submit = {
        let (m, entry) = (Rc::downgrade(modal), entry.clone());
        Rc::new(move || {
            let text = entry.dynamic_cast_ref::<gtk::Editable>().map(|e| e.text().to_string()).unwrap_or_default();
            if let Some(m) = m.upgrade() {
                m.close();
            }
            on_submit(text);
        })
    };
    {
        let s = submit.clone();
        ok.connect_clicked(move |_| s());
    }
    {
        let m = Rc::downgrade(modal);
        cancel.connect_clicked(move |_| {
            if let Some(m) = m.upgrade() {
                m.close();
            }
        });
    }
    if let Some(pe) = entry.downcast_ref::<gtk::PasswordEntry>() {
        let s = submit.clone();
        pe.connect_activate(move |_| s());
    } else if let Some(e) = entry.downcast_ref::<gtk::Entry>() {
        let s = submit.clone();
        e.connect_activate(move |_| s());
    }
    modal.open(&panel, None);
    entry.grab_focus();
}

/// Keyboard shortcut cheat sheet: sections of (label, "Ctrl+X or Y").
pub fn shortcuts_panel(sections: Vec<(&'static str, Vec<(String, String)>)>, note: &str) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("shortcuts");
    panel.set_size_request(760, -1);
    let title = gtk::Label::new(Some("KEYBOARD SHORTCUTS"));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    panel.append(&title);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_propagate_natural_height(true);
    scroller.set_max_content_height(560);
    let cols = gtk::Box::new(gtk::Orientation::Horizontal, 32);
    cols.set_homogeneous(true);
    let left = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let right = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // Balance the columns by row count.
    let total: usize = sections.iter().map(|s| s.1.len() + 2).sum();
    let mut acc = 0;
    for (name, rows) in sections {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sec.add_css_class("section");
        let h = gtk::Label::new(Some(name));
        h.add_css_class("section-title");
        h.set_xalign(0.0);
        sec.append(&h);
        let n = rows.len() + 2;
        for (label, keys) in rows {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("shortcut");
            let l = gtk::Label::new(Some(&label));
            l.set_xalign(0.0);
            l.set_hexpand(true);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&l);
            row.append(&keys_row(&keys));
            sec.append(&row);
        }
        if acc < total / 2 { left.append(&sec) } else { right.append(&sec) }
        acc += n;
    }
    cols.append(&left);
    cols.append(&right);
    scroller.set_child(Some(&cols));
    panel.append(&scroller);
    let n = gtk::Label::new(Some(note));
    n.add_css_class("settings-note");
    n.set_xalign(0.0);
    n.set_wrap(true);
    n.set_max_width_chars(60);
    panel.append(&n);
    panel.set_focusable(true);
    panel
}

/// Toast at the bottom center, gone after a moment.
pub struct Toast {
    pub label: gtk::Label,
    timer: RefCell<Option<glib::SourceId>>,
}

impl Toast {
    pub fn new() -> Rc<Toast> {
        let label = gtk::Label::new(None);
        label.add_css_class("toast");
        label.set_halign(gtk::Align::Center);
        label.set_valign(gtk::Align::End);
        label.set_visible(false);
        label.set_can_target(false);
        label.set_wrap(true);
        label.set_max_width_chars(80);
        Rc::new(Toast { label, timer: RefCell::new(None) })
    }

    pub fn show(self: &Rc<Self>, text: &str) {
        self.label.set_text(text);
        self.label.set_visible(true);
        if let Some(t) = self.timer.take() {
            t.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(2400), move || {
            if let Some(t) = weak.upgrade() {
                t.timer.take();
                t.label.set_visible(false);
            }
        });
        self.timer.replace(Some(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_prefix_and_word_starts() {
        let a = fuzzy_score("zoom", "Zoom in").unwrap();
        let b = fuzzy_score("zoom", "Reset zoom").unwrap();
        assert!(a > b);
        assert!(fuzzy_score("fw", "Fit width").is_some());
        assert!(fuzzy_score("xyz", "Fit width").is_none());
    }

    #[test]
    fn filter_keeps_order_for_empty_query() {
        let items: Vec<Item> = ["b", "a", "c"].iter().map(|s| Item::new(*s, None, || {})).collect();
        let f = filter(&items, "", 2);
        assert_eq!(f.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), vec!["b", "a"]);
    }
}
