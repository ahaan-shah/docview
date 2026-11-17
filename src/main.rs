//! docview: a minimal, low-memory PDF viewer. Rust + GTK 4 + poppler.

mod app;
mod commands;
mod config;
mod icons;
mod keymap;
mod layout;
mod pdf;
mod render;
mod state;
mod style;
mod theme;
mod ui;
mod view;

use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

pub const APP_ID: &str = "dev.docview.app";

/// Environment for a small footprint. Set before GTK starts; `DOCVIEW_GPU=1` skips it.
fn lean_env() {
    if std::env::var_os("DOCVIEW_GPU").is_some() {
        return;
    }
    // SAFETY: called first thing in main, before any thread exists.
    unsafe {
        // Software rendering: the GL / Vulkan renderers pull in the GPU driver stack (tens of
        // MB per process, and on hybrid laptops they wake the discrete GPU). Pages are
        // pre-rendered tiles, so compositing them in software is cheap.
        if std::env::var_os("GSK_RENDERER").is_none() {
            std::env::set_var("GSK_RENDERER", "cairo");
        }
        if std::env::var_os("GDK_DISABLE").is_none() {
            std::env::set_var("GDK_DISABLE", "gl,vulkan,dmabuf");
        }
        // No accessibility bus round-trips at startup when nothing listens.
        if std::env::var_os("GTK_A11Y").is_none() && std::env::var_os("DOCVIEW_A11Y").is_none() {
            std::env::set_var("GTK_A11Y", "none");
        }
    }
    // glibc gives every thread that allocates its own arena and keeps freed tile memory in
    // them. Two arenas are plenty (the env var would be read too late, so set it directly).
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
    }
}

/// Hand freed heap memory back to the system (after a burst of tile rendering or a search).
pub fn trim_heap() {
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::malloc_trim(0);
    }
}

fn main() -> glib::ExitCode {
    lean_env();
    let gapp = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE | gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    gapp.connect_activate(|g| {
        let app = app::App::new(g);
        start(&app, None);
    });
    gapp.connect_open(|g, files, _| {
        let app = app::App::new(g);
        start(&app, files.first().and_then(|f| f.path()));
    });
    gapp.run()
}

fn start(app: &Rc<app::App>, file: Option<std::path::PathBuf>) {
    if let Some((w, h)) = std::env::var("DOCVIEW_SIZE").ok().and_then(|s| {
        let (w, h) = s.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    }) {
        app.window.set_default_size(w, h);
    }
    app.window.present();
    if let Some(p) = file {
        app.open_path(&p, None);
    }
    dev_hooks(app);
}

/// Test hooks that cost nothing when unset:
/// DOCVIEW_RUN="cmd;cmd;type:text" runs command ids (or types into the focused entry) after
/// startup; DOCVIEW_SNAPSHOT=/path.png writes a PNG of the window after DOCVIEW_SNAPSHOT_DELAY ms.
fn dev_hooks(app: &Rc<app::App>) {
    let delay: u64 = std::env::var("DOCVIEW_SNAPSHOT_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
    if let Ok(cmds) = std::env::var("DOCVIEW_RUN") {
        let a = app.clone();
        glib::spawn_future_local(async move {
            glib::timeout_future(std::time::Duration::from_millis(delay / 2)).await;
            for c in cmds.split(';').map(str::trim).filter(|c| !c.is_empty()) {
                if let Some(ms) = c.strip_prefix("sleep:") {
                    glib::timeout_future(std::time::Duration::from_millis(ms.parse().unwrap_or(100))).await;
                } else if let Some(text) = c.strip_prefix("type:") {
                    if let Some(e) = focused_entry(&a) {
                        e.set_text(text);
                    }
                } else if let Some(args) = c.strip_prefix("drag:").or(c.strip_prefix("click:")).or(c.strip_prefix("dclick:")) {
                    let v: Vec<f64> = args.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    let n = if c.starts_with("dclick:") { 2 } else { 1 };
                    if v.len() >= 2 {
                        a.simulate(n, (v[0], v[1]), (v.len() >= 4).then(|| (v[2], v[3])));
                    }
                } else if c == "enter:" {
                    if let Some(e) = focused_entry(&a) {
                        e.emit_activate();
                    }
                } else {
                    a.run(c);
                }
            }
            if std::env::var_os("DOCVIEW_DEBUG").is_some() {
                eprintln!("active buttons: {:?}", a.active_buttons());
            }
        });
    }
    if let Ok(out) = std::env::var("DOCVIEW_SNAPSHOT") {
        // A window on a hidden workspace gets no frame clock ticks, so lay out and draw by
        // hand: one pass to request tiles, a second (after they render) to capture.
        let a = app.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            let _ = capture(&a);
            let a2 = a.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
                match capture(&a2) {
                    Some(tex) => {
                        if let Err(e) = tex.save_to_png(&out) {
                            eprintln!("snapshot: {e}");
                        }
                    }
                    None => eprintln!("snapshot: nothing drawn"),
                }
                if std::env::var_os("DOCVIEW_SNAPSHOT_QUIT").is_some() {
                    a2.window.close();
                }
            });
        });
    }
}

fn focused_entry(a: &Rc<app::App>) -> Option<gtk::Entry> {
    let f = gtk::prelude::GtkWindowExt::focus(&a.window)?;
    f.clone().downcast::<gtk::Entry>().ok().or_else(|| f.ancestor(gtk::Entry::static_type()).and_downcast::<gtk::Entry>())
}

fn capture(a: &Rc<app::App>) -> Option<gtk::gdk::Texture> {
    let (w, h) = (a.window.width(), a.window.height());
    let child = a.window.child()?;
    child.allocate(w, h, -1, None);
    let snap = gtk::Snapshot::new();
    // The window's own CSS background is not part of its child's snapshot.
    if let Some(c) = a.window_bg() {
        snap.append_color(&c, &gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32));
    }
    a.window.snapshot_child(&child, &snap);
    let node = snap.to_node()?;
    Some(a.window.renderer()?.render_texture(&node, None))
}
