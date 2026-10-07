//! Sync between the source and the pages: from a click on a page to the source (backward), and
//! from the source cursor to a caret on a page (forward).

use std::{num::NonZeroUsize, path::PathBuf};

use typst::{
    Library, World,
    diag::FileResult,
    foundations::{Bytes, Datetime, Duration},
    introspection::PagedPosition,
    layout::{Abs, Frame, FrameItem, Point},
    syntax::{FileId, LinkedNode, Side, Source, Span, SyntaxKind},
    text::{Font, FontBook},
    utils::LazyHash,
};
use typst_ide::{IdeWorld, Jump};

use crate::{
    offset,
    world::{Document, PreviewWorld},
};

/// Where a click on a page leads.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A char offset in the main file.
    Main(usize),
    /// A char offset in another file.
    File(PathBuf, usize),
    Url(String),
    /// A point on page INDEX (0-based), e.g. the destination of an internal link.
    Position(usize, Point),
}

/// Return where a click at POINT on page INDEX (0-based) of DOCUMENT leads.
pub fn jump(
    world: &PreviewWorld,
    document: &Document,
    index: usize,
    point: Point,
) -> Option<Target> {
    let snapshot = Snapshot {
        world,
        main: &document.source,
    };
    let position = PagedPosition {
        page: NonZeroUsize::new(index + 1)?,
        point,
    };
    match typst_ide::jump_from_click(&snapshot, &document.paged, &position)? {
        Jump::File(id, byte) if id == document.source.id() => Some(Target::Main(
            offset::byte_to_char(document.source.text(), byte),
        )),
        Jump::File(id, byte) => {
            let source = world.source(id).ok()?;
            Some(Target::File(
                world.path(id)?,
                offset::byte_to_char(source.text(), byte),
            ))
        }
        Jump::Url(url) => Some(Target::Url(url.to_string())),
        Jump::Position(position) => Some(Target::Position(position.page.get() - 1, position.point)),
    }
}

/// Where the source cursor is on the pages.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caret {
    /// Page index, 0-based.
    pub page: usize,
    /// Left edge of the caret, on the text baseline.
    pub point: Point,
    /// Font size of the text at the caret.
    pub size: Abs,
}

/// Return the caret for the cursor at byte offset CURSOR in the main file of DOCUMENT, or `None`
/// if the cursor is not in text that was laid out (e.g. it is in code or markup).
///
/// `typst_ide::jump_from_cursor` finds the same text node, but returns the start of the node and
/// no font size. This walks the frames the same way, but stops at the glyph of the cursor.
pub fn caret(document: &Document, cursor: usize) -> Option<Caret> {
    let is_text =
        |node: &LinkedNode| matches!(node.kind(), SyntaxKind::Text | SyntaxKind::MathText);
    let root = LinkedNode::new(document.source.root());
    let node = root
        .leaf_at(cursor, Side::Before)
        .filter(is_text)
        .or_else(|| root.leaf_at(cursor, Side::After).filter(is_text))?;
    let offset = cursor.checked_sub(node.offset())?;
    document
        .paged
        .pages()
        .iter()
        .enumerate()
        .find_map(|(page, content)| {
            let (point, size) = find_glyph(&content.frame, node.span(), offset)?;
            Some(Caret { page, point, size })
        })
}

/// Return the position and font size of the glyph at byte OFFSET in the text node SPAN, in FRAME.
/// The position is the left edge of the glyph, or the right edge of the node's last glyph if
/// OFFSET is at the end of the node.
fn find_glyph(frame: &Frame, span: Span, offset: usize) -> Option<(Point, Abs)> {
    let mut end = None;
    for &(mut position, ref item) in frame.items() {
        match item {
            FrameItem::Group(group) => {
                if let Some((point, size)) = find_glyph(&group.frame, span, offset) {
                    return Some((position + point.transform(group.transform), size));
                }
            }
            FrameItem::Text(text) => {
                for glyph in &text.glyphs {
                    let advance = glyph.x_advance.at(text.size);
                    if glyph.span.0 == span {
                        let start = usize::from(glyph.span.1);
                        let glyph_end = start + glyph.range().len();
                        if (start..glyph_end).contains(&offset) {
                            return Some((position, text.size));
                        }
                        if glyph_end == offset {
                            end = Some((position + Point::with_x(advance), text.size));
                        }
                    }
                    position.x += advance;
                }
            }
            _ => {}
        }
    }
    end
}

/// The world, but with the main file text of a document, so that the document's spans resolve.
struct Snapshot<'a> {
    world: &'a PreviewWorld,
    main: &'a Source,
}

