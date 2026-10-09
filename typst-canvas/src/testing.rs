//! Helpers for unit tests.

use std::{error::Error, path::Path};

use crate::world::{Document, PreviewWorld};

/// Tests return errors instead of panicking. `?` turns a missing value into a failure with a
/// message, e.g. `.ok_or("no caret")?`.
pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Return a world whose main file is `main.typ` in the crate directory, with the text TEXT.
pub fn world(text: &str) -> TestResult<PreviewWorld> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut world = PreviewWorld::new(root, &root.join("main.typ"))?;
    world.set_main_text(text);
    Ok(world)
}

/// Compile TEXT as the main file. Fail if it has errors.
pub fn compile(text: &str) -> TestResult<Document> {
    let compiled = world(text)?.compile();
    Ok(compiled
        .document
        .ok_or_else(|| format!("{:?}", compiled.diagnostics.list))?)
}

/// Return the byte offset of the first NEEDLE in TEXT.
pub fn find(text: &str, needle: &str) -> TestResult<usize> {
    Ok(text
        .find(needle)
        .ok_or_else(|| format!("{needle:?} is not in {text:?}"))?)
}
