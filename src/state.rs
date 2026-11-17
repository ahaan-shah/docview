//! Remembered state: recent files and the last position in each document.
//! ~/.local/state/docview/state.json, written on close and when switching documents.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 30;
const MAX_POSITIONS: usize = 300;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Position {
    pub page: usize,
    /// Fraction of the page scrolled past the top of the view (0..1).
    pub offset: f64,
    /// None when a fit mode was active.
    pub zoom: Option<f64>,
    pub fit: String,
    pub dual: bool,
    pub continuous: bool,
    /// Seconds since the epoch, used to drop the oldest entries.
    pub at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct State {
    pub recent: Vec<PathBuf>,
    pub positions: BTreeMap<String, Position>,
}

pub fn path() -> Option<PathBuf> {
    Some(dirs::state_dir().or_else(dirs::data_local_dir)?.join("docview").join("state.json"))
}

pub fn load() -> State {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

impl State {
    pub fn save(&self) {
        if let Some(p) = path() {
            if let Ok(text) = serde_json::to_string(self) {
                let _ = atomic_write(&p, text.as_bytes());
            }
        }
    }

    pub fn touch_recent(&mut self, file: &Path) {
        self.recent.retain(|p| p != file);
        self.recent.insert(0, file.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }

    /// Recent files that still exist.
    pub fn recent_existing(&self) -> Vec<PathBuf> {
        self.recent.iter().filter(|p| p.exists()).cloned().collect()
    }

    pub fn set_position(&mut self, file: &Path, mut pos: Position) {
        pos.at = now();
        self.positions.insert(file.to_string_lossy().into_owned(), pos);
        if self.positions.len() > MAX_POSITIONS {
            let mut by_age: Vec<(String, u64)> = self.positions.iter().map(|(k, v)| (k.clone(), v.at)).collect();
            by_age.sort_by_key(|e| e.1);
            for (k, _) in by_age.into_iter().take(self.positions.len() - MAX_POSITIONS) {
                self.positions.remove(&k);
            }
        }
    }

    pub fn position(&self, file: &Path) -> Option<&Position> {
        self.positions.get(file.to_string_lossy().as_ref())
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Write to a temporary file, then rename over the target.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("bad path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{}.tmp{}", path.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
    std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(page: usize) -> Position {
        Position { page, offset: 0.0, zoom: None, fit: "width".into(), dual: false, continuous: true, at: 0 }
    }

    #[test]
    fn recent_is_deduplicated_and_capped() {
        let mut s = State::default();
        for i in 0..40 {
            s.touch_recent(Path::new(&format!("/tmp/{i}.pdf")));
        }
        s.touch_recent(Path::new("/tmp/5.pdf"));
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert_eq!(s.recent[0], PathBuf::from("/tmp/5.pdf"));
        assert_eq!(s.recent.iter().filter(|p| p.ends_with("5.pdf")).count(), 1);
    }

    #[test]
    fn positions_roundtrip() {
        let mut s = State::default();
        s.set_position(Path::new("/a.pdf"), pos(7));
        let back: State = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.position(Path::new("/a.pdf")).unwrap().page, 7);
    }
}
