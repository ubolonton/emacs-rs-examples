//! Rasterize pages into `0xAARRGGBB` images: each page centered on a desk-colored background, with
//! a border and a drop shadow.

use std::{
    collections::HashMap,
    num::NonZeroUsize,
    ops::Range,
    panic,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use tiny_skia::Pixmap;
use typst::{
    layout::{Abs, Point},
    utils::{Scalar, hash128},
};
use typst_layout::{Page, PagedDocument};
use typst_render::RenderOptions;

use crate::sync::Caret;

/// Space around each page, in pixels. It holds the border and the shadow.
pub const MARGIN: u32 = 16;
/// Paper color under transparent page areas, e.g. with `page(fill: none)`.
const PAPER: u32 = 0xFFFF_FFFF;
const OPAQUE: u32 = 0xFF00_0000;
const BLACK: u32 = 0xFF00_0000;
const WHITE: u32 = 0xFFFF_FFFF;
/// The shadow is the page rectangle moved down, faded out over this distance, in pixels. Offset
/// plus radius must fit in `MARGIN`.
const SHADOW_RADIUS: f64 = 10.0;
const SHADOW_OFFSET: usize = 3;
/// Shadow opacity at the page edge. A dark desk needs more to show the shadow.
const LIGHT_DESK_SHADOW: f64 = 0.35;
const DARK_DESK_SHADOW: f64 = 0.6;
/// Opacity of the border color (black on a light desk, white on a dark one) over the desk.
const BORDER_OPACITY: f64 = 0.2;
/// Height of the caret bar above and below the baseline, as fractions of the font size.
const CARET_ASCENT: f64 = 0.8;
const CARET_DESCENT: f64 = 0.2;
/// Width of the caret bar: a fraction of the font size, but at least `MIN_CARET_WIDTH` pixels.
const CARET_WIDTH: f64 = 0.08;
const MIN_CARET_WIDTH: f64 = 2.0;
/// The line band is the caret bar's rows plus this fraction of the font size on each side.
const BAND_PADDING: f64 = 0.15;
const BAND_OPACITY: f64 = 0.1;
/// The largest page image, in pixels. Bounds memory use at high zoom.
pub const MAX_PAGE_PIXELS: f64 = 16_000_000.0;
pub const MIN_PIXEL_PER_PT: f64 = 0.05;
/// Color around a slide, as `0xAARRGGBB`.
const SLIDE_BACKGROUND: u32 = BLACK;

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

/// How Lisp wants the slide of a presentation to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlideView {
    /// Page index, 0-based. Too large an index shows the last page.
    pub page: usize,
    /// Size of the presentation window body, in pixels. The page fits inside, centered.
    pub width: u32,
    pub height: u32,
}

/// Everything that affects the pixels of a page image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    page: u128,
    pixel_per_pt: u64,
    width: u32,
    /// Image height of a slide. 0 for preview pages, whose height follows from the page.
    height: u32,
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
    /// Where the page is in the image.
    pub page: Rect,
    pub pixel_per_pt: f64,
    key: Key,
}

impl PageImage {
    /// Return the page point at pixel X, Y of the image, or `None` if it is outside the page.
    pub fn point_at(&self, x: f64, y: f64) -> Option<Point> {
        let (x, y) = (x - self.page.x as f64, y - self.page.y as f64);
        let inside = (0.0..=self.page.width as f64).contains(&x)
            && (0.0..=self.page.height as f64).contains(&y);
        inside.then(|| {
            Point::new(
                Abs::pt(x / self.pixel_per_pt),
                Abs::pt(y / self.pixel_per_pt),
            )
        })
    }

    /// Return the pixel position of the page point POINT in the image.
    pub fn pixel_at(&self, point: Point) -> (f64, f64) {
        (
            self.page.x as f64 + point.x.to_pt() * self.pixel_per_pt,
            self.page.y as f64 + point.y.to_pt() * self.pixel_per_pt,
        )
    }
}

