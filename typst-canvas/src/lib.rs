//! Live Typst preview in Emacs 32 canvas images.
//!
//! A [`Session`] compiles and renders on a background thread, into buffers that Rust owns. The
//! defuns below run on the Lisp thread. They copy finished page images into canvases. Only they
//! touch canvas memory, and only inside `with_canvas_data`.

mod offset;
mod render;
mod session;
mod sync;
mod world;

use std::path::Path;

use emacs::{Env, Result, Value, defun};
use typst::diag::Severity;

use crate::{
    render::View,
    session::{Request, Session},
    sync::Target,
    world::{PreviewWorld, Theme},
};

emacs::plugin_is_GPL_compatible!();

#[emacs::module(
    name = "typst-canvas-dyn",
    defun_prefix = "typst-canvas",
    separator = "--"
)]
fn init(_: &Env) -> Result<()> {
    Ok(())
}

/// Start a preview session for the Typst file MAIN, in the project directory ROOT.
/// After each served request, the session writes a newline to the pipe process NOTIFY.
#[defun(user_ptr)]
fn session_start(env: &Env, root: String, main: String, notify: Value<'_>) -> Result<Session> {
    let Ok(world) = PreviewWorld::new(Path::new(&root), Path::new(&main)) else {
        return env.signal("error", (format!("{main} is not in {root}"),));
    };
    let channel = env.open_channel(notify)?;
    Ok(Session::start(world, channel)?)
}

/// Queue a request for SESSION, replacing an unserved one. Return its ID.
///
/// TEXT is the new text of the main file, or nil to only re-render the last good document. WIDTH
/// is the preview window body width in pixels. ZOOM is a factor relative to fit-width. DESK is the
/// color around pages. PAGE and INK are the default page and text colors, or nil for Typst's
/// defaults. Colors are #xRRGGBB.
#[defun]
fn session_request(
    session: &Session,
    text: Option<String>,
    width: u32,
    zoom: f64,
    desk: u32,
    page: Option<u32>,
    ink: Option<u32>,
) -> Result<u64> {
    let theme = page.zip(ink).map(|(page, text)| Theme { page, text });
    Ok(session.request(Request {
        text,
        theme,
        view: View { width, zoom, desk },
    }))
}

/// Stop the thread of SESSION. Wait for it to finish the current request.
#[defun]
fn session_stop(session: &mut Session) -> Result<()> {
    session.stop();
    Ok(())
}

/// Return the newest output of SESSION, as a list
/// (SERVED PAGES ERRORS WARNINGS COMPILE-MS RENDER-MS).
/// SERVED is the ID of the newest served request, or 0.
#[defun]
fn session_status<'e>(env: &'e Env, session: &Session) -> Result<Value<'e>> {
    let output = session.output();
    env.list((
        output.served,
        output.pages.len(),
        output.count(Severity::Error),
        output.count(Severity::Warning),
        output.compile_ms,
        output.render_ms,
    ))
}

/// Return the diagnostics of the newest compile of SESSION, as a list of
/// (BEG END SEVERITY MESSAGE). BEG and END are buffer positions in the text of that compile.
/// SEVERITY is `:error' or `:warning'.
#[defun]
fn session_diagnostics<'e>(env: &'e Env, session: &Session) -> Result<Value<'e>> {
    let output = session.output();
    let error = env.intern(":error")?;
    let warning = env.intern(":warning")?;
    let diagnostics = output
        .diagnostics
        .iter()
        .map(|diagnostic| {
            let severity = match diagnostic.severity {
                Severity::Error => error,
                Severity::Warning => warning,
            };
            env.list((
                diagnostic.chars.start + 1,
                diagnostic.chars.end + 1,
                severity,
                diagnostic.message.as_str(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    env.list(&diagnostics)
}

/// Return where a click at pixel X, Y of the image of page INDEX (0-based) of SESSION leads:
/// - (source POS): position POS in the main file, in the text of the last good compile.
/// - (file PATH POS): position POS in another file.
/// - (url URL).
/// - (position PAGE Y): pixel row Y of the image of page PAGE, e.g. for an internal link.
///
/// Return nil if the click hits no text, shape, image or link.
#[defun]
fn session_jump<'e>(
    env: &'e Env,
    session: &Session,
    index: usize,
    x: i64,
    y: i64,
) -> Result<Option<Value<'e>>> {
    // Aim at the pixel center.
    let (x, y) = (x as f64 + 0.5, y as f64 + 0.5);
    let Some(target) = session.jump(index, x, y) else {
        return Ok(None);
    };
    let value = match target {
        Target::Main(char) => env.list((env.intern("source")?, char + 1))?,
        Target::File(path, char) => env.list((
            env.intern("file")?,
            path.to_string_lossy().as_ref(),
            char + 1,
        ))?,
        Target::Url(url) => env.list((env.intern("url")?, url))?,
        Target::Position(page, point) => {
            let Some(y) = session.pixel_y(page, point) else {
                return Ok(None);
            };
            env.list((env.intern("position")?, page, y))?
        }
    };
    Ok(Some(value))
}

/// Return (SERIAL WIDTH HEIGHT PAGE-X PAGE-Y SCALE) of the image of page INDEX (0-based) of
/// SESSION, or nil. SERIAL changes when the image changes. PAGE-X and PAGE-Y are the pixel
/// position of the page in the image. SCALE is in pixels per typographic point.
#[defun]
fn page_info<'e>(env: &'e Env, session: &Session, index: usize) -> Result<Option<Value<'e>>> {
    let output = session.output();
    output
        .pages
        .get(index)
        .map(|image| {
            env.list((
                image.serial,
                image.width,
                image.height,
                image.page.x,
                image.page.y,
                image.pixel_per_pt,
            ))
        })
        .transpose()
}

/// Copy the image of page INDEX of SESSION into CANVAS, then refresh CANVAS.
/// Return non-nil if CANVAS had the size of the image.
#[defun]
fn present_page(env: &Env, session: &Session, index: usize, canvas: Value<'_>) -> Result<bool> {
    // Release the lock before the copy, so that the thread can publish meanwhile.
    let image = session.output().pages.get(index).cloned();
    let Some(image) = image else {
        return Ok(false);
    };
    let copied = canvas.with_canvas_data(|data| {
        // Lisp resizes a canvas before it presents a page of a new size, but check anyway: a
        // mismatch must skip the copy, not panic.
        if data.width != image.width || data.height != image.height {
            return false;
        }
        data.buffer.copy_from_slice(&image.pixels);
        true
    })?;
    if copied {
        env.call("canvas-refresh", [canvas])?;
    }
    Ok(copied)
}

/// Return the pixel at X, Y of CANVAS as #xAARRGGBB, or nil if X, Y is outside CANVAS.
/// Tests use this to check what `typst-canvas--present-page' copied.
#[defun]
fn canvas_pixel(canvas: Value<'_>, x: usize, y: usize) -> Result<Option<u32>> {
    canvas.with_canvas_data(|data| {
        (x < data.width)
            .then(|| data.buffer.get(y * data.width + x).copied())
            .flatten()
    })
}
