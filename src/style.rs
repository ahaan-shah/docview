//! Palette -> derived colors (same formulas as allwrite's theme.ts) -> GTK stylesheet.

use crate::theme::Theme;

pub type Rgb = [u8; 3];

pub fn parse_hex(hex: &str) -> Rgb {
    let h = hex.trim_start_matches('#');
    let full: String = if h.len() == 3 { h.chars().flat_map(|c| [c, c]).collect() } else { h.to_string() };
    let n = u32::from_str_radix(&full, 16).unwrap_or(0);
    [(n >> 16) as u8, (n >> 8) as u8, n as u8]
}

pub fn to_hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round().clamp(0.0, 255.0) as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

pub fn luminance(c: Rgb) -> f64 {
    let ch = |v: u8| {
        let s = v as f64 / 255.0;
        if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(c[0]) + 0.7152 * ch(c[1]) + 0.0722 * ch(c[2])
}

pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Nudge `color` away from `bg` (toward white or black) until it reaches `min` contrast.
pub fn ensure_contrast(color: Rgb, bg: Rgb, min: f64) -> Rgb {
    if contrast(color, bg) >= min {
        return color;
    }
    let target = if luminance(bg) < 0.5 { [255, 255, 255] } else { [0, 0, 0] };
    let mut t = 0.08;
    while t <= 1.0001 {
        let c = mix(color, target, t);
        if contrast(c, bg) >= min {
            return c;
        }
        t += 0.08;
    }
    target
}

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub bg: Rgb,
    pub fg: Rgb,
    pub muted: Rgb,
    pub accent: Rgb,
    pub strong: Rgb,
    pub hover: Rgb,
    pub border: Rgb,
    pub panel: Rgb,
    pub selection: Rgb,
    pub code_bg: Rgb,
    /// c1..c6 (index 0 unused) after the contrast check.
    pub c: [Rgb; 7],
    /// Raw color0..color15 from the theme, for highlight colors.
    pub raw: Vec<Rgb>,
}

impl Palette {
    pub fn from_theme(t: &Theme) -> Palette {
        let bg = parse_hex(&t.bg);
        let fg = ensure_contrast(parse_hex(&t.fg), bg, 7.0);
        let accent = ensure_contrast(parse_hex(&t.accent), bg, 4.5);
        let dark = luminance(bg) < 0.5;
        let strong = mix(fg, if dark { [255; 3] } else { [0; 3] }, 0.35);
        let mut c = [fg; 7];
        for (i, slot) in c.iter_mut().enumerate().skip(1) {
            *slot = ensure_contrast(t.colors.get(i).map(|s| parse_hex(s)).unwrap_or(fg), bg, 4.5);
        }
        Palette {
            dark,
            bg,
            fg,
            muted: ensure_contrast(parse_hex(&t.muted), bg, 3.2),
            accent,
            strong,
            hover: mix(bg, fg, 0.1),
            border: mix(bg, fg, 0.14),
            panel: mix(bg, fg, 0.035),
            selection: mix(bg, accent, 0.32),
            code_bg: mix(bg, fg, 0.06),
            c,
            raw: t.colors.iter().map(|s| parse_hex(s)).collect(),
        }
    }

    /// A theme slot name ("color3", "accent", "#ffcc00") to a color.
    pub fn resolve(&self, key: &str) -> Option<Rgb> {
        match key {
            "accent" => Some(self.accent),
            "foreground" | "fg" => Some(self.fg),
            "background" | "bg" => Some(self.bg),
            k if k.starts_with('#') && (k.len() == 7 || k.len() == 4) => Some(parse_hex(k)),
            k => {
                let n: usize = k.strip_prefix("color")?.parse().ok()?;
                self.raw.get(n).copied()
            }
        }
    }

    pub fn css(&self, ui_font: &str) -> String {
        let shadow = if self.dark { "rgba(0,0,0,0.45)" } else { "rgba(0,0,0,0.14)" };
        let mut s = String::new();
        for (name, c) in [
            ("bg", self.bg),
            ("fg", self.fg),
            ("muted", self.muted),
            ("accent", self.accent),
            ("strong", self.strong),
            ("hover", self.hover),
            ("border", self.border),
            ("panel", self.panel),
            ("selection", self.selection),
            ("code_bg", self.code_bg),
        ] {
            s.push_str(&format!("@define-color {name} {};\n", to_hex(c)));
        }
        s.push_str(&format!("@define-color shadow {shadow};\n"));
        let font = ui_font.replace(['"', ';', '{', '}'], "");
        s.push_str(&format!("window.docview, window.docview * {{ font-family: \"{font}\", \"Inter\", sans-serif; }}\n"));
        s.push_str(STYLESHEET);
        s
    }
}

