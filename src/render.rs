//! Background rendering. One worker thread owns its own poppler document and renders tiles
//! (TILE x TILE device pixels) on request. The main thread replaces the request list whenever
//! the view moves, so stale work is dropped instead of queued.

use crate::pdf;
use gtk::{cairo, glib};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

pub const TILE: u32 = 512;
/// Width in device pixels of the low-resolution page preview drawn while tiles load.
pub const PREVIEW_W: f64 = 220.0;

#[derive(Clone, Copy, Hash, Eq, PartialEq, Debug)]
pub struct TileKey {
    pub page: u32,
    /// Device pixels per point x 1000. 0 means the page preview.
    pub scale: u32,
    pub tx: u32,
    pub ty: u32,
}

impl TileKey {
    pub fn scale_f(&self) -> f64 {
        self.scale as f64 / 1000.0
    }
}

pub fn quantize(scale: f64) -> u32 {
    (scale * 1000.0).round().max(1.0) as u32
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub page: u32,
    pub scale: u32,
    pub tiles: Vec<(u32, u32)>,
}

pub struct Rendered {
    pub key: TileKey,
    pub generation: u64,
    pub w: i32,
    pub h: i32,
    pub data: Vec<u8>,
}

/// Night mode: map white to `bg` and black to `fg`, keeping colors recognisable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Recolor {
    pub bg: [u8; 3],
    pub fg: [u8; 3],
}

struct Queue {
    path: Option<PathBuf>,
    password: Option<String>,
    /// Bumped on every document or style change; results from older generations are dropped.
    generation: u64,
    doc_gen: u64,
    recolor: Option<Recolor>,
    /// Clockwise page rotation in degrees.
    rotation: u32,
    jobs: VecDeque<Job>,
    in_flight: Option<Job>,
    quit: bool,
}

pub struct Renderer {
    shared: Arc<(Mutex<Queue>, Condvar)>,
}

impl Renderer {
    pub fn new(on_result: impl Fn(Rendered) + 'static) -> Renderer {
        let shared = Arc::new((
            Mutex::new(Queue {
                path: None,
                password: None,
                generation: 1,
                doc_gen: 0,
                recolor: None,
                rotation: 0,
                jobs: VecDeque::new(),
                in_flight: None,
                quit: false,
            }),
            Condvar::new(),
        ));
        let (tx, rx) = async_channel::unbounded::<Rendered>();
        let worker_shared = shared.clone();
        std::thread::Builder::new()
            .name("render".into())
            .spawn(move || worker(worker_shared, tx))
            .expect("spawn render thread");
        glib::spawn_future_local(async move {
            while let Ok(r) = rx.recv().await {
                on_result(r);
            }
        });
        Renderer { shared }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Queue) -> R) -> R {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap();
        let r = f(&mut q);
        cv.notify_one();
        r
    }

    /// Switch document (or reload the same one). Returns the new generation.
    pub fn set_doc(&self, path: Option<PathBuf>, password: Option<String>) -> u64 {
        self.with(|q| {
            q.path = path;
            q.password = password;
            q.generation += 1;
            q.doc_gen += 1;
            q.jobs.clear();
            q.generation
        })
    }

    pub fn set_recolor(&self, recolor: Option<Recolor>) -> u64 {
        self.with(|q| {
            q.recolor = recolor;
            q.generation += 1;
            q.jobs.clear();
            q.generation
        })
    }

    pub fn set_rotation(&self, rotation: u32) -> u64 {
        self.with(|q| {
            q.rotation = rotation;
            q.generation += 1;
            q.jobs.clear();
            q.generation
        })
    }

    /// Replace the pending work. Tiles the worker is rendering right now are left out.
    pub fn request(&self, mut jobs: Vec<Job>) {
        self.with(|q| {
            if let Some(cur) = &q.in_flight {
                for j in jobs.iter_mut() {
                    if j.page == cur.page && j.scale == cur.scale {
                        j.tiles.retain(|t| !cur.tiles.contains(t));
                    }
                }
                jobs.retain(|j| !j.tiles.is_empty());
            }
            q.jobs = jobs.into();
        });
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.with(|q| q.quit = true);
    }
}

