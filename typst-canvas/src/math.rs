//! Inline equation previews: the equation at the source cursor, cut out of its laid-out page, and
//! rendered at the scale of the source buffer's text.
//!
//! The cut-out reuses the layout of the last good compile, so the equation keeps the document's
//! context (`#set`, `#show`, `#let`), and needs no extra compile. It takes the frame items between
//! the equation's introspection tags, not the items whose spans are in the equation: content from
//! a `#let` binding has spans outside the equation. The items are rendered again, not cropped from
//! the page image, because the page image's scale follows the preview width and zoom, but the
//! cut-out must match the text size of the source buffer.

use std::iter;

use typst::{
    introspection::{Location, Tag},
    layout::{Abs, Frame, FrameItem, GroupItem, Point, Rect, Transform},
    math::EquationElem,
    syntax::{LinkedNode, Side, Source, Span, SyntaxKind},
};
use typst_layout::{Page, PagedDocument};

use crate::{render, sync::Caret};

/// Space around the equation, as a fraction of its largest font size.
const PADDING: f64 = 0.3;
/// Font size of an equation without text (e.g. only a line), in points.
const FALLBACK_SIZE: f64 = 10.0;
/// Opacity of a stale image's background over its pixels.
const STALE_DIMMING: f64 = 0.55;

/// A rendered equation, owned by Rust. Defuns copy it into a canvas.
#[derive(Debug)]
pub struct EquationImage {
    /// Unique per rendered image, like the serials of page images.
    pub serial: u64,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    /// Byte offset of the equation in the text that it came from. While the document is stale,
    /// the image is shown only for an equation at the same offset.
    pub start: usize,
}

impl EquationImage {
    /// Return the pixels, faded towards the background (the top left pixel, which is padding).
    pub fn dimmed(&self) -> Vec<u32> {
        let background = self.pixels.first().copied().unwrap_or_default();
        self.pixels
            .iter()
            .map(|&pixel| render::mix(background, pixel, STALE_DIMMING))
            .collect()
    }
}

/// Return the innermost equation (`$...$`, inline or display) of SOURCE whose delimiters enclose
/// byte offset CURSOR.
pub fn equation_at(source: &Source, cursor: usize) -> Option<LinkedNode<'_>> {
    let root = LinkedNode::new(source.root());
    let leaf = root
        .leaf_at(cursor, Side::After)
        .or_else(|| root.leaf_at(cursor, Side::Before))?;
    iter::successors(Some(leaf), |node| node.parent().cloned()).find(|node| {
        node.kind() == SyntaxKind::Equation
            && node.offset() < cursor
            && cursor < node.offset() + node.len()
    })
}

/// The items of an equation, cut out of its page.
#[derive(Debug)]
pub struct Cutout {
    /// Page index, 0-based.
    pub page: usize,
    /// The page frame, with only the equation's items.
    frame: Frame,
    /// Bounding box of the items on the page.
    bounds: Rect,
    /// The largest font size in the equation, or zero if it has no text.
    size: Abs,
}

/// Return the items of the equation whose syntax node has SPAN, or `None` if DOCUMENT shows no
/// such equation, or it is empty.
pub fn cut_out(document: &PagedDocument, span: Span) -> Option<Cutout> {
    document
        .pages()
        .iter()
        .enumerate()
        .find_map(|(index, page)| {
            let mut scan = Scan::new(span);
            let frame = scan.prune(&page.frame, Transform::identity());
            (scan.state != State::Before).then_some((index, frame, scan))
        })
        .and_then(|(page, frame, scan)| {
            Some(Cutout {
                page,
                frame,
                bounds: scan.bounds?,
                size: scan.size,
            })
        })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Before,
    Inside,
    After,
}

/// A walk over the items of a page, in layout order, that keeps the items between the start and
/// end tags of an equation.
struct Scan {
    span: Span,
    location: Option<Location>,
    state: State,
    bounds: Option<Rect>,
    size: Abs,
}

impl Scan {
    fn new(span: Span) -> Self {
        Self {
            span,
            location: None,
            state: State::Before,
            bounds: None,
            size: Abs::zero(),
        }
    }

