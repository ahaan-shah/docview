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
        // glibc keeps freed tile memory in per-thread arenas; two is plenty here.
        if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
            std::env::set_var("MALLOC_ARENA_MAX", "2");
        }
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
        glib::timeout_add_local_once(std::time::Duration::from_millis(delay / 2), move || {
            for c in cmds.split(';').map(str::trim).filter(|c| !c.is_empty()) {
                if let Some(text) = c.strip_prefix("type:") {
                    if let Some(f) = gtk::prelude::GtkWindowExt::focus(&a.window) {
                        if let Some(e) = f.dynamic_cast_ref::<gtk::Text>() {
                            e.set_text(text);
                            e.activate();
                        } else if let Some(e) = f.ancestor(gtk::Entry::static_type()).and_downcast::<gtk::Entry>() {
                            e.set_text(text);
                        }
                    }
                } else if let Some(id) = c.strip_prefix("enter:") {
                    let _ = id;
                    if let Some(f) = gtk::prelude::GtkWindowExt::focus(&a.window) {
                        if let Some(e) = f.ancestor(gtk::Entry::static_type()).and_downcast::<gtk::Entry>() {
                            e.emit_activate();
                        }
                    }
                } else {
                    a.run(c);
                }
            }
        });
    }
    if let Ok(out) = std::env::var("DOCVIEW_SNAPSHOT") {
        let a = app.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            let snap = gtk::Snapshot::new();
            // The window's own CSS background is not part of its child's snapshot.
            if let Some(c) = a.window_bg() {
                snap.append_color(&c, &gtk::graphene::Rect::new(0.0, 0.0, a.window.width() as f32, a.window.height() as f32));
            }
            if let Some(child) = a.window.child() {
                a.window.snapshot_child(&child, &snap);
            }
            match (snap.to_node(), a.window.renderer()) {
                (Some(node), Some(r)) => {
                    let tex = r.render_texture(&node, None);
                    if let Err(e) = tex.save_to_png(&out) {
                        eprintln!("snapshot: {e}");
                    }
                }
                (n, r) => eprintln!("snapshot: node {} renderer {}", n.is_some(), r.is_some()),
            }
            if std::env::var_os("DOCVIEW_SNAPSHOT_QUIT").is_some() {
                a.window.close();
            }
        });
    }
}
