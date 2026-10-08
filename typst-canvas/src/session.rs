//! A preview session: a background thread that compiles and renders the newest request.
//!
//! Lisp puts requests into a one-element slot. A new request replaces an unserved one, so the
//! thread always works on the newest text. After each request, the thread publishes an [`Output`]
//! and writes one byte to the notify pipe.
//!
//! The Lisp thread never waits for the session thread, which can be in a long compile, e.g. a
//! package download or the first font scan. The thread owns the world. Lisp holds the other locks
//! only for short copies, and stopping does not join the thread.

use std::{
    io::{self, Write},
    mem,
    ops::Range,
    panic::{self, AssertUnwindSafe},
    sync::{Arc, Condvar, Mutex, PoisonError},
    thread,
    time::Instant,
};

use typst::{comemo, diag::Severity, layout::Point, syntax::Source};

use crate::{
    STACK_SIZE, lock,
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

/// A result of the thread. The thread publishes a new one after each request. Lisp takes the newest
/// after a notification.
#[derive(Debug, Default, Clone)]
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
    pub diagnostics: Arc<Vec<Diagnostic>>,
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

/// Where the thread writes after each served request.
type Notify = Box<dyn Write + Send>;

struct Shared {
    slot: Mutex<Slot>,
    wake: Condvar,
    /// The newest output.
    output: Mutex<Arc<Output>>,
    /// `None` after the session stopped. The thread writes while it holds the lock.
    notify: Mutex<Option<Notify>>,
    /// Called on the thread after each compile. Tests use it to keep the thread busy.
    #[cfg(test)]
    after_compile: Mutex<Option<Box<dyn FnMut() + Send>>>,
}

/// Owned by Lisp as a `user-ptr`. Dropping it stops the thread.
pub struct Session {
    shared: Arc<Shared>,
    /// The output that Lisp shows: the newest one at the last `take_output`. Lisp thread only.
    /// Defuns read this, not the newest output, so that the page count, page images, caret and
    /// slide that one notification handler sees agree, also if the thread publishes meanwhile.
    shown: Mutex<Arc<Output>>,
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
    pub fn start(world: PreviewWorld, notify: impl Write + Send + 'static) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            slot: Mutex::default(),
            wake: Condvar::new(),
            output: Mutex::default(),
            notify: Mutex::new(Some(Box::new(notify))),
            #[cfg(test)]
            after_compile: Mutex::default(),
        });
        // Detached: `stop` does not wait for it.
        thread::Builder::new()
            .name("typst-canvas".into())
            .stack_size(STACK_SIZE)
            .spawn({
                let shared = Arc::clone(&shared);
                move || shared.run(world)
            })?;
        Ok(Self {
            shared,
            shown: Mutex::default(),
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

    /// Make the newest output the shown one, and return it. Lisp calls this once per notification.
    ///
    /// Callers get an `Arc`, not a lock guard: they build Lisp values from it, and a Lisp call can
    /// run a GC hook or the debugger, which can call a defun of this session again.
    pub fn take_output(&self) -> Arc<Output> {
        let newest = Arc::clone(&lock(&self.shared.output));
        *lock(&self.shown) = Arc::clone(&newest);
        newest
    }

    /// Return the shown output. See `take_output`.
    pub fn shown(&self) -> Arc<Output> {
        Arc::clone(&lock(&self.shown))
    }

    /// Return where a click at pixel X, Y of the image of page INDEX leads.
    pub fn jump(&self, index: usize, x: f64, y: f64) -> Option<Target> {
        let output = self.shown();
        let (document, image) = (output.document.as_ref()?, output.pages.get(index)?);
        let point = image.point_at(x, y)?;
        sync::jump(document, index, point)
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
        let output = self.shown();
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
        let document = self.shown().document.clone();
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
        Some(self.shown().pages.get(index)?.pixel_at(point).1)
    }

    /// Stop the thread, without waiting for it. It finishes its current request, if any, and
    /// exits.
    ///
    /// After this returns, the thread does not write to the notify pipe anymore. Lisp deletes the
    /// pipe process next, and in batch mode, a write to a deleted pipe kills Emacs (`SIGPIPE`).
    pub fn stop(&self) {
        lock(&self.shared.slot).stop = true;
        self.shared.wake.notify_one();
        // The thread writes while it holds this lock, so no write is in progress after this. A
        // write blocks only while the pipe is full: 64 KiB of notifications that Emacs did not
        // read, when it sent at least as many requests.
        lock(&self.shared.notify).take();
    }
}

impl Drop for Session {
    /// Emacs drops the session in a GC finalizer, so this must not wait for the thread either.
    fn drop(&mut self) {
        self.stop();
    }
}

impl Shared {
    fn run(&self, mut world: PreviewWorld) {
        while let Some((id, request)) = self.take_request() {
            let served = panic::catch_unwind(AssertUnwindSafe(|| {
                self.serve(&mut world, id, request);
            }));
            if served.is_err() {
                let mut output = lock(&self.output);
                *output = Arc::new(Output {
                    served: id,
                    diagnostics: Arc::new(vec![Diagnostic {
                        chars: 0..0,
                        severity: Severity::Error,
                        message: "typst-canvas: The compiler panicked".into(),
                    }]),
                    ..Output::clone(&output)
                });
            }
            // Emacs can delete the pipe process before the session stops. Nobody is listening
            // then.
            if let Some(notify) = lock(&self.notify).as_mut() {
                let _ = notify.write_all(b"\n");
            }
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

    fn serve(&self, world: &mut PreviewWorld, id: u64, request: Request) {
        let start = Instant::now();
        let theme_changed = world.set_theme(request.theme);
        if let Some(text) = &request.text {
            world.set_main_text(text);
        }
        let compiled = (theme_changed || request.text.is_some()).then(|| world.compile());
        #[cfg(test)]
        if let Some(after_compile) = lock(&self.after_compile).as_mut() {
            after_compile();
        }
        let compiled = compiled.map(
            |Compiled {
                 document,
                 diagnostics,
             }| {
                comemo::evict(EVICTION_AGE);
                (document.map(Arc::new), diagnostics, elapsed_ms(start))
            },
        );
        // Only this thread publishes outputs.
        let previous = Arc::clone(&lock(&self.output));
        let document = compiled
            .as_ref()
            .and_then(|(document, ..)| document.clone())
            .or_else(|| previous.document.clone());
        let start = Instant::now();
        let pages = document.as_ref().map_or_else(
            || previous.pages.clone(),
            |document| render::render_pages(&document.paged, request.view, &previous.pages),
        );
        let slide = document
            .as_ref()
            .zip(request.slide)
            .and_then(|(document, view)| {
                render::render_slide(&document.paged, view, previous.slide.as_ref())
            });
        let render_ms = elapsed_ms(start);
        let (diagnostics, compile_ms) = match compiled {
            Some((_, diagnostics, compile_ms)) => (Arc::new(diagnostics), compile_ms),
            None => (Arc::clone(&previous.diagnostics), previous.compile_ms),
        };
        *lock(&self.output) = Arc::new(Output {
            served: id,
            document,
            pages,
            slide,
            diagnostics,
            compile_ms,
            render_ms,
        });
    }
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

    use typst::layout::Abs;

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
        while session.take_output().served < id {
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
            let output = session.take_output();
            assert_eq!(output.pages.len(), 2);
            assert_eq!(*output.diagnostics, []);
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
        let output = session.take_output();
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
        let output = session.take_output();
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
        let output = session.take_output();
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
            let output = session.take_output();
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
        let output = session.take_output();
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
        assert!(session.take_output().slide.is_none());
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
            let output = session.take_output();
            assert_eq!(*output.diagnostics, [], "{text}");
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
    fn shown_output_changes_only_when_taken() -> TestResult {
        let (session, notifications) = start()?;
        let page = "#set page(width: 100pt, height: 50pt)\n";
        let id = session.request(Request {
            text: Some(format!("{page}A")),
            theme: None,
            view: view(200),
            slide: None,
        });
        wait_for(&session, &notifications, id);
        let text = format!("{page}A\n#pagebreak()\nB");
        let id = session.request(Request {
            text: Some(text.clone()),
            theme: None,
            view: view(200),
            slide: None,
        });
        // Wait for the newest output without taking it.
        while lock(&session.shared.output).served < id {
            notifications.recv_timeout(TIMEOUT)?;
        }
        // Until Lisp takes it, everything agrees with the 1-page output that it shows.
        assert_eq!(session.shown().pages.len(), 1);
        let cursor = find(&text, "B")?;
        let (_, place) = session.set_caret(Some(cursor), 0);
        assert!(place.as_ref().is_none_or(|place| place.page == 0), "{place:?}");
        assert_eq!(session.take_output().pages.len(), 2);
        let (_, place) = session.set_caret(Some(cursor), 0);
        assert_eq!(place.map(|place| place.page), Some(1));
        Ok(())
    }

    /// Keep the session thread busy after its next compile, until the returned sender is dropped.
    /// The receiver gets a message when the thread is busy.
    fn block_after_compile(session: &Session) -> (Sender<()>, Receiver<()>) {
        let (release, released) = mpsc::channel::<()>();
        let (busy, busy_receiver) = mpsc::channel();
        *lock(&session.shared.after_compile) = Some(Box::new(move || {
            let _ = busy.send(());
            let _ = released.recv();
        }));
        (release, busy_receiver)
    }

    #[test]
    fn busy_thread_blocks_neither_jump_nor_stop() -> TestResult {
        let (session, notifications) = start()?;
        let text = "#set page(width: 100pt, height: 100pt, margin: 10pt)\nHello";
        let id = session.request(Request {
            text: Some(text.into()),
            theme: None,
            view: view(200),
            slide: None,
        });
        wait_for(&session, &notifications, id);
        let (release, busy) = block_after_compile(&session);
        session.request(Request {
            text: Some(format!("{text} world")),
            theme: None,
            view: view(200),
            slide: None,
        });
        busy.recv_timeout(TIMEOUT)?;

        let image = session.take_output().pages[0].clone();
        let (x, y) = image.pixel_at(Point::new(Abs::pt(10.5), Abs::pt(15.0)));
        assert_eq!(
            session.jump(0, x, y),
            Some(Target::Main(find(text, "Hello")?))
        );

        // Drop the session on another thread, so that a wait fails the test instead of hanging it.
        let shared = Arc::downgrade(&session.shared);
        let (dropped, dropped_receiver) = mpsc::channel();
        thread::spawn(move || {
            drop(session);
            let _ = dropped.send(());
        });
        dropped_receiver
            .recv_timeout(TIMEOUT)
            .map_err(|_| "dropping the session waits for the thread")?;
        // The writer is gone, but the thread still runs.
        while notifications.try_recv().is_ok() {}
        assert_eq!(
            notifications.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        );
        assert!(shared.upgrade().is_some());
        // Released, it exits.
        drop(release);
        let deadline = Instant::now() + TIMEOUT;
        while shared.upgrade().is_some() {
            assert!(Instant::now() < deadline, "the thread does not exit");
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}