/// Render the pages of DOCUMENT for VIEW. Reuse the images in PREVIOUS that would not change, even
/// if their pages moved (e.g. after a page was inserted before them). Render the other pages in
/// parallel.
pub fn render_pages(
    document: &PagedDocument,
    view: View,
    previous: &[Arc<PageImage>],
) -> Vec<Arc<PageImage>> {
    let pages = document.pages();
    let pixel_per_pt = pixel_per_pt(pages, view);
    let reusable: HashMap<Key, &Arc<PageImage>> =
        previous.iter().map(|image| (image.key, image)).collect();
    let keys: Vec<Key> = pages
        .iter()
        .map(|page| Key {
            page: hash128(page),
            pixel_per_pt: pixel_per_pt.to_bits(),
            width: view.width,
            height: 0,
            desk: view.desk,
        })
        .collect();
    let mut images: Vec<Option<Arc<PageImage>>> = keys
        .iter()
        .map(|key| reusable.get(key).map(|image| Arc::clone(image)))
        .collect();
    let missing: Vec<usize> = (0..pages.len())
        .filter(|&index| images[index].is_none())
        .collect();
    let render = |index: usize| render_page(&pages[index], pixel_per_pt, view, keys[index]);
    for (index, image) in render_in_parallel(&missing, render) {
        images[index] = Some(Arc::new(image));
    }
    images.into_iter().flatten().collect()
}

