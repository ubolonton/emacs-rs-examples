//! The Typst world of a preview: the buffer text is the main file, everything else comes from disk.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use typst::{
    Library, LibraryExt, World, WorldExt,
    diag::{FileResult, Severity, SourceDiagnostic, Warned},
    foundations::{Bytes, Datetime, Duration},
    syntax::{DiagSpan, FileId, RootedPath, Source, VirtualPath, VirtualRoot},
    text::{Font, FontBook},
    utils::LazyHash,
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
    pub document: Option<PagedDocument>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug)]
pub struct MainOutsideRoot;

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
            library: LazyHash::new(Library::default()),
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

    pub fn compile(&self) -> Compiled {
        let Warned { output, warnings } = typst::compile::<PagedDocument>(self);
        let (document, errors) = match output {
            Ok(document) => (Some(document), Default::default()),
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

    fn world(text: &str) -> PreviewWorld {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(mut world) = PreviewWorld::new(root, &root.join("main.typ")) else {
            panic!("main.typ is inside its root");
        };
        world.set_main_text(text);
        world
    }

    #[test]
    fn compiles_pages() {
        let compiled = world("A\n#pagebreak()\nB").compile();
        assert_eq!(compiled.diagnostics, []);
        assert_eq!(
            compiled.document.map(|document| document.pages().len()),
            Some(2)
        );
    }

    #[test]
    fn locates_errors_in_chars() {
        // "é" is 2 bytes in UTF-8, but 1 char in Emacs.
        let compiled = world("é #nope").compile();
        assert!(compiled.document.is_none());
        let [diagnostic] = compiled.diagnostics.as_slice() else {
            panic!("expected 1 diagnostic, got {:?}", compiled.diagnostics);
        };
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.chars, 3..7);
        assert!(
            diagnostic.message.contains("unknown variable: nope"),
            "{}",
            diagnostic.message
        );
    }

    #[test]
    fn locates_errors_in_other_files_at_main_call_site() {
        let text = "#import \"tests/fixtures/broken.typ\": f\n#f()";
        let compiled = world(text).compile();
        let [diagnostic] = compiled.diagnostics.as_slice() else {
            panic!("expected 1 diagnostic, got {:?}", compiled.diagnostics);
        };
        let call = text.find("f()").unwrap_or_default();
        assert_eq!(diagnostic.chars, call..call + "f()".len());
        assert!(
            diagnostic
                .message
                .starts_with("tests/fixtures/broken.typ: unknown variable: nope"),
            "{}",
            diagnostic.message
        );
    }

    #[test]
    fn rejects_main_outside_root() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(PreviewWorld::new(root, Path::new("/elsewhere/main.typ")).is_err());
    }
}
