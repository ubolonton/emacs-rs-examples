//! Animates an Emacs 32 canvas image from a Rust background thread.
//!
//! The render thread draws each frame into its own buffer, then swaps it into a shared slot. A
//! Lisp timer calls `canvas-demo--present`, which copies the newest frame into the canvas. Emacs
//! never sees the render thread's buffer, and the render thread never touches the canvas.

use std::{
    f32::consts::TAU,
    mem,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use emacs::{defun, Env, Result, Value};

emacs::plugin_is_GPL_compatible!();

#[emacs::module(
    name = "canvas-demo-dyn",
    defun_prefix = "canvas-demo",
    separator = "--"
)]
fn init(_: &Env) -> Result<()> {
    Ok(())
}

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
    /// True if the frame was rendered after the last copy into the canvas.
    fresh: bool,
}

impl Frame {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height],
            fresh: false,
        }
    }
}

struct Shared {
    /// The newest finished frame. The lock is held only for a swap or a copy, never while
    /// rendering.
    latest: Mutex<Frame>,
    stop: AtomicBool,
}

/// A background render thread. Lisp owns it as a `user-ptr`. When GC frees it, the thread stops.
struct Renderer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Renderer {
    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // A panic in the render thread only ends the animation, so there is nothing to report.
            let _ = thread.join();
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start a thread that renders WIDTH x HEIGHT frames. Return the renderer.
#[defun(user_ptr)]
fn start(width: usize, height: usize) -> Result<Renderer> {
    let shared = Arc::new(Shared {
        latest: Mutex::new(Frame::new(width, height)),
        stop: AtomicBool::new(false),
    });
    let thread = thread::spawn({
        let shared = Arc::clone(&shared);
        move || render_loop(&shared, width, height)
    });
    Ok(Renderer {
        shared,
        thread: Some(thread),
    })
}

/// Stop RENDERER's thread.
#[defun]
fn stop_renderer(renderer: &mut Renderer) -> Result<()> {
    renderer.stop();
    Ok(())
}

/// Copy RENDERER's newest frame into CANVAS, and refresh CANVAS.
/// Return t if there was a new frame of the same size as CANVAS.
#[defun]
fn present(env: &Env, renderer: &Renderer, canvas: Value<'_>) -> Result<bool> {
    let Ok(mut frame) = renderer.shared.latest.lock() else {
        return Ok(false);
    };
    if !frame.fresh {
        return Ok(false);
    }
    let frame_ref = &*frame;
    let copied = canvas.with_canvas_data(|data| {
        // Lisp can resize the canvas at any time. Skip frames that no longer fit.
        if data.width != frame_ref.width || data.height != frame_ref.height {
            return false;
        }
        data.buffer.copy_from_slice(&frame_ref.pixels);
        true
    })?;
    frame.fresh = false;
    drop(frame);
    if copied {
        env.call("canvas-refresh", [canvas])?;
    }
    Ok(copied)
}

fn render_loop(shared: &Shared, width: usize, height: usize) {
    let mut back = Frame::new(width, height);
    let start = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        draw_plasma(&mut back, start.elapsed().as_secs_f32());
        back.fresh = true;
        match shared.latest.lock() {
            Ok(mut latest) => mem::swap(&mut *latest, &mut back),
            Err(_) => return,
        }
        thread::sleep(FRAME_INTERVAL);
    }
}

/// Draw a classic "plasma" effect at time SECONDS.
fn draw_plasma(frame: &mut Frame, seconds: f32) {
    let (width, height) = (frame.width as f32, frame.height as f32);
    for (row_index, row) in frame.pixels.chunks_exact_mut(frame.width).enumerate() {
        let y = row_index as f32 / height;
        for (column_index, pixel) in row.iter_mut().enumerate() {
            let x = column_index as f32 / width;
            let value = (x * 10.0 + seconds).sin()
                + (y * 8.0 - seconds * 1.3).sin()
                + ((x + y) * 6.0 + seconds * 0.7).sin()
                + (((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt() * 12.0 - seconds * 2.0).sin();
            // `value` is in [-4, 4]. Map it to a phase, then to three shifted color channels.
            let phase = value / 4.0 * TAU;
            *pixel = 0xFF00_0000
                | channel(phase) << 16
                | channel(phase + TAU / 3.0) << 8
                | channel(phase + 2.0 * TAU / 3.0);
        }
    }
}

fn channel(phase: f32) -> u32 {
    ((phase.sin() * 0.5 + 0.5) * 255.0) as u32
}
