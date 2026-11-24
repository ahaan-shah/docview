//! HTML documents through WebKitGTK 6. The library is loaded at run time (dlopen) the first
//! time an HTML file is opened: PDFs never load it, and docview neither builds against it nor
//! needs it installed. The C functions used are looked up once; properties and signals go
//! through plain GObject calls on the view.

use gtk::glib::{self, translate::*};
use gtk::prelude::*;
use std::cell::RefCell;
use std::ffi::{CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

type P = *mut c_void;

const LIB: &std::ffi::CStr = c"libwebkitgtk-6.0.so.4";

macro_rules! api {
    ($($name:ident : fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(non_snake_case)]
        struct Api { $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)* }
        fn load() -> Option<Api> {
            unsafe {
                let h = libc::dlopen(LIB.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
                if h.is_null() {
                    return None;
                }
                Some(Api { $($name: {
                    let name = concat!(stringify!($name), "\0");
                    let s = libc::dlsym(h, name.as_ptr() as *const c_char);
                    if s.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(s)
                },)* })
            }
        }
    };
}

api! {
    webkit_web_view_new: fn() -> P;
    webkit_web_view_load_uri: fn(P, *const c_char);
    webkit_web_view_go_back: fn(P);
    webkit_web_view_go_forward: fn(P);
    webkit_web_view_can_go_back: fn(P) -> i32;
    webkit_web_view_can_go_forward: fn(P) -> i32;
    webkit_web_view_reload: fn(P);
    webkit_web_view_execute_editing_command: fn(P, *const c_char);
    webkit_web_view_get_settings: fn(P) -> P;
    webkit_web_view_get_find_controller: fn(P) -> P;
    webkit_web_view_get_user_content_manager: fn(P) -> P;
    webkit_find_controller_search: fn(P, *const c_char, u32, u32);
    webkit_find_controller_count_matches: fn(P, *const c_char, u32, u32);
    webkit_find_controller_search_next: fn(P);
    webkit_find_controller_search_previous: fn(P);
    webkit_find_controller_search_finish: fn(P);
    webkit_settings_set_hardware_acceleration_policy: fn(P, i32);
    webkit_settings_set_enable_back_forward_navigation_gestures: fn(P, i32);
    webkit_web_context_get_default: fn() -> P;
    webkit_web_context_set_cache_model: fn(P, i32);
    webkit_user_style_sheet_new: fn(*const c_char, i32, i32, *const *const c_char, *const *const c_char) -> P;
    webkit_user_style_sheet_unref: fn(P);
    webkit_user_content_manager_add_style_sheet: fn(P, P);
    webkit_user_content_manager_remove_all_style_sheets: fn(P);
    webkit_print_operation_new: fn(P) -> P;
    webkit_print_operation_run_dialog: fn(P, P) -> i32;
    webkit_hit_test_result_get_link_uri: fn(P) -> *const c_char;
    webkit_navigation_policy_decision_get_navigation_action: fn(P) -> P;
    webkit_navigation_action_get_request: fn(P) -> P;
    webkit_uri_request_get_uri: fn(P) -> *const c_char;
    webkit_policy_decision_ignore: fn(P);
    webkit_network_session_get_default: fn() -> P;
    webkit_network_proxy_settings_new: fn(*const c_char, *const *const c_char) -> P;
    webkit_network_proxy_settings_free: fn(P);
    webkit_network_session_set_proxy_settings: fn(P, i32, P);
    webkit_permission_request_deny: fn(P);
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(load).as_ref()
}

/// Files shown as web pages: by extension, or by content for files without one.
pub fn is_html(path: &Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "html" | "htm" | "xhtml" | "xht" | "shtml") {
        return true;
    }
    if ext == "pdf" {
        return false;
    }
    let mut head = [0u8; 512];
    let n = std::fs::File::open(path).and_then(|mut f| std::io::Read::read(&mut f, &mut head)).unwrap_or(0);
    let s = String::from_utf8_lossy(&head[..n]).to_ascii_lowercase();
    let s = s.trim_start_matches('\u{feff}').trim_start();
    s.starts_with("<!doctype html") || s.starts_with("<html")
}