fn worker(shared: Arc<(Mutex<Queue>, Condvar)>, tx: async_channel::Sender<Rendered>) {
    let (lock, cv) = &*shared;
    let mut doc: Option<poppler::Document> = None;
    let mut loaded_gen = 0u64;
    loop {
        let (job, generation, recolor, rotation, reload) = {
            let mut q = lock.lock().unwrap();
            q.in_flight = None;
            while !q.quit && q.jobs.is_empty() {
                q = cv.wait(q).unwrap();
            }
            if q.quit {
                return;
            }
            let job = q.jobs.pop_front().unwrap();
            q.in_flight = Some(job.clone());
            let reload = (q.doc_gen != loaded_gen).then(|| (q.path.clone(), q.password.clone(), q.doc_gen));
            (job, q.generation, q.recolor, q.rotation, reload)
        };
        if let Some((path, password, g)) = reload {
            doc = path.and_then(|p| pdf::open_raw(&p, password.as_deref()).ok());
            loaded_gen = g;
        }
        let Some(d) = &doc else { continue };
        for out in render_job(d, &job, recolor, rotation) {
            if tx.send_blocking(Rendered { generation, ..out }).is_err() {
                return;
            }
        }
    }
}

/// Render the requested tiles of one page. Neighbouring tiles are drawn in one pass (poppler
/// walks the whole page content stream per call), capped so the scratch surface stays small.
fn render_job(doc: &poppler::Document, job: &Job, recolor: Option<Recolor>, rot: u32) -> Vec<Rendered> {
    let Some(page) = doc.page(job.page as i32) else { return Vec::new() };
    let (uw, uh) = page.size();
    let (pw, ph) = crate::layout::rot_size(rot, uw, uh);
    let mut out = Vec::new();
    if job.scale == 0 {
        let s = PREVIEW_W / pw.max(1.0);
        let (w, h) = ((pw * s).ceil() as i32, (ph * s).ceil() as i32);
        if let Some(data) = draw(&page, s, 0, 0, w, h, recolor, rot) {
            out.push(Rendered { key: TileKey { page: job.page, scale: 0, tx: 0, ty: 0 }, generation: 0, w, h, data });
        }
        return out;
    }
    let s = job.scale as f64 / 1000.0;
    let full_w = (pw * s).ceil() as u32;
    let full_h = (ph * s).ceil() as u32;
    // Batches of tiles whose bounding box stays under ~12 MB of scratch memory.
    let mut tiles = job.tiles.clone();
    tiles.sort_by_key(|t| (t.1, t.0));
    while !tiles.is_empty() {
        let mut batch = vec![tiles.remove(0)];
        let mut i = 0;
        while i < tiles.len() {
            let t = tiles[i];
            let (x0, y0, x1, y1) = bounds(&batch, t);
            if ((x1 - x0) * (y1 - y0)) as u64 * (TILE as u64 * TILE as u64) <= 12 * 1024 * 1024 / 4 {
                batch.push(tiles.remove(i));
            } else {
                i += 1;
            }
        }
        let tx0 = batch.iter().map(|t| t.0).min().unwrap();
        let ty0 = batch.iter().map(|t| t.1).min().unwrap();
        let tx1 = batch.iter().map(|t| t.0).max().unwrap() + 1;
        let ty1 = batch.iter().map(|t| t.1).max().unwrap() + 1;
        let ux = tx0 * TILE;
        let uy = ty0 * TILE;
        let uw = (tx1 * TILE).min(full_w).saturating_sub(ux) as i32;
        let uh = (ty1 * TILE).min(full_h).saturating_sub(uy) as i32;
        if uw <= 0 || uh <= 0 {
            continue;
        }
        let Some(data) = draw(&page, s, ux as i32, uy as i32, uw, uh, recolor, rot) else { continue };
        let stride = uw as usize * 4;
        for (tx, ty) in batch {
            let x = (tx * TILE - ux) as usize;
            let y = (ty * TILE - uy) as usize;
            let w = ((tx + 1) * TILE).min(full_w).saturating_sub(tx * TILE) as usize;
            let h = ((ty + 1) * TILE).min(full_h).saturating_sub(ty * TILE) as usize;
            if w == 0 || h == 0 {
                continue;
            }
            let mut tile = Vec::with_capacity(w * h * 4);
            for row in y..y + h {
                let start = row * stride + x * 4;
                tile.extend_from_slice(&data[start..start + w * 4]);
            }
            out.push(Rendered {
                key: TileKey { page: job.page, scale: job.scale, tx, ty },
                generation: 0,
                w: w as i32,
                h: h as i32,
                data: tile,
            });
        }
    }
    out
}

fn bounds(batch: &[(u32, u32)], extra: (u32, u32)) -> (u32, u32, u32, u32) {
    let mut b = (extra.0, extra.1, extra.0 + 1, extra.1 + 1);
    for t in batch {
        b = (b.0.min(t.0), b.1.min(t.1), b.2.max(t.0 + 1), b.3.max(t.1 + 1));
    }
    b
}

