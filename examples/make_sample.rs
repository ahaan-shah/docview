//! Writes a sample PDF (headings, prose, an outline, internal and web links) for tests and
//! README screenshots: `cargo run --example make_sample -- out.pdf [pages]`.

use gtk::pango;

const W: f64 = 595.0;
const H: f64 = 842.0;

const PROSE: &str = "Reading should feel like paper: the page in the middle, nothing moving unless you move it, and every action a keystroke away. This viewer draws pages in tiles on a background thread, keeps a small cache, and lets the rest of the window stay out of the way. Select text by dragging, double-click a word or triple-click a line, and press Ctrl+H to highlight it; highlights are saved into the PDF itself so other viewers see them too.";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).cloned().unwrap_or_else(|| "sample.pdf".into());
    let pages: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(12);
    let surface = cairo::PdfSurface::new(W, H, &out).expect("pdf surface");
    surface.set_metadata(cairo::PdfMetadata::Title, "Field Guide to Quiet Software").ok();
    surface.set_metadata(cairo::PdfMetadata::Author, "docview").ok();
    let cr = cairo::Context::new(&surface).unwrap();
    let chapters = ["Introduction", "Pages and tiles", "Selection", "Highlights", "Search", "Keys", "Memory", "Themes"];
    for p in 0..pages {
        let chapter = chapters[p % chapters.len()];
        let title = format!("{}. {}", p + 1, chapter);
        cr.tag_begin("cairo.dest", &format!("name='ch{}'", p + 1));
        cr.tag_end("cairo.dest");
        surface.add_outline(cairo::PDF_OUTLINE_ROOT, &title, &format!("dest='ch{}'", p + 1), cairo::PdfOutline::empty()).ok();
        let layout = pangocairo::functions::create_layout(&cr);
        let mut y = 72.0;
        let text = |cr: &cairo::Context, y: &mut f64, s: &str, size: f64, bold: bool, gap: f64| {
            let mut fd = pango::FontDescription::from_string("Serif");
            fd.set_absolute_size(size * pango::SCALE as f64);
            if bold {
                fd.set_weight(pango::Weight::Bold);
            }
            layout.set_font_description(Some(&fd));
            layout.set_width(((W - 144.0) * pango::SCALE as f64) as i32);
            layout.set_wrap(pango::WrapMode::WordChar);
            layout.set_text(s);
            cr.move_to(72.0, *y);
            cr.set_source_rgb(0.1, 0.1, 0.12);
            pangocairo::functions::show_layout(cr, &layout);
            *y += layout.pixel_size().1 as f64 + gap;
        };
        text(&cr, &mut y, &title, 24.0, true, 18.0);
        for i in 0..4 {
            text(&cr, &mut y, PROSE, 11.5, false, 10.0);
            if i == 1 {
                // a figure: colored boxes, to check night mode keeps colors apart
                let colors = [(0.86, 0.3, 0.3), (0.3, 0.65, 0.4), (0.25, 0.45, 0.85), (0.9, 0.7, 0.2)];
                for (k, c) in colors.iter().enumerate() {
                    cr.set_source_rgb(c.0, c.1, c.2);
                    cr.rectangle(72.0 + k as f64 * 115.0, y, 100.0, 60.0);
                    let _ = cr.fill();
                }
                y += 76.0;
            }
        }
        // Links: next chapter (internal) and a web link.
        let next = (p + 1) % pages + 1;
        cr.tag_begin("Link", &format!("dest='ch{next}'"));
        text(&cr, &mut y, &format!("Continue to chapter {next} →"), 11.5, true, 6.0);
        cr.tag_end("Link");
        cr.tag_begin("Link", "uri='https://www.gtk.org'");
        text(&cr, &mut y, "www.gtk.org", 11.5, false, 6.0);
        cr.tag_end("Link");
        cr.move_to(W / 2.0 - 6.0, H - 40.0);
        cr.set_source_rgb(0.4, 0.4, 0.45);
        cr.set_font_size(9.0);
        let _ = cr.show_text(&format!("{}", p + 1));
        let _ = cr.show_page();
    }
    surface.finish();
}