pub fn file_uri(path: &Path) -> String {
    gtk::gio::File::for_path(path).uri().to_string()
}

/// Schemes a page may load in docview. Everything else (http, https, ftp, mailto, ...) is
/// handed to the user's browser or mail app instead.
pub fn is_local(uri: &str) -> bool {
    let scheme = uri.split_once(':').map(|(s, _)| s.to_ascii_lowercase()).unwrap_or_default();
    matches!(scheme.as_str(), "file" | "about" | "data" | "blob")
}

/// Unreachable proxy for every network request: pages get no network at all (no remote
/// images, scripts, fetch, beacons or websockets), so a page can neither download anything
/// nor send anything out. Port 9 on loopback ("discard") refuses connections.
const NO_NETWORK_PROXY: &std::ffi::CStr = c"http://127.0.0.1:9";

/// WebKit features a document viewer does not need, off to shrink the attack surface.
/// WebRTC matters most: its UDP traffic would not go through the proxy.
const SETTINGS_OFF: &[&str] = &[
    "enable-webrtc",
    "enable-media-stream",
    "enable-webgl",
    "enable-webaudio",
    "enable-encrypted-media",
    "enable-mediasource",
    "enable-dns-prefetching",
    "enable-offline-web-application-cache",
    "enable-page-cache",
    "enable-developer-extras",
    "javascript-can-access-clipboard",
    "javascript-can-open-windows-automatically",
    "allow-file-access-from-file-urls",
    "allow-universal-access-from-file-urls",
];

/// Inverts the page for night mode and inverts media back, the usual "dark reader" trick.
const NIGHT_CSS: &str = "html { filter: invert(1) hue-rotate(180deg) !important; background: #fff !important; } \
     img, video, picture, canvas, iframe, embed, object, svg image, [style*=\"background-image\"] \
     { filter: invert(1) hue-rotate(180deg) !important; }";

pub struct Web {
    pub widget: gtk::Widget,
    api: &'static Api,
    find: glib::Object,
    print_op: RefCell<Option<glib::Object>>,
}

type ChooseConfig = unsafe extern "C" fn(P, *const i32, *mut P, i32, *mut i32) -> u32;
static REAL_CHOOSE: OnceLock<usize> = OnceLock::new();

const EGL_NONE: i32 = 0x3038;
const EGL_RENDERABLE_TYPE: i32 = 0x3040;
const EGL_OPENGL_ES2_BIT: i32 = 0x4;
const EGL_OPENGL_BIT: i32 = 0x8;

/// eglChooseConfig with a renderable type when the caller gave none. GTK (4.24) asks without
/// one, so EGL assumes OpenGL ES 1, which Mesa (26.3) no longer offers: GTK then gets no
/// configs, has no GL, and WebKit falls back to repainting the page on the CPU for every
/// scroll step (it composites on the GPU only when GTK has GL).
unsafe extern "C" fn choose_config(dpy: P, attrs: *const i32, configs: *mut P, size: i32, n: *mut i32) -> u32 {
    let real: ChooseConfig = unsafe { std::mem::transmute(*REAL_CHOOSE.get().unwrap()) };
    let mut list = Vec::new();
    let mut has_type = false;
    if !attrs.is_null() {
        let mut i = 0;
        loop {
            let k = unsafe { *attrs.add(i) };
            if k == EGL_NONE {
                break;
            }
            has_type |= k == EGL_RENDERABLE_TYPE;
            list.push(k);
            list.push(unsafe { *attrs.add(i + 1) });
            i += 2;
        }
    }
    if !has_type {
        for bits in [EGL_OPENGL_BIT | EGL_OPENGL_ES2_BIT, EGL_OPENGL_ES2_BIT] {
            let mut l = list.clone();
            l.extend([EGL_RENDERABLE_TYPE, bits, EGL_NONE]);
            let mut count = 0;
            if unsafe { real(dpy, l.as_ptr(), std::ptr::null_mut(), 0, &mut count) } != 0 && count > 0 {
                return unsafe { real(dpy, l.as_ptr(), configs, size, n) };
            }
        }
    }
    unsafe { real(dpy, attrs, configs, size, n) }
}