/// Mirrors allwrite's styles.css. Sizes in px; GTK CSS has no color-mix(), so mix()/alpha().
const STYLESHEET: &str = r#"
window.docview { background: @bg; color: @fg; font-size: 12.5px; }
window.docview label, window.docview entry, window.docview button { font-size: 12.5px; }

/* ---- top bar ---- */
.topbar { min-height: 44px; padding: 0 8px; transition: opacity 120ms; }
.topbar > * { margin-right: 8px; }
.topbar > *:last-child { margin-right: 0; }
.topbar .filename { color: @muted; font-weight: 500; }
.topbar .group + .group { margin-left: 6px; padding-left: 6px; border-left: 1px solid @border; }
.topbar .group { min-height: 20px; }

button.icon-btn {
  min-width: 32px; min-height: 32px; padding: 0; margin: 0;
  border: none; border-radius: 7px; background: none; box-shadow: none;
  color: @muted; transition: background 120ms, color 120ms;
}
button.icon-btn:hover { background: @hover; color: @fg; }
button.icon-btn:active { margin-top: 1px; margin-bottom: -1px; }
button.icon-btn.active { color: @accent; background: alpha(@accent, 0.14); }
button.icon-btn:focus-visible { outline: 2px solid alpha(@accent, 0.6); outline-offset: -2px; }
button.icon-btn:disabled { opacity: 0.4; }

button.zoom-btn {
  min-height: 28px; min-width: 46px; padding: 0 6px; margin: 0 2px;
  border: none; border-radius: 7px; background: none; box-shadow: none;
  color: @muted; font-variant-numeric: tabular-nums;
}
button.zoom-btn:hover { background: @hover; color: @fg; }

.pagebox { color: @muted; }
.pagebox label { font-feature-settings: "tnum"; margin-left: 6px; margin-right: 4px; }
entry.page-entry {
  min-height: 26px; min-width: 0; padding: 0 6px; margin: 0;
  background: @bg; color: @fg; border: 1px solid @border; border-radius: 6px;
  box-shadow: none; outline: none; font-feature-settings: "tnum";
}
entry.page-entry:focus-within { border-color: @accent; }
entry.page-entry text { margin: 0; }
entry.page-entry > text > selection, entry > text > selection { background: @selection; color: @fg; }

/* ---- status bar ---- */
.statusbar { min-height: 24px; padding: 0 12px; color: @muted; font-size: 11.5px; transition: opacity 120ms; }
.statusbar label { font-size: 11.5px; font-feature-settings: "tnum"; }
.statusbar > * { margin-left: 16px; }
.statusbar > *:first-child { margin-left: 0; }


/* ---- find bar (allwrite's CodeMirror search panel) ---- */
.findbar { padding: 6px 10px; background: @panel; border-top: 1px solid @border; }
.findbar > * { margin-right: 5px; }
.findbar entry {
  min-height: 26px; padding: 0 8px; min-width: 240px;
  background: @bg; color: @fg; border: 1px solid @border; border-radius: 6px; box-shadow: none; outline: none;
}
.findbar entry:focus-within { border-color: @accent; }
.findbar .count { color: @muted; font-size: 11.5px; margin-left: 4px; }
.findbar button.btn { min-height: 24px; }

/* ---- buttons ---- */
button.btn {
  padding: 3px 10px; min-height: 0; border: 1px solid @border; border-radius: 6px;
  background: none; box-shadow: none; color: @fg; transition: background 120ms;
}
button.btn:hover, button.btn:focus-visible { background: @hover; outline: none; }
button.btn.primary { background: @accent; border-color: @accent; color: @bg; font-weight: 600; }
button.btn.primary:hover, button.btn.primary:focus-visible { background: mix(@accent, white, 0.1); }
button.btn.toggle:checked { color: @accent; border-color: alpha(@accent, 0.6); background: alpha(@accent, 0.14); }

/* ---- tooltip ---- */
tooltip, tooltip.background {
  padding: 4px 9px; border-radius: 7px; background: @panel; color: @fg;
  border: 1px solid @border; box-shadow: 0 6px 20px @shadow;
}
tooltip label { font-size: 12px; }

/* ---- toast ---- */
.toast {
  margin-bottom: 40px; padding: 8px 16px; border-radius: 10px;
  background: @panel; border: 1px solid @border; box-shadow: 0 8px 28px @shadow;
}

