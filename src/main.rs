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
mod web;

use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

pub const APP_ID: &str = "dev.docview.app";

/// Variables `lean_env` set (not the user): removed again for programs docview launches.
static LEAN_VARS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();

/// Whether web pages may composite on the GPU (see `lean_env`). Unset (DOCVIEW_GPU): yes.
pub static WEB_GPU: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Mesa's EGL vendor file (glvnd), in the places distributions put it.
const MESA_EGL: &[&str] = &["/usr/share/glvnd/egl_vendor.d/50_mesa.json", "/etc/glvnd/egl_vendor.d/50_mesa.json"];

/// Environment for a small footprint. Set before GTK starts; `DOCVIEW_GPU=1` skips it.
fn lean_env() {
    let mut set = Vec::new();
    if std::env::var_os("DOCVIEW_GPU").is_none() {
        let mut def = |k: &'static str, v: &str| {
            if std::env::var_os(k).is_none() {
                // SAFETY: called first thing in main, before any thread exists.
                unsafe { std::env::set_var(k, v) };
                set.push(k);
            }
        };
        // Vulkan on the integrated GPU: compositing a HiDPI window in software (the cairo
        // renderer) takes ~10-20 ms a frame, so scrolling stutters on a 120 Hz screen.
        // Vulkan drivers are small (Intel's ~10 MB, no LLVM); GSK's GL renderer would load LLVM and
        // cost ~90 MB more. The NVIDIA driver stays out (huge, and it would wake a sleeping
        // discrete GPU on hybrid laptops), so does lavapipe (software Vulkan, LLVM), and so do
        // implicit layers (NVIDIA's Optimus layer loads its GL library). Without a usable
        // driver GTK falls back to the cairo renderer. The cost is graphics memory, which PSS
        // does not show (GTK's 4-image swapchain + GPU copies of tiles); `gpu = false` in
        // the config trades smoothness for it.
        let mut web_gpu = false;
        if config::load().app.gpu {
            def("GSK_RENDERER", "vulkan");
            def("VK_LOADER_DRIVERS_DISABLE", "*nvidia*,*lvp*");
            def("VK_LOADER_LAYERS_DISABLE", "~implicit~");
            // GL stays available for web pages: WebKit composites and scrolls on the GPU
            // (its own scrolling thread) only when GTK has GL, otherwise it repaints the page
            // on the CPU for every scroll step, which stutters. GTK sets GL up only when the
            // first web view asks for it, so PDFs never load the GL driver. Only Mesa's EGL:
            // NVIDIA's would load its huge driver and wake a sleeping discrete GPU. Costs
            // ~25 MB here and ~25 MB in WebKit's web process while a page is open.
            match MESA_EGL.iter().find(|p| std::path::Path::new(p).exists()) {
                Some(p) => {
                    def("__EGL_VENDOR_LIBRARY_FILENAMES", p);
                    web_gpu = true;
                }
                None => {
                    def("GDK_DISABLE", "gl");
                    def("__EGL_VENDOR_LIBRARY_FILENAMES", "/dev/null");
                }
            }
        } else {
            def("GSK_RENDERER", "cairo");
            def("GDK_DISABLE", "gl,vulkan,dmabuf");
            // No EGL vendor libraries: WebKit would otherwise load the GPU drivers and LLVM
            // into this process (~55 MB) even with hardware acceleration off.
            def("__EGL_VENDOR_LIBRARY_FILENAMES", "/dev/null");
        }
        let _ = WEB_GPU.set(web_gpu);
        // No accessibility bus round-trips at startup when nothing listens.
        if std::env::var_os("DOCVIEW_A11Y").is_none() {
            def("GTK_A11Y", "none");
        }
    }
    let _ = LEAN_VARS.set(set);
    // glibc gives every thread that allocates its own arena and keeps freed tile memory in
    // them. Two arenas are plenty (the env var would be read too late, so set it directly).
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
    }
}

/// A new docview process for `path`, with the environment this one started with.
pub fn spawn_self(path: &std::path::Path) {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap_or_else(|_| "docview".into()));
    for v in LEAN_VARS.get().into_iter().flatten() {
        cmd.env_remove(v);
    }
    let _ = cmd.arg(path).spawn();
}

/// Open a link in the user's default app (browser, mail, ...), without docview's
/// rendering overrides in its environment.
pub fn launch_uri(widget: &impl IsA<gtk::Widget>, uri: &str) {
    let ctx = widget.display().app_launch_context();
    for v in LEAN_VARS.get().into_iter().flatten() {
        ctx.unsetenv(v);
    }
    let uri = uri.to_string();
    glib::spawn_future_local(async move {
        if let Err(e) = gio::AppInfo::launch_default_for_uri_future(&uri, Some(&ctx)).await {
            eprintln!("docview: could not open {uri}: {e}");
        }
    });
}

