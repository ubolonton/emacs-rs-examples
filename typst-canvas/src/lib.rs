//! Live Typst preview in Emacs 32 canvas images.
//!
//! A [`Session`] compiles and renders on a background thread, into buffers that Rust owns. The
//! defuns below run on the Lisp thread. They copy finished page images into canvases. Only they
//! touch canvas memory, and only inside `with_canvas_data`.

mod math;
mod offset;
mod render;
mod session;
mod sync;
mod world;

use std::path::Path;

use emacs::{Env, Result, Value, Vector, defun};
use typst::diag::Severity;

use crate::{
    math::EquationView,
    render::{SlideView, View},
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
/// defaults. Colors are #xRRGGBB. SLIDE is nil, or [PAGE WIDTH HEIGHT] to also render page PAGE
/// (0-based) as large as fits WIDTH x HEIGHT pixels, for a presentation.
#[expect(clippy::too_many_arguments)]
#[defun]
fn session_request(
    session: &Session,
    text: Option<String>,
    width: u32,
    zoom: f64,
    desk: u32,
    page: Option<u32>,
    ink: Option<u32>,
    slide: Option<Vector<'_>>,
) -> Result<u64> {
    let theme = page.zip(ink).map(|(page, text)| Theme { page, text });
    let slide = slide
        .map(|slide| -> Result<SlideView> {
            Ok(SlideView {
                page: slide.get(0)?,
                width: slide.get(1)?,
                height: slide.get(2)?,
            })
        })
        .transpose()?;
    Ok(session.request(Request {
        text,
        theme,
        view: View { width, zoom, desk },
        slide,
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

/// Move the caret of SESSION to char offset CURSOR (0-based) of the main file, or hide it if
/// CURSOR is nil or not in laid-out text. COLOR is the caret color, as #xRRGGBB. The offset is
/// in the text of the last good compile.
///
/// Return (OLD NEW TOP BOTTOM). OLD and NEW are the pages of the previous and the new caret, or
/// nil. Present them again to show the change. TOP and BOTTOM are the pixel rows of the new
/// caret's line in the image of page NEW.
#[defun]
fn session_set_caret<'e>(
    env: &'e Env,
    session: &Session,
    cursor: Option<usize>,
    color: u32,
) -> Result<Value<'e>> {
    let (old, new) = session.set_caret(cursor, color);
    let (page, top, bottom) = match new {
        Some(place) => (
            Some(place.page),
            Some(place.rows.start),
            Some(place.rows.end),
        ),
        None => (None, None, None),
    };
    env.list((old, page, top, bottom))
}

/// Copy the image of page INDEX of SESSION into CANVAS, with the caret if it is on that page.
/// Then refresh CANVAS. Return non-nil if CANVAS had the size of the image.
#[defun]
fn present_page(env: &Env, session: &Session, index: usize, canvas: Value<'_>) -> Result<bool> {
    // Release the lock before the copy, so that the thread can publish meanwhile.
    let image = session.output().pages.get(index).cloned();
    let Some(image) = image else {
        return Ok(false);
    };
    // The caret is drawn onto the copy, so the cached image stays clean.
    let caret = session.caret_on(index);
    let copied = canvas.with_canvas_data(|data| {
        // Lisp resizes a canvas before it presents a page of a new size, but check anyway: a
        // mismatch must skip the copy, not panic.
        if data.width != image.width || data.height != image.height {
            return false;
        }
        data.buffer.copy_from_slice(&image.pixels);
        if let Some((caret, color)) = caret {
            render::draw_caret(data.buffer, &image, &caret, color);
        }
        true
    })?;
    if copied {
        env.call("canvas-refresh", [canvas])?;
    }
    Ok(copied)
}

/// Return (SERIAL WIDTH HEIGHT) of the slide of SESSION, or nil if the newest request did not ask
/// for one. SERIAL changes when the image changes.
#[defun]
fn slide_info<'e>(env: &'e Env, session: &Session) -> Result<Option<Value<'e>>> {
    let slide = session.output().slide.clone();
    slide
        .map(|image| env.list((image.serial, image.width, image.height)))
        .transpose()
}

/// Copy the slide of SESSION into CANVAS, and refresh CANVAS. Return non-nil if CANVAS had the
/// size of the slide.
#[defun]
fn present_slide(env: &Env, session: &Session, canvas: Value<'_>) -> Result<bool> {
    let slide = session.output().slide.clone();
    let Some(image) = slide else {
        return Ok(false);
    };
    copy_into(env, canvas, image.width, image.height, &image.pixels)
}

/// Return the equation of SESSION that encloses char offset CURSOR (0-based) of the newest text
/// sent, as (END SERIAL WIDTH HEIGHT STALE), or nil if there is none, or it has no image yet.
///
/// END is the buffer position right after the equation. The image shows the equation with
/// PX-PER-EM pixels per em of its font, at most MAX-WIDTH pixels wide, with the caret of the last
/// `typst-canvas--session-set-caret'. STALE is non-nil if the newest text did not compile: the
/// image is from the last good compile, and `typst-canvas--present-equation' dims it.
#[defun]
fn session_equation<'e>(
    env: &'e Env,
    session: &Session,
    cursor: usize,
    px_per_em: f64,
    max_width: usize,
) -> Result<Option<Value<'e>>> {
    let view = EquationView {
        px_per_em,
        max_width,
    };
    session
        .equation(cursor, view)
        .map(|place| {
            env.list((
                place.end + 1,
                place.image.serial,
                place.image.width,
                place.image.height,
                place.stale,
            ))
        })
        .transpose()
}

/// Copy the equation of the last `typst-canvas--session-equation' call into CANVAS, dimmed if it
/// is stale, and refresh CANVAS. Return non-nil if CANVAS had the size of the image.
#[defun]
fn present_equation(env: &Env, session: &Session, canvas: Value<'_>) -> Result<bool> {
    let Some((image, stale)) = session.equation_image() else {
        return Ok(false);
    };
    if stale {
        copy_into(env, canvas, image.width, image.height, &image.dimmed())
    } else {
        copy_into(env, canvas, image.width, image.height, &image.pixels)
    }
}

/// Copy PIXELS, WIDTH x HEIGHT, into CANVAS, and refresh it. Return false, and copy nothing, if
/// CANVAS has another size.
fn copy_into(
    env: &Env,
    canvas: Value<'_>,
    width: usize,
    height: usize,
    pixels: &[u32],
) -> Result<bool> {
    let copied = canvas.with_canvas_data(|data| {
        if data.width != width || data.height != height {
            return false;
        }
        data.buffer.copy_from_slice(pixels);
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
