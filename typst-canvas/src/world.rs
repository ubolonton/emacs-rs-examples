//! The Typst world of a preview: the buffer text is the main file, everything else comes from disk.

use std::{
    error::Error,
    fmt,
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

use typst::{
    Library, LibraryExt, World, WorldExt,
    diag::{FileResult, Severity, SourceDiagnostic, Warned},
    foundations::{Bytes, Datetime, Duration, Smart},
    layout::{Celled, PageElem, Sides},
    model::TableElem,
    syntax::{DiagSpan, FileId, RootedPath, Source, VirtualPath, VirtualRoot},
    text::{Font, FontBook, TextElem},
    utils::LazyHash,
    visualize::{Color, LineElem, Paint, Stroke},
};
use typst_ide::IdeWorld;
use typst_kit::{
    datetime::Time,
    downloader::SystemDownloader,
    files::{FileStore, FsRoot, SystemFiles},
    fonts::{self, FontStore},
    packages::SystemPackages,
};
use typst_layout::PagedDocument;

use crate::offset;

/// User agent for package downloads from Typst Universe.
const USER_AGENT: &str = concat!("typst-canvas/", env!("CARGO_PKG_VERSION"));

/// Fonts are shared by all sessions. The system scan takes a while, so it runs on first use, which
/// is on a compile thread.
static FONTS: LazyLock<FontStore> = LazyLock::new(|| {
    let mut store = FontStore::new();
    store.extend(fonts::system());
    store.extend(fonts::embedded());
    store
});

pub struct PreviewWorld {
    /// The buffer text. It is edited in place, so that Typst can reparse incrementally.
    main: Source,
    library: LazyHash<Library>,
    theme: Option<Theme>,
    /// Other project files and packages, loaded from disk on demand.
    files: FileStore<SystemFiles>,
    time: Time,
}

/// A compile error or warning, located in the main file.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    /// Character range in the main file text (0-based, like a Rust slice).
    pub chars: Range<usize>,
    pub severity: Severity,
    pub message: String,
}

pub struct Compiled {
    /// `None` if there were errors.
    pub document: Option<Document>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A compiled document, with the main file text that it came from. Spans in the document resolve
/// against this text, which can be older than the world's after a failed compile.
#[derive(Debug)]
pub struct Document {
    pub paged: PagedDocument,
    pub source: Source,
}

#[derive(Debug)]
pub struct MainOutsideRoot;

impl fmt::Display for MainOutsideRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the main file is not in the project root")
    }
}

impl Error for MainOutsideRoot {}

/// Default page and text colors, as `0xRRGGBB`. Documents can still set their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub page: u32,
    pub text: u32,
}

impl PreviewWorld {
    /// Create a world whose project root is ROOT, and whose main file is at MAIN.
    ///
    /// MAIN does not need to exist on disk. Its path only names it, and resolves relative imports.
    pub fn new(root: &Path, main: &Path) -> Result<Self, MainOutsideRoot> {
        let vpath = VirtualPath::virtualize(root, main).map_err(|_| MainOutsideRoot)?;
        let id = RootedPath::new(VirtualRoot::Project, vpath).intern();
        let packages = SystemPackages::new(SystemDownloader::new(USER_AGENT));
        let files = SystemFiles::new(FsRoot::new(PathBuf::from(root)), packages);
        Ok(Self {
            main: Source::new(id, String::new()),
            library: LazyHash::new(library(None)),
            theme: None,
            files: FileStore::new(files),
            time: Time::system(),
        })
    }

    /// Replace the main file text, and mark other files as stale, so that the next compile reads
    /// them again.
    pub fn set_main_text(&mut self, text: &str) {
        self.main.replace(text);
        self.files.reset();
        self.time = Time::system();
    }

    /// Return the file system path of the file ID.
    pub fn path(&self, id: FileId) -> Option<PathBuf> {
        self.files.loader().resolve(id).ok()
    }

    /// Set the default colors. Return true if they changed, and so the document must be compiled
    /// again.
    ///
    /// The colors are styles of the standard library, not a `#set` rule in the source, so spans
    /// stay valid.
    pub fn set_theme(&mut self, theme: Option<Theme>) -> bool {
        if self.theme == theme {
            return false;
        }
        self.theme = theme;
        self.library = LazyHash::new(library(theme));
        true
    }

    pub fn compile(&self) -> Compiled {
        let Warned { output, warnings } = typst::compile::<PagedDocument>(self);
        let (document, errors) = match output {
            Ok(paged) => (
                Some(Document {
                    paged,
                    source: self.main.clone(),
                }),
                Default::default(),
            ),
            Err(errors) => (None, errors),
        };
        let diagnostics = errors
            .iter()
            .chain(&warnings)
            .map(|diagnostic| self.locate(diagnostic))
            .collect();
        Compiled {
            document,
            diagnostics,
        }
    }

