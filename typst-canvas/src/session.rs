//! A preview session: a background thread that compiles and renders the newest request.
//!
//! Lisp puts requests into a one-element slot. A new request replaces an unserved one, so the
//! thread always works on the newest text, and the Lisp thread never waits for a compile. After
//! each request, the thread publishes an [`Output`] and writes one byte to the notify pipe.

use std::{
    io::{self, Write},
    mem,
    ops::Range,
    panic::{self, AssertUnwindSafe},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
    time::Instant,
};

use typst::{comemo, diag::Severity, layout::Point, syntax::Source};

use crate::{
    STACK_SIZE,
    math::{self, EquationImage, EquationView},
    offset,
    render::{self, PageImage, SlideView, View},
    sync::{self, Caret, Target},
    world::{Compiled, Diagnostic, Document, PreviewWorld, Theme},
};

/// Memoized results unused for this many compiles are evicted. Same as `typst watch`.
const EVICTION_AGE: usize = 10;

#[derive(Debug, Clone)]
pub struct Request {
    /// New text of the main file. `None` re-renders the last good document, e.g. after a zoom.
    pub text: Option<String>,
    /// Default colors. A change compiles the document again, also without new text.
    pub theme: Option<Theme>,
    pub view: View,
    /// The page to render as a slide, while a presentation shows.
    pub slide: Option<SlideView>,
}

/// The newest result of the thread. Lisp reads it after a notification.
#[derive(Debug, Default)]
pub struct Output {
    /// ID of the newest request that this output reflects. 0 before the first one.
    pub served: u64,
    /// The last document that compiled without errors. Errors keep the previous one.
    pub document: Option<Arc<Document>>,
    /// Images of the pages of `document`.
    pub pages: Vec<Arc<PageImage>>,
    /// The slide of the newest request that asked for one.
    pub slide: Option<Arc<PageImage>>,
    /// Diagnostics of the newest compile, also if it failed.
    pub diagnostics: Vec<Diagnostic>,
    pub compile_ms: f64,
    pub render_ms: f64,
}

impl Output {
    pub fn count(&self, severity: Severity) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == severity)
            .count()
    }
}

#[derive(Default)]
struct Slot {
    /// The newest request that the thread did not take yet, with its ID.
    pending: Option<(u64, Request)>,
    last_id: u64,
    stop: bool,
}

struct Shared {
    slot: Mutex<Slot>,
    wake: Condvar,
    /// Locked by the thread while it compiles.
    world: Mutex<PreviewWorld>,
    output: Mutex<Output>,
}

/// Owned by Lisp as a `user-ptr`. Dropping it stops the thread.
pub struct Session {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// The caret and its color (`0xRRGGBB`). Only the Lisp thread uses it, to draw onto canvases.
    caret: Mutex<Option<(Caret, u32)>>,
    /// The newest text sent, parsed. Only the Lisp thread uses it, to find the equation at the
    /// cursor in the text that the buffer has now, also while that text does not compile.
    text: Mutex<Source>,
    /// The equation at the cursor, and whether it is stale (from an older text). Lisp thread only.
    equation: Mutex<Option<(Arc<EquationImage>, bool)>>,
}

/// The equation at the cursor, for Lisp.
pub struct EquationPlace {
    /// Char offset of the end of the equation, in the newest text.
    pub end: usize,
    pub image: Arc<EquationImage>,
    /// True if the image is from an older text, because the newest one did not compile.
    pub stale: bool,
}

/// Where the caret is on its page image.
#[derive(Debug, Clone, PartialEq)]
pub struct CaretPlace {
    pub page: usize,
    /// Pixel rows of the caret's line band.
    pub rows: Range<usize>,
}

