//! The command registry. Buttons, keys and the palette all run commands by id; the behavior
//! lives in `App::run` (app.rs).

pub struct Command {
    pub id: &'static str,
    pub label: &'static str,
    pub keys: &'static [&'static str],
    /// Icon name in icons.rs, for top-bar buttons.
    pub icon: Option<&'static str>,
    /// Section in the shortcut cheat sheet.
    pub group: &'static str,
    /// Left out of the palette (it would only open itself, or needs a document position).
    pub hidden: bool,
}

const fn c(
    id: &'static str,
    label: &'static str,
    keys: &'static [&'static str],
    icon: Option<&'static str>,
    group: &'static str,
) -> Command {
    Command { id, label, keys, icon, group, hidden: false }
}

const fn h(id: &'static str, label: &'static str, keys: &'static [&'static str], group: &'static str) -> Command {
    Command { id, label, keys, icon: None, group, hidden: true }
}

pub const COMMANDS: &[Command] = &[
    // File
    c("open", "Open file…", &["Ctrl+O"], Some("open"), "File"),
    c("open-recent", "Open recent…", &["Ctrl+Shift+R"], None, "File"),
    c("reload", "Reload document", &["Ctrl+R"], None, "File"),
    c("print", "Print…", &["Ctrl+Shift+P"], Some("print"), "File"),
    c("copy-path", "Copy file path", &[], None, "File"),
    c("properties", "Document properties", &["Ctrl+I"], None, "File"),
    c("close", "Close window", &["Ctrl+W", "Ctrl+Q"], None, "File"),
    // Navigate
    c("goto-page", "Go to page…", &["Ctrl+G", "Ctrl+L"], None, "Navigate"),
    c("outline", "Go to section…", &["Ctrl+Shift+O"], Some("outline"), "Navigate"),
    c("next-page", "Next page", &["N", "Ctrl+PageDown", "Shift+Down"], None, "Navigate"),
    c("prev-page", "Previous page", &["P", "Ctrl+PageUp", "Shift+Up"], None, "Navigate"),
    c("first-page", "First page", &["Home", "Ctrl+Home", "G"], None, "Navigate"),
    c("last-page", "Last page", &["End", "Ctrl+End", "Shift+G"], None, "Navigate"),
    c("links", "Follow a link on this page…", &["Ctrl+Shift+L"], None, "Navigate"),
    c("back", "Back", &["Alt+Left", "Backspace"], Some("back"), "Navigate"),
    c("forward", "Forward", &["Alt+Right", "Shift+Backspace"], Some("forward"), "Navigate"),
    h("scroll-down", "Scroll down", &["Down", "J"], "Navigate"),
    h("scroll-up", "Scroll up", &["Up", "K"], "Navigate"),
    h("scroll-left", "Scroll left", &["Left", "H"], "Navigate"),
    h("scroll-right", "Scroll right", &["Right", "L"], "Navigate"),
    h("screen-down", "Scroll a screen down", &["PageDown", "Space"], "Navigate"),
    h("screen-up", "Scroll a screen up", &["PageUp", "Shift+Space"], "Navigate"),
    // View
    c("zoom-in", "Zoom in", &["Ctrl+=", "Ctrl+Shift+=", "=", "Shift+="], Some("zoom-in"), "View"),
    c("zoom-out", "Zoom out", &["Ctrl+-", "-"], Some("zoom-out"), "View"),
    c("zoom-reset", "Actual size (100%)", &["Ctrl+0", "0"], None, "View"),
    c("zoom-pick", "Zoom…", &["Z"], None, "View"),
    c("fit-width", "Fit width", &["W"], Some("fit-width"), "View"),
    c("fit-page", "Fit page", &["F"], Some("fit-page"), "View"),
    c("continuous", "Toggle continuous scrolling", &["C"], Some("continuous"), "View"),
    c("dual", "Toggle dual pages", &["D"], Some("dual"), "View"),
    c("cover", "Toggle cover page in dual mode", &["Shift+D"], None, "View"),
    c("night", "Toggle night mode", &["I"], Some("night"), "View"),
    c("rotate-right", "Rotate clockwise", &["Ctrl+Right", "R"], None, "View"),
    c("rotate-left", "Rotate counterclockwise", &["Ctrl+Left", "Shift+R"], None, "View"),
    c("presentation", "Presentation mode", &["F5"], None, "View"),
    c("topbar", "Toggle top bar", &["Ctrl+B"], None, "View"),
    c("focus", "Toggle focus mode (hide both bars)", &["Ctrl+Shift+F"], Some("focus"), "View"),
    c("theme-mode", "Switch color mode (system, light, dark)", &["Ctrl+Shift+T"], None, "View"),
    c("fullscreen", "Toggle fullscreen", &["F11"], None, "View"),
    // Text
    c("find", "Find", &["Ctrl+F", "/"], Some("search"), "Text"),
    c("find-next", "Find next", &["F3", "Ctrl+Enter"], None, "Text"),
    c("find-prev", "Find previous", &["Shift+F3", "Ctrl+Shift+Enter"], None, "Text"),
    c("copy", "Copy selection", &["Ctrl+C"], Some("copy"), "Text"),
    c("select-all", "Select all text on the page", &["Ctrl+A"], None, "Text"),
    c("highlight", "Highlight selection or match", &["Ctrl+H", "Shift+H"], Some("highlight"), "Text"),
    c("unhighlight", "Remove highlight", &["Delete", "Shift+Delete"], None, "Text"),
    c("highlight-color", "Highlight color…", &[], None, "Text"),
    c("undo", "Undo highlight change", &["Ctrl+Z"], None, "Text"),
    c("redo", "Redo highlight change", &["Ctrl+Shift+Z", "Ctrl+Y"], None, "Text"),
    c("highlights", "Go to highlight…", &["Ctrl+Shift+H"], None, "Text"),
    h("escape", "Clear selection / close", &["Escape"], "Text"),
    // App
    h("palette", "Command palette", &["Ctrl+P", "Ctrl+Shift+A"], "App"),
    c("settings", "Settings", &["Ctrl+,"], Some("settings"), "App"),
    c("shortcuts", "Keyboard shortcuts", &["Ctrl+K", "Ctrl+/", "Shift+/", "F1"], None, "App"),
];

pub fn get(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = COMMANDS.iter().map(|c| c.id).collect();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }

    #[test]
    fn icons_exist() {
        for c in COMMANDS {
            if let Some(i) = c.icon {
                assert!(crate::icons::svg(i).is_some(), "missing icon {i}");
            }
        }
    }
}