    /// Convert DIAGNOSTIC into a char range in the main file. A diagnostic in another file goes
    /// to the main-file call site that led to it, or to the start of the main file.
    fn locate(&self, diagnostic: &SourceDiagnostic) -> Diagnostic {
        let main = self.main.id();
        let in_main = |span: DiagSpan| {
            (span.id() == Some(main))
                .then(|| self.range(span))
                .flatten()
        };
        let mut message = diagnostic.message.to_string();
        let bytes = match in_main(diagnostic.span) {
            Some(bytes) => Some(bytes),
            None => {
                // The range goes to the main file, so name the file of the problem.
                if let Some(id) = diagnostic.span.id() {
                    message = format!("{}: {message}", id.vpath().get_without_slash());
                }
                diagnostic
                    .trace
                    .iter()
                    .find_map(|point| in_main(point.span.into()))
            }
        };
        for hint in &diagnostic.hints {
            message.push_str("\nhint: ");
            message.push_str(&hint.v);
        }
        let text = self.main.text();
        let chars = match bytes {
            Some(bytes) => {
                offset::byte_to_char(text, bytes.start)..offset::byte_to_char(text, bytes.end)
            }
            None => 0..0,
        };
        Diagnostic {
            chars,
            severity: diagnostic.severity,
            message,
        }
    }
}

/// Return the standard library, with THEME's default colors if there is one.
fn library(theme: Option<Theme>) -> Library {
    let mut library = Library::default();
    if let Some(Theme { page, text }) = theme {
        let text = Paint::from(color(text));
        library
            .styles
            .set(PageElem::fill, Smart::Custom(Some(color(page).into())));
        library.styles.set(TextElem::fill, text.clone());
        // Strokes default to black, which disappears on a dark page. Give the most common ones the
        // text color.
        let stroke = Stroke {
            paint: Smart::Custom(text),
            ..Stroke::default()
        };
        library.styles.set(LineElem::stroke, stroke.clone());
        library.styles.set(
            TableElem::stroke,
            Celled::Value(Sides::splat(Some(Some(Arc::new(stroke))))),
        );
    }
    library
}

fn color(rgb: u32) -> Color {
    let [_, red, green, blue] = rgb.to_be_bytes();
    Color::from_u8(red, green, blue, u8::MAX)
}

impl World for PreviewWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        FONTS.book()
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            self.files.source(id)
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main.id() {
            Ok(Bytes::from_string(self.main.clone()))
        } else {
            self.files.file(id)
        }
    }

    fn font(&self, index: usize) -> Option<Font> {
        FONTS.font(index)
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        self.time.today(offset)
    }
}

impl IdeWorld for PreviewWorld {
    fn upcast(&self) -> &dyn World {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{TestResult, find, world};

    #[test]
    fn compiles_pages() -> TestResult {
        let compiled = world("A\n#pagebreak()\nB")?.compile();
        assert_eq!(compiled.diagnostics, []);
        assert_eq!(
            compiled
                .document
                .map(|document| document.paged.pages().len()),
            Some(2)
        );
        Ok(())
    }

    #[test]
    fn locates_errors_in_chars() -> TestResult {
        // "é" is 2 bytes in UTF-8, but 1 char in Emacs.
        let compiled = world("é #nope")?.compile();
        assert!(compiled.document.is_none());
        assert_eq!(compiled.diagnostics.len(), 1, "{:?}", compiled.diagnostics);
        let diagnostic = &compiled.diagnostics[0];
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.chars, 3..7);
        assert!(
            diagnostic.message.contains("unknown variable: nope"),
            "{}",
            diagnostic.message
        );
        Ok(())
    }

    #[test]
    fn locates_errors_in_other_files_at_main_call_site() -> TestResult {
        let text = "#import \"tests/fixtures/broken.typ\": f\n#f()";
        let compiled = world(text)?.compile();
        assert_eq!(compiled.diagnostics.len(), 1, "{:?}", compiled.diagnostics);
        let diagnostic = &compiled.diagnostics[0];
        let call = find(text, "f()")?;
        assert_eq!(diagnostic.chars, call..call + "f()".len());
        assert!(
            diagnostic
                .message
                .starts_with("tests/fixtures/broken.typ: unknown variable: nope"),
            "{}",
            diagnostic.message
        );
        Ok(())
    }

    #[test]
    fn theme_sets_default_colors() -> TestResult {
        let mut world = world("#rect(width: 1pt, height: 1pt)")?;
        assert!(world.set_theme(Some(Theme {
            page: 0x10_2030,
            text: 0xF0_E0D0,
        })));
        assert!(!world.set_theme(Some(Theme {
            page: 0x10_2030,
            text: 0xF0_E0D0,
        })));
        let compiled = world.compile();
        let document = compiled
            .document
            .ok_or_else(|| format!("{:?}", compiled.diagnostics))?;
        let page = &document.paged.pages()[0];
        assert_eq!(
            page.fill,
            Smart::Custom(Some(Color::from_u8(0x10, 0x20, 0x30, 0xFF).into()))
        );
        assert!(world.set_theme(None));
        let compiled = world.compile();
        assert_eq!(
            compiled
                .document
                .map(|document| document.paged.pages()[0].fill.clone()),
            Some(Smart::Auto)
        );
        Ok(())
    }

    #[test]
    fn rejects_main_outside_root() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(PreviewWorld::new(root, Path::new("/elsewhere/main.typ")).is_err());
    }
}