/// Hand freed heap memory back to the system (after a burst of tile rendering or a search).
pub fn trim_heap() {
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::malloc_trim(0);
    }
}

/// DOCVIEW_DEBUG=1: log commands, layout and drawing to stderr.
pub fn debug() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var_os("DOCVIEW_DEBUG").is_some())
}

pub static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn main() -> glib::ExitCode {
    START.get_or_init(std::time::Instant::now);
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
        // `docview https://...`: web addresses belong in the browser; docview only shows
        // local files.
        if let Some(f) = files.first()
            && f.path().is_none() {
                let ctx = gtk::gdk::Display::default().map(|d| d.app_launch_context());
                if let Some(c) = &ctx {
                    for v in LEAN_VARS.get().into_iter().flatten() {
                        c.unsetenv(v);
                    }
                }
                if let Err(e) = gio::AppInfo::launch_default_for_uri(&f.uri(), ctx.as_ref()) {
                    eprintln!("docview: could not open {}: {e}", f.uri());
                }
                return;
            }
        let app = app::App::new(g);
        start(&app, files.first().and_then(|f| f.path()));
    });
    gapp.run()
}

thread_local! {
    /// Open windows. Every signal handler holds only a weak reference to its `App`, so this
    /// list is what keeps each one alive; an entry is dropped when its window is destroyed.
    static APPS: std::cell::RefCell<Vec<Rc<app::App>>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn start(app: &Rc<app::App>, file: Option<std::path::PathBuf>) {
    APPS.with(|a| a.borrow_mut().push(app.clone()));
    app.window.connect_destroy(|w| {
        // try_with: windows are also destroyed while the list itself is dropped at exit.
        let _ = APPS.try_with(|a| {
            if let Ok(mut a) = a.try_borrow_mut() {
                a.retain(|x| x.window != *w);
            }
        });
    });
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
/// DOCVIEW_RUN="cmd;cmd;type:text;open:/path" runs command ids (or types into the focused entry) after
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
                } else if let Some(p) = c.strip_prefix("open:") {
                    a.open_path(std::path::Path::new(p), None);
                } else if let Some(n) = c.strip_prefix("bench:") {
                    // Frame cost: scroll a step and render the whole window offscreen, n times.
                    let n: u32 = n.parse().unwrap_or(60);
                    let _ = capture(&a);
                    glib::timeout_future(std::time::Duration::from_millis(500)).await;
                    let mut times = Vec::new();
                    for _ in 0..n {
                        a.run("scroll-down");
                        glib::timeout_future(std::time::Duration::from_millis(120)).await;
                        let t = thread_cpu_ms();
                        let _ = capture(&a);
                        times.push(thread_cpu_ms() - t);
                    }
                    times.sort_by(|x, y| x.total_cmp(y));
                    eprintln!(
                        "bench: {n} frames, median {:.1} ms, p90 {:.1} ms, max {:.1} ms",
                        times[times.len() / 2],
                        times[times.len() * 9 / 10],
                        times[times.len() - 1]
                    );
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
                eprintln!("{}", a.debug_sizes());
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

fn thread_cpu_ms() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 * 1000.0 + ts.tv_nsec as f64 / 1e6
}

fn focused_entry(a: &Rc<app::App>) -> Option<gtk::Entry> {
    let f = gtk::prelude::GtkWindowExt::focus(&a.window)?;
    f.clone().downcast::<gtk::Entry>().ok().or_else(|| f.ancestor(gtk::Entry::static_type()).and_downcast::<gtk::Entry>())
}

fn capture(a: &Rc<app::App>) -> Option<gtk::gdk::Texture> {
    // DOCVIEW_SIZE overrides the window size (a tiled window ignores its default size).
    let (w, h) = std::env::var("DOCVIEW_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((a.window.width(), a.window.height()));
    let child = a.window.child()?;
    child.allocate(w, h, -1, None);
    // Render at the display's scale, so the PNG shows real device pixels.
    let k = a.window.surface().map(|s| s.scale()).unwrap_or(1.0) as f32;
    let snap = gtk::Snapshot::new();
    snap.scale(k, k);
    // The window's own CSS background is not part of its child's snapshot.
    if let Some(c) = a.window_bg() {
        snap.append_color(&c, &gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32));
    }
    snap.push_clip(&gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32));
    a.window.snapshot_child(&child, &snap);
    snap.pop();
    let node = snap.to_node()?;
    if let Ok(p) = std::env::var("DOCVIEW_NODE") {
        let _ = std::fs::write(p, node.serialize());
    }
    let vp = gtk::graphene::Rect::new(0.0, 0.0, w as f32 * k, h as f32 * k);
    Some(a.window.renderer()?.render_texture(&node, Some(&vp)))
}
