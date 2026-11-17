//! System palette: pywal (~/.cache/wal/colors.json) -> XDG portal -> built-in.


use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub source: &'static str,
    pub dark: bool,
    pub bg: String,
    pub fg: String,
    pub cursor: String,
    pub muted: String,
    pub accent: String,
    pub heading: String,
    /// color0..color15
    pub colors: Vec<String>,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct PortalState {
    /// 0 = no preference, 1 = dark, 2 = light
    pub scheme: Option<u32>,
    pub accent: Option<(f64, f64, f64)>,
}

pub fn wal_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("wal"))
}

pub fn wal_file() -> Option<PathBuf> {
    Some(wal_dir()?.join("colors.json"))
}

fn is_hex(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn luminance(hex: &str) -> f64 {
    let ch = |i: usize| {
        let v = u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(1) + 0.7152 * ch(3) + 0.0722 * ch(5)
}

struct Raw {
    background: String,
    foreground: String,
    cursor: String,
    colors: Vec<String>,
}

impl Raw {
    /// Resolve "background" | "foreground" | "cursor" | "colorN" | "#rrggbb".
    fn resolve(&self, key: &str) -> Option<String> {
        match key {
            "background" => Some(self.background.clone()),
            "foreground" => Some(self.foreground.clone()),
            "cursor" => Some(self.cursor.clone()),
            k if is_hex(k) => Some(k.to_string()),
            k => {
                let n: usize = k.strip_prefix("color")?.parse().ok()?;
                self.colors.get(n).cloned()
            }
        }
    }

    fn into_theme(self, source: &'static str, map: &BTreeMap<String, String>) -> Theme {
        let slot = |name: &str, default: &str| {
            map.get(name)
                .and_then(|k| self.resolve(k))
                .or_else(|| self.resolve(default))
                .unwrap_or_else(|| self.foreground.clone())
        };
        let bg = slot("bg", "background");
        Theme {
            source,
            dark: luminance(&bg) < 0.4,
            fg: slot("fg", "foreground"),
            cursor: slot("cursor", "cursor"),
            muted: slot("muted", "color8"),
            accent: slot("accent", "color4"),
            // Headings use the text color by default; the old teal read as "blue text".
            heading: slot("heading", "foreground"),
            bg,
            colors: self.colors,
        }
    }
}

pub fn parse_pywal(json: &str, map: &BTreeMap<String, String>) -> Option<Theme> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let get = |a: &str, b: &str| -> Option<String> {
        let s = v.get(a)?.get(b)?.as_str()?.to_lowercase();
        is_hex(&s).then_some(s)
    };
    let background = get("special", "background")?;
    let foreground = get("special", "foreground")?;
    let cursor = get("special", "cursor").unwrap_or_else(|| foreground.clone());
    let mut colors = Vec::with_capacity(16);
    for i in 0..16 {
        colors.push(get("colors", &format!("color{i}"))?);
    }
    Some(Raw { background, foreground, cursor, colors }.into_theme("pywal", map))
}

const DARK: [&str; 16] = [
    "#1b1d23", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#c8ccd4",
    "#636b7a", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#ffffff",
];
const LIGHT: [&str; 16] = [
    "#fbfaf8", "#c4423c", "#4f8a2e", "#a36a00", "#2f6fd0", "#8d44b8", "#1f8a94", "#33363d",
    "#8a8f99", "#c4423c", "#4f8a2e", "#a36a00", "#2f6fd0", "#8d44b8", "#1f8a94", "#000000",
];

pub fn builtin(portal: PortalState, source: &'static str, map: &BTreeMap<String, String>) -> Theme {
    let light = portal.scheme == Some(2);
    let pal = if light { LIGHT } else { DARK };
    let mut colors: Vec<String> = pal.iter().map(|s| s.to_string()).collect();
    if let Some((r, g, b)) = portal.accent {
        if [r, g, b].iter().all(|c| (0.0..=1.0).contains(c)) {
            let h = |c: f64| (c * 255.0).round() as u8;
            colors[4] = format!("#{:02x}{:02x}{:02x}", h(r), h(g), h(b));
        }
    }
    let fg = if light { "#24262b" } else { "#d7dae0" };
    Raw { background: colors[0].clone(), foreground: fg.into(), cursor: fg.into(), colors }
        .into_theme(source, map)
}

pub fn current(portal: PortalState, map: &BTreeMap<String, String>) -> Theme {
    if let Some(t) = wal_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|j| parse_pywal(&j, map))
    {
        return t;
    }
    let source = if portal.scheme.is_some() { "portal" } else { "builtin" };
    builtin(portal, source, map)
}

