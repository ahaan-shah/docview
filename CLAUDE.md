# docview

Minimal PDF viewer for Linux (Hyprland + pywal), sibling of `../allwrite` (same look, command
registry, palette, shortcut sheet, theme code). Rust + GTK 4 (gtk4-rs) + poppler-glib.
Replaces GNOME Papers on this machine.

## Priorities

1. **Low RAM**: target under 100-150 MB PSS while in use, lower is better; 0% idle CPU.
   No polling or idle timers: everything is event driven (inotify via GIO monitors, D-Bus portal
   signals, GTK events). One-shot debounce timers after activity are fine.
2. Looks and feels like allwrite. Compare with `../allwrite/src/styles.css` before changing UI.
3. Every action is a command in `src/commands.rs`, dispatched in `App::run` (`src/app.rs`).
   Buttons, keys and the palette all run commands; do not wire behavior anywhere else.

## Commands

```
cargo build --release                       # target/release/docview
cargo test --release
cargo run --release --example make_sample -- tests/fixtures/sample.pdf 12
scripts/shot.sh out.png [file] ['cmd;cmd'] [WxH] [delay]   # screenshot, no window on your screen
scripts/measure.sh [pid]                    # PSS / RSS / CPU of a running docview
scripts/install.sh [--default]              # per-user install into ~/.local
```

## Do not disturb the desktop

The user works on this machine while sessions run. Never open the app on their workspace.
- `scripts/shot.sh` runs the app with `GDK_BACKEND=x11` inside a rootful `Xwayland :93` parked
  on Hyprland workspace 9 (`hl.exec_cmd(cmd, { workspace = "9 silent" })`, Lua API).
  A Wayland window on a hidden workspace gets **no frame callbacks**, so GTK never revalidates
  CSS or lays out after the first frame: screenshots taken that way show stale styles. Broadway
  (`gtk4-broadwayd`) has the same problem. Xwayland's X11 frame clock ticks on a timer.
- The Xwayland root is the size of the workspace-9 tile (~1426x761), so windows are clamped to it.
- shot.sh points `XDG_CONFIG_HOME` / `XDG_STATE_HOME` at a temp dir; never let tests write the
  user's real `~/.config/docview` or `~/.local/state/docview`.
- `pkill -f <pattern>` can match the shell running it (the pattern is in its own command line)
  and kill the tool call. Use `pgrep -f "^Xwayland :93"` style anchored patterns.
- Use only `tests/fixtures/sample.pdf` (generated) for committed screenshots, never the user's PDFs.
- Kill leftovers when done (`pkill -x docview`) and check `hyprctl activeworkspace`.

Dev hooks (cost nothing when unset): `DOCVIEW_RUN="cmd;sleep:300;type:text;enter:;drag:x0,y0,x1,y1;click:x,y;dclick:x,y"`
runs command ids / fake input after startup; `DOCVIEW_SNAPSHOT=out.png` (+ `_DELAY` ms, `_QUIT`)
lays out, draws, and saves a PNG; `DOCVIEW_SIZE=WxH`; `DOCVIEW_DEBUG=1` prints timing and state.

## Layout

```
src/
  main.rs      entry, lean env (cairo renderer, no GL/Vulkan, mallopt arenas), dev hooks
  app.rs       window, top bar, status bar, find bar, empty state, App::run, pickers, settings
  view.rs      DocView: GtkScrollable widget. Layout, tile drawing, selection, links, search,
               highlights (+ undo), history, rotation. Emits `Change` to the app.
  render.rs    worker thread with its own poppler::Document; renders 512px tiles on request
  layout.rs    pure page placement (continuous / single, dual / cover), fit zoom, rotation math
  pdf.rs       poppler wrapper + FFI (links, outline, highlight annots, selection, atomic save)
  commands.rs  registry       keymap.rs  combos, GDK events, overrides
  ui.rs        modal layer, picker (palette), dialogs, shortcut sheet, toast
  style.rs     palette -> derived colors (allwrite's theme.ts formulas) -> GTK CSS
  theme.rs     pywal -> portal -> built-in (copied from allwrite)
  icons.rs     Lucide SVGs (data/icons) drawn with cairo (tiny SVG path parser)
  config.rs    ~/.config/docview/config.toml    state.rs  recent files + positions
examples/make_sample.rs   generates the test / screenshot PDF
```

## Things that are easy to get wrong

- Coordinates: `pdf::Rect` and `DocPoint` are **unrotated page points, top-left origin**. poppler's
  link / annot / find rects are bottom-left (flipped in pdf.rs); selection APIs are top-left.
  Only layout and drawing see rotated coordinates (`layout::rot_*`, `DocView::disp_rect`).
- poppler objects are not thread safe: the main thread, the render worker and each search
  thread open their own `poppler::Document`. After a highlight edit the file is saved atomically
  (temp + rename; the open docs keep the old inode) and the worker is re-pointed via `set_doc`.
  The file watcher ignores our own save by comparing (mtime, len) with `own_save`.
- Render results carry a generation; results from an older generation (doc/recolor/rotation
  change) are dropped. Pages touched by an annotation edit keep their old tiles on screen
  (marked stale through `page_gen`) until fresh ones arrive.
- Tiles are `B8g8r8a8Premultiplied` (cairo ARGB32, painted opaque). Any other format makes the
  cairo GSK renderer convert on every frame (that cost ~20 extra threads and lots of RAM).
- `MALLOC_ARENA_MAX` in the env is read before `main`; use `mallopt` (done in `lean_env`).
- Edition 2024: `gen` is a reserved keyword.
- Key combos: letters come from the layout's keyval, everything else from the unshifted keyval
  of the same keycode, so `Ctrl+Shift+=` and `Shift+/` work. In text entries plain keys and the
  usual editing combos stay with the entry (`App::on_key`).

## Measured (2026-11-18, 1338-page book, scale 2 display)

| | PSS |
|---|---|
| Open, first screen rendered (Wayland) | ~62 MB |
| Scrolling, zooming, dual, night, full-text search | peak ~117 MB, settles ~100 MB |

First tile ~215 ms after launch. Idle CPU 0. Binary 1.4 MB.

## Git and releases

Public repository `ahaan-shah/docview`, branch `main`. Commit after each working change.
Releasing: bump the version in `Cargo.toml` and `packaging/PKGBUILD`, commit, push a tag `vX.Y.Z`.
`.github/workflows/release.yml` builds on Ubuntu 24.04 (x86-64 + ARM64; GTK 4.14 is the floor,
so gtk4-rs features stay at `v4_12`) and publishes tarballs, `.deb`, `.rpm` and `checksums.txt`.
The root `install.sh` downloads that tarball.