/// Route GTK's eglChooseConfig (through libepoxy's dispatch pointer) to `choose_config`.
/// Must run before GTK initializes GL, i.e. before the first web view.
fn fix_egl_configs() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| unsafe {
        let epoxy = libc::dlopen(c"libepoxy.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
        let egl = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if epoxy.is_null() || egl.is_null() {
            return;
        }
        let slot = libc::dlsym(epoxy, c"epoxy_eglChooseConfig".as_ptr()) as *mut usize;
        let real = libc::dlsym(egl, c"eglChooseConfig".as_ptr());
        if slot.is_null() || real.is_null() {
            return;
        }
        let _ = REAL_CHOOSE.set(real as usize);
        *slot = choose_config as ChooseConfig as usize;
    });
}

impl Web {
    /// None when WebKitGTK 6 is not installed.
    pub fn new() -> Option<Web> {
        let api = api()?;
        fix_egl_configs();
        unsafe {
            let ctx = (api.webkit_web_context_get_default)();
            if !ctx.is_null() {
                // Smallest caches: we show one document, not a browsing session.
                (api.webkit_web_context_set_cache_model)(ctx, 0);
            }
            let session = (api.webkit_network_session_get_default)();
            if session.is_null() {
                return None; // never show pages without the network cut off
            }
            let proxy = (api.webkit_network_proxy_settings_new)(NO_NETWORK_PROXY.as_ptr(), std::ptr::null());
            (api.webkit_network_session_set_proxy_settings)(session, 2, proxy);
            (api.webkit_network_proxy_settings_free)(proxy);
            let raw = (api.webkit_web_view_new)();
            if raw.is_null() {
                return None;
            }
            let widget: gtk::Widget = from_glib_none(raw as *mut gtk::ffi::GtkWidget);
            let settings = (api.webkit_web_view_get_settings)(raw);
            // 0 = always, 1 = never. GPU compositing scrolls smoothly (see `lean_env`);
            // without GL (the `gpu = false` config) WebKit paints on the CPU.
            let gpu = *crate::WEB_GPU.get().unwrap_or(&true);
            (api.webkit_settings_set_hardware_acceleration_policy)(settings, if gpu { 0 } else { 1 });
            (api.webkit_settings_set_enable_back_forward_navigation_gestures)(settings, 1);
            let so: glib::Object = from_glib_none(settings as *mut glib::gobject_ffi::GObject);
            for name in SETTINGS_OFF {
                if so.find_property(name).is_some() {
                    so.set_property(name, false);
                }
            }
            let find: glib::Object = from_glib_none((api.webkit_web_view_get_find_controller)(raw) as *mut glib::gobject_ffi::GObject);
            widget.set_hexpand(true);
            widget.set_vexpand(true);
            // Location, notifications, camera, ...: always no.
            widget.connect_local("permission-request", false, move |v| {
                if let Ok(req) = v[1].get::<glib::Object>() {
                    (api.webkit_permission_request_deny)(req.as_ptr() as P);
                }
                Some(true.to_value())
            });
            Some(Web { widget, api, find, print_op: RefCell::new(None) })
        }
    }

    fn ptr(&self) -> P {
        self.widget.as_ptr() as P
    }

    pub fn load_uri(&self, uri: &str) {
        let c = CString::new(uri).unwrap_or_default();
        unsafe { (self.api.webkit_web_view_load_uri)(self.ptr(), c.as_ptr()) };
    }

    pub fn load_file(&self, path: &Path) {
        self.load_uri(&file_uri(path));
    }

    pub fn back(&self) {
        unsafe { (self.api.webkit_web_view_go_back)(self.ptr()) }
    }

    pub fn forward(&self) {
        unsafe { (self.api.webkit_web_view_go_forward)(self.ptr()) }
    }