    /// Return a copy of FRAME with only the items of the equation, and the groups that contain
    /// them. TRANSFORM maps FRAME to the page.
    fn prune(&mut self, frame: &Frame, transform: Transform) -> Frame {
        let mut pruned = Frame::new(frame.size(), frame.kind());
        for (position, item) in frame.items() {
            if self.state == State::After {
                break;
            }
            let inside = self.state == State::Inside;
            let local = transform.pre_concat(Transform::translate(position.x, position.y));
            match item {
                FrameItem::Group(group) => {
                    let frame = self.prune(&group.frame, local.pre_concat(group.transform));
                    if !frame.is_empty() {
                        let group = GroupItem {
                            frame,
                            ..group.clone()
                        };
                        pruned.push(*position, FrameItem::Group(group));
                    }
                }
                FrameItem::Tag(Tag::Start(content, _))
                    if self.state == State::Before
                        && content.span() == self.span
                        && content.is::<EquationElem>() =>
                {
                    self.state = State::Inside;
                    self.location = content.location();
                }
                FrameItem::Tag(Tag::End(location, ..))
                    if inside && Some(*location) == self.location =>
                {
                    self.state = State::After;
                }
                FrameItem::Text(text) if inside => {
                    self.include(local, text.bbox());
                    self.size = self.size.max(text.size);
                    pruned.push(*position, item.clone());
                }
                FrameItem::Shape(shape, _) if inside => {
                    self.include(local, shape.bbox(true));
                    pruned.push(*position, item.clone());
                }
                FrameItem::Image(_, size, _) if inside => {
                    self.include(local, Rect::from_pos_size(Point::zero(), *size));
                    pruned.push(*position, item.clone());
                }
                _ => {}
            }
        }
        pruned
    }

    /// Add RECT, in coordinates that TRANSFORM maps to the page, to the bounds. RECT can be
    /// upside down: `TextItem::bbox` flips Y after it computes the box.
    fn include(&mut self, transform: Transform, rect: Rect) {
        // Text without visible glyphs, e.g. a space, has an infinite, inverted box.
        let finite = |length: Abs| length.to_pt().is_finite();
        if rect.min.x > rect.max.x || !finite(rect.min.y) || !finite(rect.max.y) {
            return;
        }
        let corners = [
            rect.min,
            Point::new(rect.max.x, rect.min.y),
            Point::new(rect.min.x, rect.max.y),
            rect.max,
        ]
        .map(|corner| corner.transform(transform));
        let mut bounds = self.bounds.unwrap_or(Rect::new(corners[0], corners[0]));
        for corner in corners {
            bounds.min = bounds.min.min(corner);
            bounds.max = bounds.max.max(corner);
        }
        self.bounds = Some(bounds);
    }
}

/// How Lisp wants an equation to look.
#[derive(Debug, Clone, Copy)]
pub struct EquationView {
    /// Pixels per em of the equation's largest font: the font size of the source buffer.
    pub px_per_em: f64,
    /// Width limit, in pixels. A wider equation is scaled down.
    pub max_width: usize,
}