/* ---- overlay: palette, settings, dialogs, shortcuts ---- */
.scrim { background: alpha(@bg, 0.6); }
.palette, .settings, .dialog, .shortcuts {
  margin-top: 12px; background: @panel; border: 1px solid @border; border-radius: 12px;
  box-shadow: 0 16px 48px @shadow;
}
.palette entry {
  min-height: 44px; padding: 0 16px; font-size: 14px;
  background: none; border: none; border-bottom: 1px solid @border; border-radius: 12px 12px 0 0;
  box-shadow: none; outline: none; color: @fg;
}
.palette entry text placeholder { color: @muted; }
.palette list { background: none; padding: 5px; }
.palette row {
  padding: 6px 10px; border-radius: 6px; min-height: 0; background: none; color: @fg; font-size: 13px;
}
.palette row label { font-size: 13px; }
.palette row:selected, .palette row.active { background: alpha(@accent, 0.16); color: @fg; }
.palette row:hover { background: alpha(@accent, 0.08); }
.palette row:selected:hover { background: alpha(@accent, 0.16); }
.palette .hint { color: @muted; font-size: 11.5px; }
.palette-empty { color: @muted; padding: 10px; font-size: 12.5px; }
label.kbd {
  color: @muted; font-family: "JetBrains Mono", monospace; font-size: 11px;
  padding: 1px 6px; border: 1px solid @border; border-radius: 5px; background: @bg;
}

.dialog { padding: 18px; }
.dialog label.msg { font-size: 13.5px; margin-bottom: 16px; }
.dialog entry {
  min-height: 30px; padding: 0 8px; margin-bottom: 16px;
  background: @bg; color: @fg; border: 1px solid @border; border-radius: 6px; box-shadow: none; outline: none;
}
.dialog entry:focus-within { border-color: @accent; }
.dialog .buttons > * { margin-left: 8px; }

.settings, .shortcuts { padding: 16px 18px; }
.settings label, .shortcuts label { font-size: 13px; }
label.heading {
  margin-bottom: 12px; font-size: 13px; font-weight: 600; color: @muted; letter-spacing: 0.5px;
}
.setting { min-height: 34px; }
.settings-note, label.settings-note { margin-top: 12px; color: @muted; font-size: 11.5px; }

switch {
  min-width: 32px; min-height: 18px; border-radius: 9px; padding: 0; margin: 0;
  background: @border; border: none; box-shadow: none; outline: none; transition: background 140ms;
}
switch:checked { background: @accent; }
switch slider {
  min-width: 14px; min-height: 14px; margin: 2px; border-radius: 50%;
  background: @fg; border: none; box-shadow: none; outline: none;
}
switch:checked slider { background: @bg; }
switch image { color: transparent; }
switch:focus-visible { outline: 2px solid alpha(@accent, 0.6); outline-offset: 2px; }

dropdown > button, .settings dropdown button {
  padding: 3px 10px; min-height: 0; border: 1px solid @border; border-radius: 6px;
  background: none; box-shadow: none; color: @fg;
}
dropdown > button:hover { background: @hover; }
popover.menu > contents, popover > contents {
  background: @panel; color: @fg; border: 1px solid @border; border-radius: 10px; box-shadow: 0 8px 28px @shadow; padding: 4px;
}
popover modelbutton, popover row { border-radius: 6px; padding: 4px 8px; color: @fg; }
popover row:selected, popover row:hover { background: alpha(@accent, 0.16); }

.shortcuts .section { margin-bottom: 16px; }
.shortcuts label.section-title { margin-bottom: 4px; font-size: 11.5px; font-weight: 600; color: @accent; letter-spacing: 0.3px; }
.shortcut { min-height: 26px; border-bottom: 1px solid alpha(@border, 0.5); }
.shortcut:last-child { border-bottom: none; }
.keys label.kbd { color: @fg; border-bottom-width: 2px; min-width: 8px; }
.keys > * { margin-left: 3px; }
.keys .or { color: @muted; padding: 0 3px; font-size: 12px; }

/* ---- empty state ---- */
.empty .title { color: @muted; font-size: 15px; font-weight: 500; }
.empty .hint { color: @muted; font-size: 12.5px; }
.empty button.recent {
  padding: 4px 10px; min-height: 0; border: none; border-radius: 6px; background: none; box-shadow: none; color: @fg;
}
.empty button.recent:hover { background: @hover; }
.empty button.recent label.path { color: @muted; font-size: 11.5px; }

/* ---- document area ---- */
.docview-area { background: @bg; }
scrolledwindow { background: @bg; }
scrollbar { background: none; border: none; }
scrollbar slider { background: @hover; border-radius: 5px; min-width: 6px; min-height: 6px; border: 2px solid transparent; }
scrollbar slider:hover { background: @border; }
scrollbar.overlay-indicator:not(.hovering) slider { min-width: 4px; min-height: 4px; }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_is_enforced() {
        let bg = parse_hex("#1a1b26");
        let fg = ensure_contrast(parse_hex("#1c1d28"), bg, 7.0);
        assert!(contrast(fg, bg) >= 7.0);
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(to_hex(parse_hex("#a9b1d6")), "#a9b1d6");
        assert_eq!(parse_hex("#fff"), [255, 255, 255]);
    }
}