impl World for Snapshot<'_> {
    fn library(&self) -> &LazyHash<Library> {
        self.world.library()
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.world.book()
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            self.world.source(id)
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main.id() {
            Ok(Bytes::from_string(self.main.clone()))
        } else {
            self.world.file(id)
        }
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.world.font(index)
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        self.world.today(offset)
    }
}

impl IdeWorld for Snapshot<'_> {
    fn upcast(&self) -> &dyn World {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use typst::layout::Abs;

    use super::*;

    const PAGE: &str = "#set page(width: 100pt, height: 100pt, margin: 10pt)\n";

    fn compile(text: &str) -> (PreviewWorld, Document) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(mut world) = PreviewWorld::new(root, &root.join("main.typ")) else {
            panic!("main.typ is inside its root");
        };
        world.set_main_text(text);
        let compiled = world.compile();
        let Some(document) = compiled.document else {
            panic!("{:?}", compiled.diagnostics);
        };
        (world, document)
    }

    /// A point in the first line of text, at X points from the left page edge.
    fn first_line(x: f64) -> Point {
        Point::new(Abs::pt(x), Abs::pt(15.0))
    }

    /// Return the caret at the first occurrence of NEEDLE in TEXT, plus SHIFT bytes.
    fn caret_at(document: &Document, text: &str, needle: &str, shift: usize) -> Option<Caret> {
        caret(document, text.find(needle)? + shift)
    }

    #[test]
    fn caret_follows_cursor_within_text() {
        let text = format!("{PAGE}Hello world");
        let (_, document) = compile(&text);
        let Some(start) = caret_at(&document, &text, "Hello", 0) else {
            panic!("no caret at the start of the text");
        };
        assert_eq!(start.page, 0);
        assert_eq!(start.size, Abs::pt(11.0));
        assert!((start.point.x - Abs::pt(10.0)).abs() < Abs::pt(0.01));
        let xs: Vec<_> = (1..="Hello world".len())
            .filter_map(|shift| caret_at(&document, &text, "Hello", shift))
            .map(|caret| caret.point.x)
            .collect();
        assert_eq!(xs.len(), "Hello world".len());
        assert!(xs.windows(2).all(|pair| pair[0] < pair[1]), "{xs:?}");
        assert!(xs[0] > start.point.x);
    }

    #[test]
    fn caret_finds_page() {
        let text = format!("{PAGE}A\n#pagebreak()\nB");
        let (_, document) = compile(&text);
        assert_eq!(
            caret_at(&document, &text, "B", 0).map(|caret| caret.page),
            Some(1)
        );
    }

    #[test]
    fn caret_is_hidden_outside_text() {
        let text = format!("{PAGE}Hello");
        let (_, document) = compile(&text);
        assert_eq!(caret_at(&document, &text, "page", 0), None);
    }

    #[test]
    fn click_on_text_jumps_to_char() {
        let text = format!("{PAGE}é Hello");
        let (world, document) = compile(&text);
        // Right after the left margin: before "é".
        let target = jump(&world, &document, 0, first_line(10.5));
        let start = text.chars().count() - "é Hello".chars().count();
        assert_eq!(target, Some(Target::Main(start)));
        assert_eq!(jump(&world, &document, 0, first_line(95.0)), None);
        assert_eq!(jump(&world, &document, 1, first_line(10.5)), None);
    }

    #[test]
    fn click_uses_text_of_document_after_failed_compile() {
        let text = format!("{PAGE}Hello");
        let (mut world, document) = compile(&text);
        world.set_main_text(&format!("#nope\n{text}"));
        assert!(world.compile().document.is_none());
        let start = text.chars().count() - "Hello".chars().count();
        assert_eq!(
            jump(&world, &document, 0, first_line(10.5)),
            Some(Target::Main(start))
        );
    }

    #[test]
    fn click_on_link_jumps_to_destination() {
        let text = format!(
            "{PAGE}#link(\"https://typst.app\")[Web] #link(<there>)[Here]\n#pagebreak()\n= There <there>"
        );
        let (world, document) = compile(&text);
        assert_eq!(
            jump(&world, &document, 0, first_line(11.0)),
            Some(Target::Url("https://typst.app".into()))
        );
        let Some(Target::Position(page, _)) = jump(&world, &document, 0, first_line(35.0)) else {
            panic!("expected a position");
        };
        assert_eq!(page, 1);
    }

    #[test]
    fn click_on_included_text_jumps_to_its_file() {
        let (world, document) = compile(&format!("{PAGE}#include \"tests/fixtures/included.typ\""));
        let Some(Target::File(path, char)) = jump(&world, &document, 0, first_line(10.5)) else {
            panic!("expected another file");
        };
        assert!(path.ends_with("tests/fixtures/included.typ"), "{path:?}");
        assert_eq!(char, 0);
    }
}