/// Render CUTOUT of PAGE for VIEW, with CARET (and its color, `0xRRGGBB`) if it is in the
/// equation. START is the byte offset of the equation in the source.
pub fn render(
    page: &Page,
    cutout: &Cutout,
    view: EquationView,
    start: usize,
    caret: Option<(Caret, u32)>,
) -> EquationImage {
    let em = if cutout.size > Abs::zero() {
        cutout.size
    } else {
        Abs::pt(FALLBACK_SIZE)
    };
    let padding = Point::splat(em * PADDING);
    let origin = cutout.bounds.min - padding;
    let size = (cutout.bounds.max + padding - origin).to_size();
    let fit = (view.px_per_em / em.to_pt()).min(view.max_width.max(1) as f64 / size.x.to_pt());
    let pixel_per_pt = render::clamp_scale(fit, size.x.to_pt() * size.y.to_pt());

    let mut frame = Frame::hard(size);
    frame.push(
        -origin,
        FrameItem::Group(GroupItem::new(cutout.frame.clone())),
    );
    let pixmap = render::rasterize(
        &Page {
            frame,
            ..page.clone()
        },
        pixel_per_pt,
    );
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
    let mut pixels = render::opaque_pixels(&pixmap);
    if let Some((caret, color)) = caret.filter(|(caret, _)| caret.page == cutout.page) {
        let to_pixels = |length: Abs| length.to_pt() * pixel_per_pt;
        let (x, baseline) = (
            to_pixels(caret.point.x - origin.x),
            to_pixels(caret.point.y - origin.y),
        );
        let inside = (0.0..width as f64).contains(&x) && (0.0..height as f64).contains(&baseline);
        if inside {
            render::draw_bar(
                &mut pixels,
                width,
                x,
                baseline,
                to_pixels(caret.size),
                0xFF00_0000 | (color & 0xFF_FFFF),
            );
        }
    }
    EquationImage {
        serial: render::next_serial(),
        width,
        height,
        pixels,
        start,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::world::{Document, PreviewWorld};

    const PAGE: &str = "#set page(width: 200pt, height: 100pt, margin: 10pt)\n";

    fn compile(text: &str) -> Document {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(mut world) = PreviewWorld::new(root, &root.join("main.typ")) else {
            panic!("main.typ is inside its root");
        };
        world.set_main_text(text);
        let compiled = world.compile();
        let Some(document) = compiled.document else {
            panic!("{:?}", compiled.diagnostics);
        };
        document
    }

    /// Return the cut-out of the equation at the first occurrence of NEEDLE in TEXT.
    fn cutout_at(text: &str, needle: &str) -> Option<Cutout> {
        let document = compile(text);
        let node = equation_at(&document.source, text.find(needle)?)?;
        cut_out(&document.paged, node.span())
    }

    #[test]
    fn equation_at_needs_cursor_inside_delimiters() {
        let source = Source::detached("A $x + y$ B\n$ z $");
        let text = source.text();
        let found = |cursor: usize| {
            equation_at(&source, cursor).map(|node| node.offset()..node.offset() + node.len())
        };
        let inline = text.find('$').map(|start| start..start + "$x + y$".len());
        assert_eq!(found(text.find('x').unwrap_or_default()), inline);
        assert_eq!(found(text.find(" y").unwrap_or_default() + 2), inline);
        // On the delimiters' outer sides, the cursor is outside.
        assert_eq!(found(text.find('$').unwrap_or_default()), None);
        assert_eq!(found(text.find(" B").unwrap_or_default()), None);
        assert!(found(text.find('z').unwrap_or_default()).is_some());
        assert_eq!(found(0), None);
    }

    #[test]
    fn cut_out_bounds_inline_equation() {
        let text = format!("{PAGE}Before $x^2 + y^2$ after.");
        let Some(cutout) = cutout_at(&text, "x^2") else {
            panic!("no cut-out");
        };
        assert_eq!(cutout.page, 0);
        assert_eq!(cutout.size, Abs::pt(11.0));
        // The equation is in the first line, after "Before ", and narrower than the line.
        let width = cutout.bounds.max.x - cutout.bounds.min.x;
        assert!(cutout.bounds.min.x > Abs::pt(30.0), "{cutout:?}");
        assert!(width > Abs::pt(20.0) && width < Abs::pt(90.0), "{cutout:?}");
        assert!(cutout.bounds.max.y < Abs::pt(30.0), "{cutout:?}");
    }

    #[test]
    fn cut_out_includes_content_from_let_bindings() {
        let literal = cutout_at(&format!("{PAGE}$x + y$"), "x");
        let bound = cutout_at(&format!("{PAGE}#let y = $y$\n$x + y$"), "x +");
        let width = |cutout: Option<Cutout>| {
            cutout.map(|cutout| (cutout.bounds.max.x - cutout.bounds.min.x).to_pt())
        };
        let (Some(literal), Some(bound)) = (width(literal), width(bound)) else {
            panic!("no cut-out");
        };
        assert!((literal - bound).abs() < 0.01, "{literal} vs {bound}");
    }

    #[test]
    fn cut_out_finds_display_equation_on_later_page() {
        let text = format!("{PAGE}A\n#pagebreak()\n$ sum_(n=1)^oo 1/n^2 $");
        let Some(cutout) = cutout_at(&text, "sum") else {
            panic!("no cut-out");
        };
        assert_eq!(cutout.page, 1);
        // The big operator and its limits are taller than a line.
        assert!(cutout.bounds.max.y - cutout.bounds.min.y > Abs::pt(20.0));
    }

    #[test]
    fn cut_out_skips_empty_equation() {
        assert!(cutout_at(&format!("{PAGE}A $$ B"), "$$").is_none());
    }

    #[test]
    fn render_scales_to_text_size_and_width_limit() {
        let text = format!("{PAGE}$x + y$");
        let document = compile(&text);
        let Some(cutout) = equation_at(&document.source, text.rfind('x').unwrap_or_default())
            .and_then(|node| cut_out(&document.paged, node.span()))
        else {
            panic!("no cut-out");
        };
        let page = &document.paged.pages()[0];
        let view = |px_per_em, max_width| EquationView {
            px_per_em,
            max_width,
        };
        let small = render(page, &cutout, view(11.0, 1000), 0, None);
        let large = render(page, &cutout, view(22.0, 1000), 0, None);
        assert_eq!(small.pixels.len(), small.width * small.height);
        assert!(large.width >= 2 * small.width - 1 && large.width <= 2 * small.width + 1);
        let limited = render(page, &cutout, view(22.0, small.width), 0, None);
        assert!(limited.width <= small.width);
        // The default page fill is white, and the text is black.
        let darkest = |pixels: &[u32]| pixels.iter().map(|&pixel| pixel & 0xFF).min();
        assert_eq!(small.pixels[0], 0xFFFF_FFFF);
        assert!(darkest(&small.pixels) < Some(0x40));
        // Dimming lightens the text towards the white background.
        assert!(darkest(&small.dimmed()) > Some(0x80));
    }
}
