//! Key combos as strings ("Ctrl+Shift+S"): parsing, matching GDK events, user overrides.
//! Same canonical form as allwrite: modifiers in Ctrl, Alt, Shift order, key upper-cased.

use crate::commands::{Command, COMMANDS};
use gtk::gdk;
use std::collections::{BTreeMap, HashMap};

fn alias(part: &str) -> Option<&'static str> {
    Some(match part.to_lowercase().as_str() {
        "control" | "ctrl" | "mod" | "cmd" => "Ctrl",
        "shift" => "Shift",
        "alt" => "Alt",
        "plus" | "+" => "=",
        "esc" => "Escape",
        "return" => "Enter",
        "space" => "Space",
        "pgup" | "pageup" => "PageUp",
        "pgdn" | "pagedown" => "PageDown",
        "del" | "delete" => "Delete",
        _ => return None,
    })
}

pub fn normalize(combo: &str) -> String {
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let mut key = String::new();
    let c = combo.trim();
    let c = if let Some(stripped) = c.strip_suffix("++") { format!("{stripped}+plus") } else { c.to_string() };
    for part in c.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let p = alias(part).map(String::from).unwrap_or_else(|| part.to_string());
        match p.as_str() {
            "Ctrl" => ctrl = true,
            "Alt" => alt = true,
            "Shift" => shift = true,
            _ => {
                let mut ch = p.chars();
                key = if p.chars().count() == 1 {
                    p.to_uppercase()
                } else {
                    ch.next().map(|f| f.to_uppercase().collect::<String>() + ch.as_str()).unwrap_or_default()
                }
            }
        }
    }
    if key.is_empty() {
        return String::new();
    }
    format!("{}{}{}{}", if ctrl { "Ctrl+" } else { "" }, if alt { "Alt+" } else { "" }, if shift { "Shift+" } else { "" }, key)
}

/// Name for an unshifted GDK keyval, matching the names `normalize` produces.
fn key_name(kv: gdk::Key) -> Option<String> {
    use gdk::Key as K;
    let named = match kv {
        K::Page_Up | K::KP_Page_Up => "PageUp",
        K::Page_Down | K::KP_Page_Down => "PageDown",
        K::Home | K::KP_Home => "Home",
        K::End | K::KP_End => "End",
        K::Up | K::KP_Up => "Up",
        K::Down | K::KP_Down => "Down",
        K::Left | K::KP_Left => "Left",
        K::Right | K::KP_Right => "Right",
        K::space | K::KP_Space => "Space",
        K::Return | K::KP_Enter | K::ISO_Enter => "Enter",
        K::Escape => "Escape",
        K::Tab | K::ISO_Left_Tab => "Tab",
        K::BackSpace => "Backspace",
        K::Delete | K::KP_Delete => "Delete",
        K::Insert => "Insert",
        K::equal | K::plus | K::KP_Add => "=",
        K::minus | K::underscore | K::KP_Subtract => "-",
        K::comma => ",",
        K::period => ".",
        K::slash | K::KP_Divide => "/",
        K::backslash => "\\",
        K::semicolon => ";",
        K::apostrophe => "'",
        K::grave => "`",
        K::bracketleft => "[",
        K::bracketright => "]",
        K::Shift_L | K::Shift_R | K::Control_L | K::Control_R | K::Alt_L | K::Alt_R | K::Super_L | K::Super_R
        | K::Meta_L | K::Meta_R | K::ISO_Level3_Shift | K::Caps_Lock => return None,
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_string());
    }
    let n = kv.name()?.to_string();
    if let Some(f) = n.strip_prefix('F') {
        if f.parse::<u8>().is_ok() {
            return Some(n);
        }
    }
    if let Some(d) = n.strip_prefix("KP_") {
        if d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()) {
            return Some(d.to_string());
        }
    }
    let ch = kv.to_unicode()?;
    if ch.is_alphanumeric() {
        return Some(ch.to_uppercase().collect());
    }
    Some(ch.to_string())
}

