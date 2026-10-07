//! A preview session: a background thread that compiles and renders the newest request.
//!
//! Lisp puts requests into a one-element slot. A new request replaces an unserved one, so the
//! thread always works on the newest text, and the Lisp thread never waits for a compile. After
//! each request, the thread publishes an [`Output`] and writes one byte to the notify pipe.

use std::{
    io::{self, Write},
    panic::{self, AssertUnwindSafe},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
    time::Instant,
};

use typst::diag::Severity;
use typst_layout::PagedDocument;

use crate::{
    render::{self, PageImage, View},
    world::{Compiled, Diagnostic, PreviewWorld},
};

/// Memoized results unused for this many compiles are evicted. Same as `typst watch`.
const EVICTION_AGE: usize = 10;
/// Layout recursion is not stack-safe, so give the thread as much stack as a main thread.
const STACK_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Request {
    /// New text of the main file. `None` re-renders the last good document, e.g. after a zoom.
    pub text: Option<String>,
    pub view: View,
}

/// The newest result of the thread. Lisp reads it after a notification.
#[derive(Debug, Default)]
pub struct Output {
    /// ID of the newest request that this output reflects. 0 before the first one.
    pub served: u64,
    /// The last document that compiled without errors. Errors keep the previous one.
    pub document: Option<Arc<PagedDocument>>,
    /// Images of the pages of `document`.
    pub pages: Vec<Arc<PageImage>>,
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
        })
    }

    /// Queue REQUEST, replacing an unserved one. Return its ID.
    pub fn request(&self, request: Request) -> u64 {
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
        let compiled = request.text.map(|text| {
            let start = Instant::now();
            let Compiled {
                document,
                diagnostics,
            } = {
                let mut world = lock(&self.world);
                world.set_main_text(&text);
                world.compile()
            };
            typst::comemo::evict(EVICTION_AGE);
            (document.map(Arc::new), diagnostics, elapsed_ms(start))
        });
        let new_document = compiled
            .as_ref()
            .and_then(|(document, ..)| document.clone());

        // Only this thread writes the output, so it does not change while we render without the
        // lock.
        let (document, previous) = {
            let output = lock(&self.output);
            (
                new_document.or_else(|| output.document.clone()),
                output.pages.clone(),
            )
        };
        let start = Instant::now();
        let pages = document
            .as_ref()
            .map(|document| render::render_pages(document, request.view, &previous));
        let render_ms = elapsed_ms(start);

        let mut output = lock(&self.output);
        output.served = id;
        output.render_ms = render_ms;
        output.document = document;
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

    fn start() -> (Session, Receiver<u8>) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(world) = PreviewWorld::new(root, &root.join("main.typ")) else {
            panic!("main.typ is inside its root");
        };
        let (sender, receiver) = mpsc::channel();
        let Ok(session) = Session::start(world, ChannelWriter(sender)) else {
            panic!("cannot spawn the session thread");
        };
        (session, receiver)
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
    fn renders_pages_and_keeps_last_good_document() {
        let (session, notifications) = start();
        let text = Some("#set page(width: 100pt, height: 50pt)\nA\n#pagebreak()\nB".into());
        let id = session.request(Request {
            text,
            view: view(200),
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
            view: view(200),
        });
        wait_for(&session, &notifications, id);
        let output = session.output();
        assert_eq!(output.count(Severity::Error), 1);
        assert_eq!(output.pages.len(), 2);
        assert!(
            Arc::ptr_eq(&output.pages[1], &first_pages[1]),
            "page re-rendered"
        );
    }

    #[test]
    fn view_requests_keep_pending_text() {
        let (session, notifications) = start();
        let text = Some("#set page(width: 100pt, height: 50pt)\nA".into());
        session.request(Request {
            text,
            view: view(200),
        });
        let id = session.request(Request {
            text: None,
            view: view(300),
        });
        wait_for(&session, &notifications, id);
        let output = session.output();
        assert_eq!(output.pages.len(), 1);
        assert_eq!(output.pages[0].width, 300);
    }

    #[test]
    fn stop_joins_thread() {
        let (mut session, notifications) = start();
        session.request(Request {
            text: Some("A".into()),
            view: view(200),
        });
        session.stop();
        assert!(session.thread.is_none());
        // The thread dropped the writer.
        while notifications.recv_timeout(TIMEOUT).is_ok() {}
        assert!(matches!(
            notifications.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }
}