impl Session {
    /// Start a thread that compiles in WORLD, and writes to NOTIFY after each served request.
    pub fn start(world: PreviewWorld, mut notify: impl Write + Send + 'static) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            slot: Mutex::default(),
            wake: Condvar::new(),
            world: Mutex::new(world),
            output: Mutex::default(),
        });
        let thread = thread::Builder::new()
            .name("typst-canvas".into())
            .stack_size(STACK_SIZE)
            .spawn({
                let shared = Arc::clone(&shared);
                move || shared.run(&mut notify)
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
            caret: Mutex::default(),
            text: Mutex::new(Source::detached(String::new())),
            equation: Mutex::default(),
        })
    }

    /// Queue REQUEST, replacing an unserved one. Return its ID.
    pub fn request(&self, request: Request) -> u64 {
        if let Some(text) = &request.text {
            // An incremental reparse: usually cheaper than the copy of the text.
            lock(&self.text).replace(text);
        }
        let mut slot = lock(&self.shared.slot);
        slot.last_id += 1;
        let id = slot.last_id;
        // A view-only request must not drop the text of the request it replaces.
        let replaced_text = slot.pending.take().and_then(|(_, replaced)| replaced.text);
        let text = request.text.or(replaced_text);
        slot.pending = Some((id, Request { text, ..request }));
        self.shared.wake.notify_one();
        id
    }

    pub fn output(&self) -> MutexGuard<'_, Output> {
        lock(&self.shared.output)
    }

    /// Return where a click at pixel X, Y of the image of page INDEX leads.
    ///
    /// Waits for the current compile, because it needs the world.
    pub fn jump(&self, index: usize, x: f64, y: f64) -> Option<Target> {
        let (document, image) = {
            let output = self.output();
            (output.document.clone()?, output.pages.get(index).cloned()?)
        };
        let point = image.point_at(x, y)?;
        sync::jump(&lock(&self.shared.world), &document, index, point)
    }

    /// Move the caret to char offset CURSOR of the main file, or hide it if CURSOR is `None` or not
    /// in laid-out text. COLOR is the bar color, as `0xRRGGBB`. Return the page of the previous
    /// caret, and the place of the new one.
    ///
    /// The offset is in the text of the last good compile.
    pub fn set_caret(
        &self,
        cursor: Option<usize>,
        color: u32,
    ) -> (Option<usize>, Option<CaretPlace>) {
        let output = self.output();
        let caret = cursor
            .zip(output.document.as_ref())
            .and_then(|(cursor, document)| {
                let byte = offset::char_to_byte(document.source.text(), cursor);
                sync::caret(document, byte)
            });
        let place = caret.and_then(|caret| {
            let image = output.pages.get(caret.page)?;
            Some(CaretPlace {
                page: caret.page,
                rows: render::caret_band(image, &caret),
            })
        });
        let previous = mem::replace(&mut *lock(&self.caret), caret.map(|caret| (caret, color)));
        (previous.map(|(caret, _)| caret.page), place)
    }

    /// Return the caret and its color, if it is on page INDEX.
    pub fn caret_on(&self, index: usize) -> Option<(Caret, u32)> {
        lock(&self.caret).filter(|(caret, _)| caret.page == index)
    }

    /// Return the equation that encloses char offset CURSOR of the newest text, rendered for VIEW,
    /// or `None` if there is none, or it has no image yet.
    ///
    /// If the last good document has the newest text, render the equation from it, with the caret.
    /// Else keep the image of the last call, as stale, if it shows an equation at the same offset.
    pub fn equation(&self, cursor: usize, view: EquationView) -> Option<EquationPlace> {
        let text = lock(&self.text);
        let byte = offset::char_to_byte(text.text(), cursor);
        let Some((start, end)) =
            math::equation_at(&text, byte).map(|node| (node.offset(), node.offset() + node.len()))
        else {
            *lock(&self.equation) = None;
            return None;
        };
        let end = offset::byte_to_char(text.text(), end);
        let document = self.output().document.clone();
        let fresh = document.filter(|document| document.source.text() == text.text());
        drop(text);

        let mut equation = lock(&self.equation);
        match fresh {
            Some(document) => {
                let image = math::equation_at(&document.source, byte)
                    .and_then(|node| math::cut_out(&document.paged, node.span()))
                    .and_then(|cutout| {
                        let page = &document.paged.pages()[cutout.page];
                        math::render(page, &cutout, view, start, self.caret_on(cutout.page))
                    });
                *equation = image.map(|image| (Arc::new(image), false));
            }
            None => match equation.as_mut() {
                Some((image, stale)) if image.start == start => *stale = true,
                _ => *equation = None,
            },
        }
        equation.as_ref().map(|(image, stale)| EquationPlace {
            end,
            image: Arc::clone(image),
            stale: *stale,
        })
    }

    /// Return the image of the last `equation` call, and whether it is stale.
    pub fn equation_image(&self) -> Option<(Arc<EquationImage>, bool)> {
        lock(&self.equation).clone()
    }

    /// Return the pixel row of the page point POINT in the image of page INDEX.
    pub fn pixel_y(&self, index: usize, point: Point) -> Option<f64> {
        let output = self.output();
        Some(output.pages.get(index)?.pixel_at(point).1)
    }

    /// Stop the thread. Wait for it to finish the current request.
    pub fn stop(&mut self) {
        lock(&self.shared.slot).stop = true;
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.take() {
            // The thread catches panics from Typst, so there is nothing to report.
            let _ = thread.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Shared {
    fn run(&self, notify: &mut impl Write) {
        while let Some((id, request)) = self.take_request() {
            if panic::catch_unwind(AssertUnwindSafe(|| self.serve(id, request))).is_err() {
                let mut output = lock(&self.output);
                output.served = id;
                output.diagnostics = vec![Diagnostic {
                    chars: 0..0,
                    severity: Severity::Error,
                    message: "typst-canvas: The compiler panicked".into(),
                }];
            }
            // Emacs can delete the pipe process before the session stops. Nobody is listening
            // then.
            let _ = notify.write_all(b"\n");
        }
    }

    /// Wait for a request. Return `None` if the session stopped.
    fn take_request(&self) -> Option<(u64, Request)> {
        let mut slot = lock(&self.slot);
        loop {
            if slot.stop {
                return None;
            }
            if let Some(pending) = slot.pending.take() {
                return Some(pending);
            }
            slot = self.wake.wait(slot).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn serve(&self, id: u64, request: Request) {
        let start = Instant::now();
        let compiled = {
            let mut world = lock(&self.world);
            let theme_changed = world.set_theme(request.theme);
            if let Some(text) = &request.text {
                world.set_main_text(text);
            }
            (theme_changed || request.text.is_some()).then(|| world.compile())
        };
        let compiled = compiled.map(
            |Compiled {
                 document,
                 diagnostics,
             }| {
                comemo::evict(EVICTION_AGE);
                (document.map(Arc::new), diagnostics, elapsed_ms(start))
            },
        );
        let new_document = compiled
            .as_ref()
            .and_then(|(document, ..)| document.clone());

        // Only this thread writes the output, so it does not change while we render without the
        // lock.
        let (document, previous, previous_slide) = {
            let output = lock(&self.output);
            (
                new_document.or_else(|| output.document.clone()),
                output.pages.clone(),
                output.slide.clone(),
            )
        };
        let start = Instant::now();
        let pages = document
            .as_ref()
            .map(|document| render::render_pages(&document.paged, request.view, &previous));
        let slide = document
            .as_ref()
            .zip(request.slide)
            .and_then(|(document, view)| {
                render::render_slide(&document.paged, view, previous_slide.as_ref())
            });
        let render_ms = elapsed_ms(start);

        let mut output = lock(&self.output);
        output.served = id;
        output.render_ms = render_ms;
        output.document = document;
        output.slide = slide;
        if let Some(pages) = pages {
            output.pages = pages;
        }
        if let Some((_, diagnostics, compile_ms)) = compiled {
            output.diagnostics = diagnostics;
            output.compile_ms = compile_ms;
        }
    }
}

/// Lock MUTEX. A panic while it was locked does not leave its data in a state that we cannot
/// use, because all updates are single assignments.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::mpsc::{self, Receiver, Sender},
        time::Duration,
    };

    use super::*;
    use crate::testing::{TestResult, find};

    const TIMEOUT: Duration = Duration::from_secs(60);

    struct ChannelWriter(Sender<u8>);

    impl Write for ChannelWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            for byte in bytes {
                let _ = self.0.send(*byte);
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn start() -> TestResult<(Session, Receiver<u8>)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let world = PreviewWorld::new(root, &root.join("main.typ"))?;
        let (sender, receiver) = mpsc::channel();
        let session = Session::start(world, ChannelWriter(sender))?;
        Ok((session, receiver))
    }

    /// Wait for notifications until SESSION served request ID.
    fn wait_for(session: &Session, notifications: &Receiver<u8>, id: u64) {
        while session.output().served < id {
            assert!(
                notifications.recv_timeout(TIMEOUT).is_ok(),
                "no notification"
            );
        }
    }

    fn view(width: u32) -> View {
        View {
            width,
            zoom: 1.0,
            desk: 0x00_8080,
        }
    }

    #[test]
    fn renders_pages_and_keeps_last_good_document() -> TestResult {
        let (session, notifications) = start()?;
        let text = Some("#set page(width: 100pt, height: 50pt)\nA\n#pagebreak()\nB".into());
        let id = session.request(Request {
            text,
            theme: None,
            view: view(200),
            slide: None,
        });
        wait_for(&session, &notifications, id);
        let first_pages = {
            let output = session.output();
            assert_eq!(output.pages.len(), 2);
            assert_eq!(output.diagnostics, []);
            // 100pt fit into 200 - 2 * MARGIN px.
            assert_eq!(output.pages[0].width, 200);
            assert_eq!(output.pages[0].height, 84 + 2 * render::MARGIN as usize);
            output.pages.clone()
        };

        let id = session.request(Request {
            text: Some("#nope".into()),
            theme: None,
            view: view(200),
            slide: None,
        });
        wait_for(&session, &notifications, id);
        let output = session.output();
        assert_eq!(output.count(Severity::Error), 1);
        assert_eq!(output.pages.len(), 2);
        assert!(
            Arc::ptr_eq(&output.pages[1], &first_pages[1]),
            "page re-rendered"
        );
        Ok(())
    }

    #[test]
    fn view_requests_keep_pending_text() -> TestResult {
        let (session, notifications) = start()?;
        let text = Some("#set page(width: 100pt, height: 50pt)\nA".into());
        session.request(Request {
            text,
            theme: None,
            view: view(200),
            slide: None,
        });
        let id = session.request(Request {
            text: None,
            theme: None,
            view: view(300),
            slide: None,
        });
        wait_for(&session, &notifications, id);
        let output = session.output();
        assert_eq!(output.pages.len(), 1);
        assert_eq!(output.pages[0].width, 300);
        Ok(())
    }

    #[test]
    fn theme_change_compiles_again() -> TestResult {
        let (session, notifications) = start()?;
        let text = Some("#set page(width: 100pt, height: 50pt)".into());
        let id = session.request(Request {
            text,
            theme: None,
            view: view(200),
            slide: None,
        });
        wait_for(&session, &notifications, id);
        let id = session.request(Request {
            text: None,
            theme: Some(Theme {
                page: 0x00_0000,
                text: 0xFF_FFFF,
            }),
            view: view(200),
            slide: None,
        });
        wait_for(&session, &notifications, id);
        let output = session.output();
        let image = &output.pages[0];
        // The middle of the page has the theme's page color.
        assert_eq!(
            image.pixels[image.height / 2 * image.width + image.width / 2],
            0xFF00_0000
        );
        Ok(())
    }

    #[test]
    fn slide_fits_page_into_view() -> TestResult {
        let (session, notifications) = start()?;
        let text = Some("#set page(width: 160pt, height: 90pt)\nA\n#pagebreak()\nB".into());
        let slide = |page| SlideView {
            page,
            width: 400,
            height: 300,
        };
        let id = session.request(Request {
            text,
            theme: None,
            view: view(200),
            slide: Some(slide(1)),
        });
        wait_for(&session, &notifications, id);
        let first = {
            let output = session.output();
            let image = output.slide.clone().ok_or("no slide")?;
            // The image fills the view. The page fits its width, centered vertically on black.
            assert_eq!((image.width, image.height), (400, 300));
            assert_eq!((image.page.width, image.page.height), (400, 225));
            assert_eq!(image.pixels[0], 0xFF00_0000);
            image
        };
        // The same slide again is not rendered again. A page past the end shows the last one.
        let id = session.request(Request {
            text: None,
            theme: None,
            view: view(200),
            slide: Some(slide(5)),
        });
        wait_for(&session, &notifications, id);
        let output = session.output();
        assert!(
            output
                .slide
                .as_ref()
                .is_some_and(|image| Arc::ptr_eq(image, &first))
        );
        drop(output);
        let id = session.request(Request {
            text: None,
            theme: None,
            view: view(200),
            slide: None,
        });
        wait_for(&session, &notifications, id);
        assert!(session.output().slide.is_none());
        Ok(())
    }

    #[test]
    fn renders_pages_without_area() -> TestResult {
        let (session, notifications) = start()?;
        for text in [
            "#set page(width: 0pt)\nA",
            "#set page(height: 0pt)\nA",
            "#set page(width: 0pt, height: 0pt)",
        ] {
            let id = session.request(Request {
                text: Some(text.into()),
                theme: None,
                view: view(200),
                slide: Some(SlideView {
                    page: 0,
                    width: 400,
                    height: 300,
                }),
            });
            wait_for(&session, &notifications, id);
            let output = session.output();
            assert_eq!(output.diagnostics, [], "{text}");
            assert_eq!(output.pages.len(), 1, "{text}");
            assert!(output.slide.is_some(), "{text}");
        }
        Ok(())
    }

    #[test]
    fn equation_goes_stale_while_text_fails() -> TestResult {
        let (session, notifications) = start()?;
        let equation_view = EquationView {
            px_per_em: 16.0,
            max_width: 500,
        };
        let serve = |text: &str| {
            let id = session.request(Request {
                text: Some(text.into()),
                theme: None,
                view: view(200),
                slide: None,
            });
            wait_for(&session, &notifications, id);
        };
        let good = "#set page(width: 100pt, height: 50pt)\nA $x + y$ B";
        serve(good);
        let cursor = find(good, "x")?;
        let fresh = session
            .equation(cursor, equation_view)
            .ok_or("no equation")?;
        assert!(!fresh.stale);
        assert_eq!(fresh.end, find(good, " B")?);
        // Outside the equation, there is none.
        assert!(session.equation(good.len(), equation_view).is_none());
        session.equation(cursor, equation_view);
        // An error inside the equation keeps the last image, as stale.
        serve("#set page(width: 100pt, height: 50pt)\nA $x + #nope$ B");
        let stale = session
            .equation(cursor, equation_view)
            .ok_or("no stale equation")?;
        assert!(stale.stale);
        assert_eq!(stale.end, fresh.end + "#nope".len() - "y".len());
        Ok(())
    }

    #[test]
    fn stop_joins_thread() -> TestResult {
        let (mut session, notifications) = start()?;
        session.request(Request {
            text: Some("A".into()),
            theme: None,
            view: view(200),
            slide: None,
        });
        session.stop();
        assert!(session.thread.is_none());
        // The thread dropped the writer.
        while notifications.recv_timeout(TIMEOUT).is_ok() {}
        assert!(matches!(
            notifications.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        Ok(())
    }
}