// ---- XDG desktop portal (only consulted when pywal is absent) ----

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.Settings";
const NS: &str = "org.freedesktop.appearance";

fn unwrap_variant(mut v: gtk::glib::Variant) -> gtk::glib::Variant {
    while v.type_().as_str() == "v" {
        match v.as_variant() {
            Some(inner) => v = inner,
            None => break,
        }
    }
    v
}

fn apply_setting(st: &mut PortalState, key: &str, value: gtk::glib::Variant) {
    let value = unwrap_variant(value);
    match key {
        "color-scheme" => st.scheme = value.get::<u32>(),
        "accent-color" => st.accent = value.get::<(f64, f64, f64)>(),
        _ => {}
    }
}

/// Must be called on the GTK main thread. `on_change` runs on that thread too.
pub fn init_portal(
    state: &'static std::sync::Mutex<PortalState>,
    on_change: impl Fn() + 'static,
) {
    use gtk::gio;
    use gtk::glib::prelude::*;

    let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return;
    };
    for key in ["color-scheme", "accent-color"] {
        let res = conn.call_sync(
            Some(DEST),
            PATH,
            IFACE,
            "Read",
            Some(&(NS, key).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            400,
            gio::Cancellable::NONE,
        );
        if let Ok(reply) = res {
            if reply.n_children() > 0 {
                apply_setting(&mut state.lock().unwrap(), key, reply.child_value(0));
            }
        }
    }
    #[allow(deprecated)]
    let _id = conn.signal_subscribe(
        Some(DEST),
        Some(IFACE),
        Some("SettingChanged"),
        Some(PATH),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, params| {
            if params.n_children() < 3 {
                return;
            }
            let ns = params.child_value(0);
            let key = params.child_value(1);
            if ns.str() != Some(NS) {
                return;
            }
            if let Some(key) = key.str() {
                apply_setting(&mut state.lock().unwrap(), key, params.child_value(2));
                on_change();
            }
        },
    );
    // The session bus connection is a process-wide singleton; keep it alive.
    std::mem::forget(conn);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        let mut colors = String::new();
        for i in 0..16 {
            colors.push_str(&format!("\"color{i}\": \"#0{:x}0{:x}0{:x}\"{}", i, i, i, if i < 15 { "," } else { "" }));
        }
        format!(
            "{{\"wallpaper\": \"x.png\", \"alpha\": \"100\", \"special\": {{\"background\": \"#101418\", \"foreground\": \"#E0E0E0\", \"cursor\": \"#ffcc00\"}}, \"colors\": {{{colors}}}}}"
        )
    }

    #[test]
    fn parses_and_maps_defaults() {
        let t = parse_pywal(&sample(), &BTreeMap::new()).unwrap();
        assert_eq!(t.source, "pywal");
        assert!(t.dark);
        assert_eq!(t.bg, "#101418");
        assert_eq!(t.fg, "#e0e0e0");
        assert_eq!(t.cursor, "#ffcc00");
        assert_eq!(t.muted, "#080808");
        assert_eq!(t.accent, "#040404");
        assert_eq!(t.heading, t.fg);
        assert_eq!(t.colors.len(), 16);
    }

    #[test]
    fn mapping_overrides() {
        let mut m = BTreeMap::new();
        m.insert("accent".to_string(), "color2".to_string());
        m.insert("heading".to_string(), "#abcdef".to_string());
        m.insert("muted".to_string(), "nonsense".to_string());
        let t = parse_pywal(&sample(), &m).unwrap();
        assert_eq!(t.accent, "#020202");
        assert_eq!(t.heading, "#abcdef");
        assert_eq!(t.muted, "#080808"); // bad override falls back to default
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_pywal("{}", &BTreeMap::new()).is_none());
        assert!(parse_pywal("not json", &BTreeMap::new()).is_none());
        assert!(parse_pywal(&sample().replace("#101418", "red"), &BTreeMap::new()).is_none());
    }

    #[test]
    fn builtin_follows_portal() {
        let m = BTreeMap::new();
        let dark = builtin(PortalState::default(), "builtin", &m);
        assert!(dark.dark);
        let light = builtin(PortalState { scheme: Some(2), accent: Some((1.0, 0.0, 0.5)) }, "portal", &m);
        assert!(!light.dark);
        assert_eq!(light.accent, "#ff0080");
        let bad = builtin(PortalState { scheme: Some(1), accent: Some((2.0, 0.0, 0.0)) }, "portal", &m);
        assert_eq!(bad.accent, DARK[4]);
    }
}
