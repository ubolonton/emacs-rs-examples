//! Sync between the source and the pages: from a click on a page to the source.

use std::{num::NonZeroUsize, path::PathBuf};

use typst::{
    Library, World,
    diag::FileResult,
    foundations::{Bytes, Datetime, Duration},
    introspection::PagedPosition,
    layout::Point,
    syntax::{FileId, Source},
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