/// Combo for a key press, or None for bare modifiers. `unshifted` is the level-0 keyval for the
/// same physical key, so Ctrl+Shift+= works whatever character Shift produces.
pub fn combo_from_event(kv: gdk::Key, unshifted: Option<gdk::Key>, state: gdk::ModifierType) -> Option<String> {
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK) || state.contains(gdk::ModifierType::META_MASK);
    let alt = state.contains(gdk::ModifierType::ALT_MASK);
    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
    // Letters use the layout's own keyval; everything else uses the unshifted key.
    let letter = kv.to_unicode().filter(|c| c.is_alphabetic());
    let key = if let Some(c) = letter {
        c.to_uppercase().collect()
    } else {
        key_name(unshifted.unwrap_or(kv)).or_else(|| key_name(kv))?
    };
    if key.is_empty() {
        return None;
    }
    Some(format!("{}{}{}{}", if ctrl { "Ctrl+" } else { "" }, if alt { "Alt+" } else { "" }, if shift { "Shift+" } else { "" }, key))
}

pub struct Keymap {
    map: HashMap<String, &'static str>,
    overrides: BTreeMap<String, String>,
}

impl Keymap {
    pub fn new(overrides: &BTreeMap<String, String>) -> Keymap {
        let mut map = HashMap::new();
        for c in COMMANDS {
            if overrides.contains_key(c.id) {
                continue;
            }
            for k in c.keys {
                let n = normalize(k);
                if !n.is_empty() {
                    map.insert(n, c.id);
                }
            }
        }
        // Overrides win when two commands end up on the same combo.
        for (id, keys) in overrides {
            if let Some(c) = COMMANDS.iter().find(|c| c.id == id) {
                for k in keys.split(',') {
                    let n = normalize(k);
                    if !n.is_empty() {
                        map.insert(n, c.id);
                    }
                }
            }
        }
        Keymap { map, overrides: overrides.clone() }
    }

    pub fn lookup(&self, combo: &str) -> Option<&'static str> {
        self.map.get(combo).copied()
    }

    /// The combos bound to a command, for tooltips, the palette and the cheat sheet.
    pub fn keys_for(&self, cmd: &Command) -> Vec<String> {
        match self.overrides.get(cmd.id) {
            Some(k) => k.split(',').map(normalize).filter(|s| !s.is_empty()).collect(),
            None => cmd.keys.iter().map(|k| normalize(k)).filter(|s| !s.is_empty()).collect(),
        }
    }

    pub fn first_key(&self, cmd: &Command) -> Option<String> {
        self.keys_for(cmd).into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize("shift+ctrl+s"), "Ctrl+Shift+S");
        assert_eq!(normalize("Ctrl++"), "Ctrl+=");
        assert_eq!(normalize("ctrl+pgdn"), "Ctrl+PageDown");
        assert_eq!(normalize("alt+left"), "Alt+Left");
        assert_eq!(normalize("Ctrl+"), "");
        assert_eq!(normalize("f11"), "F11");
    }

    #[test]
    fn events_map_to_combos() {
        use gdk::ModifierType as M;
        assert_eq!(combo_from_event(gdk::Key::o, None, M::CONTROL_MASK).unwrap(), "Ctrl+O");
        assert_eq!(combo_from_event(gdk::Key::G, None, M::SHIFT_MASK).unwrap(), "Shift+G");
        assert_eq!(combo_from_event(gdk::Key::plus, Some(gdk::Key::equal), M::CONTROL_MASK | M::SHIFT_MASK).unwrap(), "Ctrl+Shift+=");
        assert_eq!(combo_from_event(gdk::Key::question, Some(gdk::Key::slash), M::CONTROL_MASK | M::SHIFT_MASK).unwrap(), "Ctrl+Shift+/");
        assert_eq!(combo_from_event(gdk::Key::Page_Down, None, M::empty()).unwrap(), "PageDown");
        assert_eq!(combo_from_event(gdk::Key::F11, None, M::empty()).unwrap(), "F11");
        assert!(combo_from_event(gdk::Key::Control_L, None, M::CONTROL_MASK).is_none());
    }

    #[test]
    fn every_default_key_is_unique() {
        let mut seen = HashMap::new();
        for c in COMMANDS {
            for k in c.keys {
                let n = normalize(k);
                assert!(!n.is_empty(), "{} has a bad key {k}", c.id);
                if let Some(prev) = seen.insert(n.clone(), c.id) {
                    panic!("{n} is bound to both {prev} and {}", c.id);
                }
            }
        }
    }

    #[test]
    fn overrides_replace_defaults() {
        let mut o = BTreeMap::new();
        o.insert("highlight".to_string(), "Ctrl+H".to_string());
        let km = Keymap::new(&o);
        assert_eq!(km.lookup("Ctrl+H"), Some("highlight"));
        assert_eq!(km.lookup("Shift+H"), None);
    }
}