/// Render the device-pixel rectangle (x, y, w, h) of a page at `scale` into packed BGRA rows.
#[allow(clippy::too_many_arguments)]
fn draw(page: &poppler::Page, scale: f64, x: i32, y: i32, w: i32, h: i32, recolor: Option<Recolor>, rot: u32) -> Option<Vec<u8>> {
    // ARGB32 painted opaque white: exactly GDK's default memory format, so GTK can draw
    // the bytes without converting them.
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().ok()?;
        cr.translate(-x as f64, -y as f64);
        cr.scale(scale, scale);
        let (pw, ph) = page.size();
        use std::f64::consts::{FRAC_PI_2, PI};
        match rot {
            90 => {
                cr.translate(ph, 0.0);
                cr.rotate(FRAC_PI_2);
            }
            180 => {
                cr.translate(pw, ph);
                cr.rotate(PI);
            }
            270 => {
                cr.translate(0.0, pw);
                cr.rotate(3.0 * FRAC_PI_2);
            }
            _ => {}
        }
        // Highlights are drawn here as flat marker strokes (multiply, like ink on paper)
        // instead of poppler's rounded appearance.
        if pdf::render_without_highlights(page, &cr) {
            cr.set_operator(cairo::Operator::Multiply);
            for (_, quads, (r, g, b)) in pdf::page_highlights(page) {
                cr.set_source_rgb(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
                for q in quads {
                    cr.rectangle(q.x0, q.y0, q.x1 - q.x0, q.y1 - q.y0);
                }
                let _ = cr.fill();
            }
        } else {
            page.render(&cr);
        }
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().ok()?;
    let row = w as usize * 4;
    let mut out = Vec::with_capacity(row * h as usize);
    for r in 0..h as usize {
        out.extend_from_slice(&data[r * stride..r * stride + row]);
    }
    if let Some(rc) = recolor {
        apply_recolor(&mut out, rc);
    }
    Some(out)
}

/// Lightness inversion between the theme's background and foreground. Hue offsets from gray
/// are kept so colored figures stay distinguishable. Bytes are B, G, R, A (opaque).
pub fn apply_recolor(px: &mut [u8], rc: Recolor) {
    let (bg, fg) = (rc.bg, rc.fg);
    for p in px.as_chunks_mut::<4>().0 {
        let (b, g, r) = (p[0] as i32, p[1] as i32, p[2] as i32);
        let l = (r * 77 + g * 150 + b * 29) >> 8; // 0..255
        let mix = |c_fg: u8, c_bg: u8| -> i32 { (c_fg as i32 * (255 - l) + c_bg as i32 * l) / 255 };
        let base = [mix(fg[2], bg[2]), mix(fg[1], bg[1]), mix(fg[0], bg[0])]; // b, g, r
        p[0] = (base[0] + (b - l) * 3 / 4).clamp(0, 255) as u8;
        p[1] = (base[1] + (g - l) * 3 / 4).clamp(0, 255) as u8;
        p[2] = (base[2] + (r - l) * 3 / 4).clamp(0, 255) as u8;
        p[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Highlights are drawn by us (flat, multiplied), not skipped.
    #[test]
    fn highlights_are_drawn() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf");
        let doc = pdf::open_raw(&path, None).unwrap();
        let mut seen = false;
        for i in 0..doc.n_pages() {
            let page = doc.page(i).unwrap();
            let Some((_, quads, _)) = pdf::page_highlights(&page).into_iter().find(|h| !h.1.is_empty()) else { continue };
            let q = quads[0];
            let (w, h) = page.size();
            let px = draw(&page, 1.0, 0, 0, w.ceil() as i32, h.ceil() as i32, None, 0).unwrap();
            // Average over the quad: colored, not white paper.
            let (mut sum, mut n) = ([0u64; 3], 0u64);
            for y in q.y0.ceil() as usize..q.y1.floor() as usize {
                for x in q.x0.ceil() as usize..q.x1.floor() as usize {
                    let o = (y * w.ceil() as usize + x) * 4;
                    for c in 0..3 {
                        sum[c] += px[o + c] as u64;
                    }
                    n += 1;
                }
            }
            let avg: Vec<u64> = sum.iter().map(|s| s / n.max(1)).collect();
            assert!(avg.iter().max().unwrap() - avg.iter().min().unwrap() > 30, "page {i} quad {q:?} avg {avg:?}");
            seen = true;
        }
        if !seen {
            eprintln!("fixture has no highlights; nothing checked");
        }
    }

    #[test]
    fn recolor_maps_white_to_bg_and_black_to_fg() {
        let rc = Recolor { bg: [10, 20, 30], fg: [200, 210, 220] };
        let mut px = vec![255, 255, 255, 255, 0, 0, 0, 255];
        apply_recolor(&mut px, rc);
        // B, G, R order: bg is (r=10, g=20, b=30).
        assert_eq!(&px[0..3], &[30, 20, 10]);
        assert_eq!(&px[4..7], &[220, 210, 200]);
    }

    #[test]
    fn quantize_rounds() {
        assert_eq!(quantize(1.23456), 1235);
    }
}
