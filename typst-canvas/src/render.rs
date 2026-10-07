//! Rasterize pages into `0xAARRGGBB` images: each page centered on a desk-colored background.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use typst::utils::{Scalar, hash128};
use typst_layout::{Page, PagedDocument};
use typst_render::RenderOptions;

/// Space around each page, in pixels.
pub const MARGIN: u32 = 16;
/// Paper color under transparent page areas, e.g. with `page(fill: none)`.
const PAPER: u32 = 0xFFFF_FFFF;
const OPAQUE: u32 = 0xFF00_0000;
/// The largest page image, in pixels. Bounds memory use at high zoom.
const MAX_PAGE_PIXELS: f64 = 16_000_000.0;
const MIN_PIXEL_PER_PT: f64 = 0.05;

/// Serials are unique across sessions, so that Lisp can compare them without knowing where an
/// image came from.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// How Lisp wants the pages to look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    /// Width of the preview window body, in pixels. At zoom 1, the widest page fits this width.
    pub width: u32,
    pub zoom: f64,
    /// Color around the pages, as `0xRRGGBB`.
    pub desk: u32,
}

/// Everything that affects the pixels of a page image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    page: u128,
    pixel_per_pt: u64,
    width: u32,
    desk: u32,
}

/// A rendered page, owned by Rust. Defuns copy it into a canvas.
#[derive(Debug)]
pub struct PageImage {
    /// Unique per rendered image. Lisp compares serials to skip unchanged pages.
    pub serial: u64,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    key: Key,
}

/// Render the pages of DOCUMENT for VIEW. Reuse the images in PREVIOUS that would not change, even
/// if their pages moved (e.g. after a page was inserted before them).
pub fn render_pages(
    document: &PagedDocument,
    view: View,
    previous: &[Arc<PageImage>],
) -> Vec<Arc<PageImage>> {
    let pixel_per_pt = pixel_per_pt(document.pages(), view);
    let reusable: HashMap<Key, &Arc<PageImage>> =
        previous.iter().map(|image| (image.key, image)).collect();
    document
        .pages()
        .iter()
        .map(|page| {
            let key = Key {
                page: hash128(page),
                pixel_per_pt: pixel_per_pt.to_bits(),
                width: view.width,
                desk: view.desk,
            };
            match reusable.get(&key) {
                Some(image) => Arc::clone(image),
                None => Arc::new(render_page(page, pixel_per_pt, view, key)),
            }
        })
        .collect()
}

/// Return the scale at which the widest page fits the view width, times the zoom.
fn pixel_per_pt(pages: &[Page], view: View) -> f64 {
    let (max_width, max_area) = pages
        .iter()
        .fold((0.0_f64, 0.0_f64), |(width, area), page| {
            let size = page.frame.size();
            (
                width.max(size.x.to_pt()),
                area.max(size.x.to_pt() * size.y.to_pt()),
            )
        });
    let available = f64::from(view.width.saturating_sub(2 * MARGIN).max(1));
    let fit = available / max_width * view.zoom;
    let budget = (MAX_PAGE_PIXELS / max_area).sqrt();
    // `max` last: it also replaces NaN, from empty pages.
    fit.min(budget).max(MIN_PIXEL_PER_PT)
}

fn render_page(page: &Page, pixel_per_pt: f64, view: View, key: Key) -> PageImage {
    let options = RenderOptions {
        pixel_per_pt: Scalar::new(pixel_per_pt),
        render_bleed: false,
    };
    let pixmap = typst_render::render(page, &options);
    let (page_width, page_height) = (pixmap.width() as usize, pixmap.height() as usize);
    let margin = MARGIN as usize;
    let width = (page_width + 2 * margin).max(view.width as usize);
    let height = page_height + 2 * margin;
    let (page_x, page_y) = ((width - page_width) / 2, margin);
    let mut pixels = vec![OPAQUE | (view.desk & 0xFF_FFFF); width * height];
    for (row_index, source) in pixmap.data().chunks_exact(4 * page_width).enumerate() {
        let start = (page_y + row_index) * width + page_x;
        let row = &mut pixels[start..start + page_width];
        let (source_pixels, _) = source.as_chunks::<4>();
        for (pixel, rgba) in row.iter_mut().zip(source_pixels) {
            *pixel = over(*rgba, PAPER);
        }
    }
    PageImage {
        serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
        width,
        height,
        pixels,
        key,
    }
}

/// Composite the premultiplied RGBA pixel over the opaque `0xAARRGGBB` color BACKGROUND.
fn over([red, green, blue, alpha]: [u8; 4], background: u32) -> u32 {
    let alpha = u32::from(alpha);
    let channel = |source: u8, shift: u32| {
        let below = (background >> shift) & 0xFF;
        (u32::from(source) + (below * (255 - alpha) + 127) / 255).min(255) << shift
    };
    OPAQUE | channel(red, 16) | channel(green, 8) | channel(blue, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_keeps_opaque_pixels() {
        assert_eq!(over([0x12, 0x34, 0x56, 0xFF], PAPER), 0xFF12_3456);
    }

    #[test]
    fn over_shows_background_through_transparent_pixels() {
        assert_eq!(over([0, 0, 0, 0], 0xFF20_4060), 0xFF20_4060);
        // Half-transparent black, premultiplied, over white.
        assert_eq!(over([0, 0, 0, 0x80], PAPER), 0xFF7F_7F7F);
    }
}
