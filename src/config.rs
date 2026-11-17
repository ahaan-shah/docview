//! ~/.config/docview/config.toml

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ViewCfg {
    /// "width" | "page" | "none"
    pub fit: String,
    /// Zoom used when fit = "none" (1.0 = 100%).
    pub zoom: f64,
    pub continuous: bool,
    pub dual: bool,
    /// In dual mode, show the first page on its own (book layout).
    pub cover: bool,
    /// Recolor pages with the theme's background and text colors.
    pub night: bool,
}

impl Default for ViewCfg {
    fn default() -> Self {
        Self { fit: "width".into(), zoom: 1.0, continuous: true, dual: false, cover: true, night: false }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AppCfg {
    /// Reopen documents where you left off.
    pub restore_position: bool,
    /// Highlight color: a theme slot (color1..color15, accent) or #rrggbb.
    pub highlight: String,
    /// UI font family.
    pub font: String,
    /// Upper bound for rendered page tiles kept in memory, in MB.
    pub cache_mb: u32,
}

impl Default for AppCfg {
    fn default() -> Self {
        Self { restore_position: true, highlight: "color3".into(), font: "Inter".into(), cache_mb: 48 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Config {
    pub view: ViewCfg,
    pub app: AppCfg,
    /// command id -> key combo, e.g. highlight = "Ctrl+H"
    pub keys: BTreeMap<String, String>,
    /// theme slot -> pywal key or hex, e.g. accent = "color4"
    pub theme: BTreeMap<String, String>,
}

pub fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("docview").join("config.toml"))
}

pub fn parse(text: &str) -> Config {
    toml::from_str(text).unwrap_or_default()
}

pub fn load() -> Config {
    path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse(&t)).unwrap_or_default()
}

pub fn save(cfg: &Config) -> Result<(), String> {
    let p = path().ok_or("no config directory")?;
    let text = toml::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    crate::state::atomic_write(&p, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_config_keeps_defaults() {
        let c = parse("[view]\ndual = true\n[keys]\nhighlight = \"Ctrl+H\"\n");
        assert!(c.view.dual);
        assert!(c.view.continuous);
        assert_eq!(c.view.fit, "width");
        assert_eq!(c.keys.get("highlight").unwrap(), "Ctrl+H");
    }

    #[test]
    fn garbage_is_default() {
        assert_eq!(parse("not = [toml"), Config::default());
    }

    #[test]
    fn roundtrip() {
        let mut c = Config::default();
        c.view.night = true;
        c.theme.insert("accent".into(), "color2".into());
        assert_eq!(parse(&toml::to_string_pretty(&c).unwrap()), c);
    }
}