    pub fn can_go_back(&self) -> bool {
        unsafe { (self.api.webkit_web_view_can_go_back)(self.ptr()) != 0 }
    }

    pub fn can_go_forward(&self) -> bool {
        unsafe { (self.api.webkit_web_view_can_go_forward)(self.ptr()) != 0 }
    }

    pub fn reload(&self) {
        unsafe { (self.api.webkit_web_view_reload)(self.ptr()) }
    }

    pub fn zoom(&self) -> f64 {
        self.widget.property::<f64>("zoom-level")
    }

    pub fn set_zoom(&self, z: f64) {
        self.widget.set_property("zoom-level", z.clamp(0.25, 5.0));
    }

    pub fn title(&self) -> Option<String> {
        self.widget.property::<Option<String>>("title").filter(|t| !t.trim().is_empty())
    }

    pub fn uri(&self) -> Option<String> {
        self.widget.property::<Option<String>>("uri")
    }

    pub fn is_loading(&self) -> bool {
        self.widget.property::<bool>("is-loading")
    }

    /// The local file currently shown, if the page is a file.
    pub fn path(&self) -> Option<PathBuf> {
        let uri = self.uri()?;
        uri.starts_with("file:").then(|| gtk::gio::File::for_uri(&uri).path()).flatten()
    }

    pub fn editing(&self, command: &str) {
        let c = CString::new(command).unwrap_or_default();
        unsafe { (self.api.webkit_web_view_execute_editing_command)(self.ptr(), c.as_ptr()) };
    }

    pub fn find(&self, text: &str, case: bool) {
        let fc = self.find.as_ptr() as P;
        if text.is_empty() {
            unsafe { (self.api.webkit_find_controller_search_finish)(fc) };
            return;
        }
        let c = CString::new(text).unwrap_or_default();
        let opts = (1 << 4) | if case { 0 } else { 1 };
        unsafe {
            (self.api.webkit_find_controller_search)(fc, c.as_ptr(), opts, 1000);
            (self.api.webkit_find_controller_count_matches)(fc, c.as_ptr(), opts, 1000);
        }
    }

    pub fn find_step(&self, dir: i32) {
        let fc = self.find.as_ptr() as P;
        unsafe {
            if dir < 0 { (self.api.webkit_find_controller_search_previous)(fc) } else { (self.api.webkit_find_controller_search_next)(fc) }
        }
    }

    pub fn find_finish(&self) {
        unsafe { (self.api.webkit_find_controller_search_finish)(self.find.as_ptr() as P) };
    }

    /// Match counts: Some(n) after a search or count, None when nothing was found.
    pub fn connect_find(&self, f: impl Fn(Option<u32>) + Clone + 'static) {
        let g = f.clone();
        self.find.connect_local("counted-matches", false, move |v| {
            g(Some(v[1].get::<u32>().unwrap_or(0)).filter(|n| *n > 0));
            None
        });
        self.find.connect_local("failed-to-find-text", false, move |_| {
            f(None);
            None
        });
    }

    pub fn set_night(&self, on: bool) {
        unsafe {
            let ucm = (self.api.webkit_web_view_get_user_content_manager)(self.ptr());
            (self.api.webkit_user_content_manager_remove_all_style_sheets)(ucm);
            if on {
                let css = CString::new(NIGHT_CSS).unwrap();
                let sheet = (self.api.webkit_user_style_sheet_new)(css.as_ptr(), 0, 0, std::ptr::null(), std::ptr::null());
                (self.api.webkit_user_content_manager_add_style_sheet)(ucm, sheet);
                (self.api.webkit_user_style_sheet_unref)(sheet);
            }
        }
    }

    pub fn print(&self, parent: &gtk::Window) {
        unsafe {
            let op = (self.api.webkit_print_operation_new)(self.ptr());
            if op.is_null() {
                return;
            }
            // Keep the operation alive until printing is done.
            let obj: glib::Object = from_glib_full(op as *mut glib::gobject_ffi::GObject);
            self.print_op.replace(Some(obj));
            (self.api.webkit_print_operation_run_dialog)(op, parent.as_ptr() as P);
        }
    }