/// Call RENDER on each of INDICES, on up to one thread per core. Return the results with their
/// indices. A panic in a render thread resumes on the calling thread.
fn render_in_parallel<F>(indices: &[usize], render: F) -> Vec<(usize, PageImage)>
where
    F: Fn(usize) -> PageImage + Sync,
{
    let threads = thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(indices.len());
    if threads <= 1 {
        return indices
            .iter()
            .map(|&index| (index, render(index)))
            .collect();
    }
    thread::scope(|scope| {
        let render = &render;
        // Thread N renders every Nth page, so that pages of similar cost spread out.
        let handles: Vec<_> = (0..threads)
            .map(|thread| {
                scope.spawn(move || {
                    indices
                        .iter()
                        .skip(thread)
                        .step_by(threads)
                        .map(|&index| (index, render(index)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| panic::resume_unwind(panic))
            })
            .collect()
    })
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
    clamp_scale(fit, max_area)
}

/// Return the scale FIT, but small enough that an area of AREA square points fits the pixel
/// budget of a page image.
pub fn clamp_scale(fit: f64, area: f64) -> f64 {
    let budget = (MAX_PAGE_PIXELS / area).sqrt();
    // `max` last: it also replaces NaN, from empty pages.
    fit.min(budget).max(MIN_PIXEL_PER_PT)
}

/// Rasterize PAGE at PIXEL_PER_PT.
pub fn rasterize(page: &Page, pixel_per_pt: f64) -> Pixmap {
    let options = RenderOptions {
        pixel_per_pt: Scalar::new(pixel_per_pt),
        render_bleed: false,
    };
    typst_render::render(page, &options)
}

/// Return the pixels of PIXMAP as `0xAARRGGBB`, on paper where it is transparent.
pub fn opaque_pixels(pixmap: &Pixmap) -> Vec<u32> {
    let (pixels, _) = pixmap.data().as_chunks::<4>();
    pixels.iter().map(|rgba| over(*rgba, PAPER)).collect()
}

/// Copy PIXMAP onto PIXELS, whose rows are WIDTH long, at RECT, on paper where it is
/// transparent.
fn paint(pixels: &mut [u32], width: usize, rect: Rect, pixmap: &Pixmap) {
    for (row_index, source) in pixmap.data().chunks_exact(4 * rect.width).enumerate() {
        let start = (rect.y + row_index) * width + rect.x;
        let row = &mut pixels[start..start + rect.width];
        let (source_pixels, _) = source.as_chunks::<4>();
        for (pixel, rgba) in row.iter_mut().zip(source_pixels) {
            *pixel = over(*rgba, PAPER);
        }
    }
}

fn render_page(page: &Page, pixel_per_pt: f64, view: View, key: Key) -> PageImage {
    let pixmap = rasterize(page, pixel_per_pt);
    let (page_width, page_height) = (pixmap.width() as usize, pixmap.height() as usize);
    let margin = MARGIN as usize;
    let width = (page_width + 2 * margin).max(view.width as usize);
    let height = page_height + 2 * margin;
    let (page_x, page_y) = ((width - page_width) / 2, margin);
    let desk = OPAQUE | (view.desk & 0xFF_FFFF);
    let mut pixels = vec![desk; width * height];
    let page = Rect {
        x: page_x,
        y: page_y,
        width: page_width,
        height: page_height,
    };
    decorate(&mut pixels, width, page, desk);
    paint(&mut pixels, width, page, &pixmap);
    PageImage {
        serial: next_serial(),
        width,
        height,
        pixels,
        page,
        pixel_per_pt,
        key,
    }
}

/// Render the page of VIEW in DOCUMENT as a slide: as large as fits the view, centered, on black.
/// Reuse PREVIOUS if it would not change. Return `None` if DOCUMENT has no pages.
pub fn render_slide(
    document: &PagedDocument,
    view: SlideView,
    previous: Option<&Arc<PageImage>>,
) -> Option<Arc<PageImage>> {
    let pages = document.pages();
    let page = pages.get(view.page.min(pages.len().checked_sub(1)?))?;
    let size = page.frame.size();
    let fit = (f64::from(view.width.max(1)) / size.x.to_pt())
        .min(f64::from(view.height.max(1)) / size.y.to_pt());
    let pixel_per_pt = clamp_scale(fit, size.x.to_pt() * size.y.to_pt());
    let key = Key {
        page: hash128(page),
        pixel_per_pt: pixel_per_pt.to_bits(),
        width: view.width,
        height: view.height,
        desk: SLIDE_BACKGROUND,
    };
    if let Some(previous) = previous.filter(|image| image.key == key) {
        return Some(Arc::clone(previous));
    }
    let pixmap = rasterize(page, pixel_per_pt);
    let (page_width, page_height) = (pixmap.width() as usize, pixmap.height() as usize);
    // Rounding can make the page a pixel larger than the view.
    let width = (view.width as usize).max(page_width);
    let height = (view.height as usize).max(page_height);
    let rect = Rect {
        x: (width - page_width) / 2,
        y: (height - page_height) / 2,
        width: page_width,
        height: page_height,
    };
    let mut pixels = vec![SLIDE_BACKGROUND; width * height];
    paint(&mut pixels, width, rect, &pixmap);
    Some(Arc::new(PageImage {
        serial: next_serial(),
        width,
        height,
        pixels,
        page: rect,
        pixel_per_pt,
        key,
    }))
}

/// Return a new serial for an image. Serials are unique across sessions and image kinds.
pub fn next_serial() -> u64 {
    NEXT_SERIAL.fetch_add(1, Ordering::Relaxed)
}

/// Return the pixel rows of the line band of CARET in IMAGE.
pub fn caret_band(image: &PageImage, caret: &Caret) -> Range<usize> {
    let (_, baseline) = image.pixel_at(caret.point);
    let size = caret.size.to_pt() * image.pixel_per_pt;
    let page_rows = image.page.y..image.page.y + image.page.height;
    let top = (baseline - (CARET_ASCENT + BAND_PADDING) * size)
        .floor()
        .max(0.0) as usize;
    let bottom = (baseline + (CARET_DESCENT + BAND_PADDING) * size)
        .ceil()
        .max(0.0) as usize;
    top.clamp(page_rows.start, page_rows.end)..bottom.clamp(page_rows.start, page_rows.end)
}

/// Draw CARET onto BUFFER, a copy of the pixels of IMAGE: a translucent band across the page at
/// the caret's line, and a bar in COLOR (`0xRRGGBB`).
pub fn draw_caret(buffer: &mut [u32], image: &PageImage, caret: &Caret, color: u32) {
    if buffer.len() != image.width * image.height {
        return;
    }
    let color = OPAQUE | (color & 0xFF_FFFF);
    let width = image.width;
    let page_columns = image.page.x..image.page.x + image.page.width;
    for y in caret_band(image, caret) {
        for pixel in &mut buffer[y * width + page_columns.start..y * width + page_columns.end] {
            *pixel = mix(color, *pixel, BAND_OPACITY);
        }
    }
    let (x, baseline) = image.pixel_at(caret.point);
    draw_bar(
        buffer,
        width,
        x,
        baseline,
        caret.size.to_pt() * image.pixel_per_pt,
        color,
    );
}

/// Draw a caret bar in the opaque `0xAARRGGBB` COLOR onto BUFFER, whose rows are WIDTH long: at
/// column X, for text of SIZE pixels whose baseline is at row BASELINE.
pub fn draw_bar(buffer: &mut [u32], width: usize, x: f64, baseline: f64, size: f64, color: u32) {
    let height = buffer.len() / width.max(1);
    let bar_width = (CARET_WIDTH * size).max(MIN_CARET_WIDTH).round();
    let left = (x - bar_width / 2.0).round().max(0.0) as usize;
    let columns = left.min(width)..(left + bar_width as usize).min(width);
    let top = (baseline - CARET_ASCENT * size).round().max(0.0) as usize;
    let bottom = (baseline + CARET_DESCENT * size).round().max(0.0) as usize;
    for y in top.min(height)..bottom.min(height) {
        buffer[y * width + columns.start..y * width + columns.end].fill(color);
    }
}

/// A rectangle in an image, in pixels.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Draw a drop shadow and a 1-pixel border around PAGE, onto the desk-colored PIXELS, whose rows
/// are WIDTH long. The page area itself is left alone.
fn decorate(pixels: &mut [u32], width: usize, page: Rect, desk: u32) {
    let height = pixels.len() / width;
    let dark = luminance(desk) < 0.5;
    let opacity = if dark {
        DARK_DESK_SHADOW
    } else {
        LIGHT_DESK_SHADOW
    };
    let (left, right) = (page.x as f64, (page.x + page.width) as f64);
    let top = (page.y + SHADOW_OFFSET) as f64;
    let bottom = top + page.height as f64;
    let reach = SHADOW_RADIUS.ceil() as usize;
    let rows =
        page.y.saturating_sub(reach)..(page.y + page.height + SHADOW_OFFSET + reach).min(height);
    let columns = page.x.saturating_sub(reach)..(page.x + page.width + reach).min(width);
    for y in rows {
        // Skip the page area: in its rows, only the strips left and right of it.
        let spans = if (page.y..page.y + page.height).contains(&y) {
            [columns.start..page.x, page.x + page.width..columns.end]
        } else {
            [columns.clone(), 0..0]
        };
        for x in spans.into_iter().flatten() {
            // Distance from the pixel center to the shadow rectangle.
            let (center_x, center_y) = (x as f64 + 0.5, y as f64 + 0.5);
            let dx = (left - center_x).max(center_x - right).max(0.0);
            let dy = (top - center_y).max(center_y - bottom).max(0.0);
            let fade = (1.0 - dx.hypot(dy) / SHADOW_RADIUS).max(0.0);
            let pixel = &mut pixels[y * width + x];
            *pixel = mix(BLACK, *pixel, opacity * fade * fade);
        }
    }
    let border = mix(if dark { WHITE } else { BLACK }, desk, BORDER_OPACITY);
    let (outer_left, outer_top) = (page.x.saturating_sub(1), page.y.saturating_sub(1));
    let (outer_right, outer_bottom) = (page.x + page.width, page.y + page.height);
    let (outer_right, outer_bottom) = (outer_right.min(width - 1), outer_bottom.min(height - 1));
    for y in [outer_top, outer_bottom] {
        pixels[y * width + outer_left..=y * width + outer_right].fill(border);
    }
    for y in outer_top..=outer_bottom {
        pixels[y * width + outer_left] = border;
        pixels[y * width + outer_right] = border;
    }
}

/// Relative luminance of the `0xAARRGGBB` color COLOR, from 0 to 1. Ignores gamma, which is
/// enough to tell light from dark.
fn luminance(color: u32) -> f64 {
    let channel = |shift: u32| f64::from((color >> shift) & 0xFF) / 255.0;
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// Mix the opaque `0xAARRGGBB` colors TOP over BOTTOM, with TOP at OPACITY (0 to 1).
pub fn mix(top: u32, bottom: u32, opacity: f64) -> u32 {
    let channel = |shift: u32| {
        let (top, bottom) = (
            f64::from((top >> shift) & 0xFF),
            f64::from((bottom >> shift) & 0xFF),
        );
        ((bottom + (top - bottom) * opacity).round() as u32).min(255) << shift
    };
    OPAQUE | channel(16) | channel(8) | channel(0)
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

    const DESK: u32 = 0xFFE0_E0E0;

    /// Decorate a 48x48 desk with a 16x16 page in the middle. Return the pixels.
    fn decorated(desk: u32) -> (Vec<u32>, Rect) {
        let mut pixels = vec![desk; 48 * 48];
        let page = Rect {
            x: 16,
            y: 16,
            width: 16,
            height: 16,
        };
        decorate(&mut pixels, 48, page, desk);
        (pixels, page)
    }

    #[test]
    fn decorate_draws_border_and_shadow_in_margin() {
        let (pixels, page) = decorated(DESK);
        let at = |x: usize, y: usize| pixels[y * 48 + x];
        // The page is untouched, and far corners keep the desk color.
        assert_eq!(at(20, 20), DESK);
        assert_eq!(at(0, 0), DESK);
        // A border right outside the page, darker than the light desk.
        let border = at(page.x - 1, 20);
        assert_eq!(at(page.x + page.width, 20), border);
        assert_eq!(at(20, page.y - 1), border);
        assert!(luminance(border) < luminance(DESK));
        // The shadow fades out downwards, below the border.
        let below = |distance: usize| at(24, page.y + page.height + distance);
        assert!(luminance(below(1)) < luminance(below(5)));
        assert!(luminance(below(5)) < luminance(DESK));
        assert_eq!(below(15), DESK);
    }

    #[test]
    fn decorate_lightens_border_on_dark_desk() {
        let desk = 0xFF14_1414;
        let (pixels, page) = decorated(desk);
        assert!(luminance(pixels[20 * 48 + page.x - 1]) > luminance(desk));
    }

    /// A white 100x60 page at 2 pixels per point, in the middle of a 140x100 image.
    fn blank_image() -> PageImage {
        let (width, height) = (140, 100);
        PageImage {
            serial: 0,
            width,
            height,
            pixels: vec![WHITE; width * height],
            page: Rect {
                x: 20,
                y: 20,
                width: 100,
                height: 60,
            },
            pixel_per_pt: 2.0,
            key: Key {
                page: 0,
                pixel_per_pt: 0,
                width: 0,
                height: 0,
                desk: 0,
            },
        }
    }

    #[test]
    fn draw_caret_draws_bar_and_band() {
        let image = blank_image();
        // 10pt text with its baseline at 15pt: 30 pixels below the page top.
        let caret = Caret {
            page: 0,
            point: Point::new(Abs::pt(10.0), Abs::pt(15.0)),
            size: Abs::pt(10.0),
        };
        let mut buffer = image.pixels.clone();
        draw_caret(&mut buffer, &image, &caret, 0xFF_0000);
        let at = |x: usize, y: usize| buffer[y * image.width + x];
        let baseline = 20 + 30;
        // The bar is at x = 20 + 2 * 10, from 0.8 em above the baseline to 0.2 em below.
        assert_eq!(at(40, baseline - 1), 0xFFFF_0000);
        assert_eq!(at(40, baseline - 15), 0xFFFF_0000);
        assert_eq!(at(40, baseline + 3), 0xFFFF_0000);
        // The band spans the page width, and stays inside the page.
        let band = at(21, baseline - 1);
        assert_eq!(band, mix(0xFFFF_0000, WHITE, BAND_OPACITY));
        assert_eq!(at(119, baseline - 1), band);
        assert_eq!(at(19, baseline - 1), WHITE);
        assert_eq!(at(21, baseline - 25), WHITE);
        assert_eq!(caret_band(&image, &caret), baseline - 19..baseline + 7);
    }

    #[test]
    fn render_in_parallel_renders_each_index_once() {
        let indices = [0, 2, 3, 5, 7, 8, 9, 11, 12, 14];
        let mut results = render_in_parallel(&indices, |index| PageImage {
            serial: index as u64,
            ..blank_image()
        });
        results.sort_by_key(|(index, _)| *index);
        let serials: Vec<(usize, u64)> = results
            .iter()
            .map(|(index, image)| (*index, image.serial))
            .collect();
        let expected: Vec<(usize, u64)> =
            indices.iter().map(|&index| (index, index as u64)).collect();
        assert_eq!(serials, expected);
    }

    #[test]
    fn mix_interpolates_channels() {
        assert_eq!(mix(WHITE, BLACK, 0.0), BLACK);
        assert_eq!(mix(WHITE, BLACK, 1.0), WHITE);
        assert_eq!(mix(0xFF20_4080, BLACK, 0.5), 0xFF10_2040);
    }

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