    /// The link under the pointer (for the status bar), or None.
    pub fn connect_hover(&self, f: impl Fn(Option<String>) + 'static) {
        let api = self.api;
        self.widget.connect_local("mouse-target-changed", false, move |v| {
            let link = v[1].get::<glib::Object>().ok().and_then(|hit| unsafe {
                let s = (api.webkit_hit_test_result_get_link_uri)(hit.as_ptr() as P);
                (!s.is_null()).then(|| std::ffi::CStr::from_ptr(s).to_string_lossy().to_string())
            });
            f(link);
            None
        });
    }

    /// Navigation policy. Only local pages load here (`is_local`); any other link is
    /// refused and passed to `on_external` (open it in the browser). Links that open a new
    /// window load here instead, and `on_link` may take over a local link (returns true to
    /// stop WebKit from loading it), e.g. a PDF.
    pub fn connect_links(&self, on_link: impl Fn(&str) -> bool + 'static, on_external: impl Fn(&str) + 'static) {
        let api = self.api;
        let view = self.widget.downgrade();
        self.widget.connect_local("decide-policy", false, move |v| {
            let kind = unsafe { glib::gobject_ffi::g_value_get_enum(v[2].to_glib_none().0) };
            if kind > 1 {
                return Some(false.to_value());
            }
            let Ok(decision) = v[1].get::<glib::Object>() else { return Some(false.to_value()) };
            let d = decision.as_ptr() as P;
            let uri = unsafe {
                let action = (api.webkit_navigation_policy_decision_get_navigation_action)(d);
                let req = if action.is_null() { std::ptr::null_mut() } else { (api.webkit_navigation_action_get_request)(action) };
                let s = if req.is_null() { std::ptr::null() } else { (api.webkit_uri_request_get_uri)(req) };
                (!s.is_null()).then(|| std::ffi::CStr::from_ptr(s).to_string_lossy().to_string())
            };
            let Some(uri) = uri else {
                unsafe { (api.webkit_policy_decision_ignore)(d) };
                return Some(true.to_value());
            };
            if !is_local(&uri) {
                unsafe { (api.webkit_policy_decision_ignore)(d) };
                on_external(&uri);
                return Some(true.to_value());
            }
            if on_link(&uri) {
                unsafe { (api.webkit_policy_decision_ignore)(d) };
                return Some(true.to_value());
            }
            if kind == 1 {
                // target=_blank / window.open: one view, so load it here.
                unsafe { (api.webkit_policy_decision_ignore)(d) };
                if let Some(w) = view.upgrade() {
                    let c = CString::new(uri).unwrap_or_default();
                    unsafe { (api.webkit_web_view_load_uri)(w.as_ptr() as P, c.as_ptr()) };
                }
                return Some(true.to_value());
            }
            Some(false.to_value())
        });
    }

    /// Title, address, history, zoom or loading state changed.
    pub fn connect_changed(&self, f: impl Fn() + Clone + 'static) {
        for prop in ["title", "uri", "zoom-level", "is-loading"] {
            let f = f.clone();
            self.widget.connect_notify_local(Some(prop), move |_, _| f());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_detection() {
        let dir = std::env::temp_dir().join(format!("docview-web-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("page");
        std::fs::write(&a, "\u{feff}  <!DOCTYPE html><title>x</title>").unwrap();
        let b = dir.join("notes");
        std::fs::write(&b, "%PDF-1.7").unwrap();
        assert!(is_html(&a));
        assert!(!is_html(&b));
        assert!(is_html(Path::new("/nonexistent/x.HTM")));
        assert!(!is_html(Path::new("/nonexistent/x.pdf")));
        assert!(is_local("file:///tmp/a.html#top"));
        assert!(is_local("about:blank"));
        assert!(!is_local("https://example.com"));
        assert!(!is_local("HTTP://example.com"));
        assert!(!is_local("mailto:someone@example.com"));
        assert!(!is_local("javascript-ish"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
