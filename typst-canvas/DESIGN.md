# typst-canvas: Design

Live Typst preview inside Emacs 32. A Rust module compiles the buffer text with the `typst` crate on
a background thread, rasterizes pages with `typst-render`, and copies the pixels into Emacs canvas
images (`Value::with_canvas_data`). No external process, no PDF viewer, no browser.

Target: `emacs-32-gtk` (GUI build; canvases work in `-batch` too). Typst crates: 0.15.x.

The dev profile uses `opt-level = 2` for all crates: Typst and the per-pixel loops in `render.rs`
are too slow for live use without optimizations.

## Features

| Area | Behavior |
|------|----------|
| Live | Each edit sends the buffer text to the compile thread. Latest text wins; stale requests are dropped. The UI never blocks on a compile. |
| Notify | The thread writes to a pipe process (`Env::open_channel`) when a new result is ready. The pipe filter refreshes the preview. No polling timer. |
| Pages | One canvas per page, stacked in a preview buffer. Only pages whose frame hash changed are re-rendered and re-copied. |
| Zoom | Default: fit page width to the preview window body. `+`/`-`/`0` zoom; window resize re-renders. Over 100%, pages wider than the window scroll horizontally: `truncate-lines`, `auto-hscroll-mode` off (point is always at the start of a page line, so automatic hscroll would undo each scroll), `<left>`/`<right>`, `C-x <`/`>`, or shift + wheel. Emacs draws images cut at the left edge, and `posn-object-x-y` includes the hscroll, so clicks still map right. Zoom ≤ 100% resets hscroll. Caret scrolling is vertical only. |
| Errors | Diagnostics go to Flymake in the source buffer. The preview keeps the last good render and shows the error count in its header line. Positions from the last good render (caret, click jumps) and diagnostics are mapped to the newest text, so typing above them does not shift them. |
| Backward sync | `mouse-1` on a page (hand pointer), or `RET` (the middle row of the visible part of the page at point, at several columns until one hits): `typst_ide::jump_from_click` → select the source window, go to the char, pulse the word (or the line). Other files open with `find-file-other-window`. Links open with `browse-url`; internal links scroll the preview. |
| Forward sync | `typst-canvas-follow-cursor` (default on). Point movement in the source (`post-command-hook`, debounced 0.1 s), and each new result: find the caret (page, point, font size) → draw a bar in the `cursor` face color and a translucent line band onto the canvas at copy time, so cached page images stay clean. Only the old and new caret pages are copied again. If the caret's line is not visible, scroll so that it is 1/3 from the top (`window-start` + pixel vscroll). No caret when point is not in laid-out text (e.g. in code). |
| Theme | `typst-canvas-match-theme` (default on): page fill = `default` face background, text fill = foreground, set via `Library` styles (no source rewriting, so spans stay valid). Line and table strokes get the text color too; other default strokes stay black. Toggle with `t` (buffer-local in the preview). `enable-theme-functions`/`disable-theme-functions` re-apply it while a session exists. |
| Look | Each canvas = page + margin in the desk color + 1-pixel border + soft drop shadow, drawn in Rust. Border and shadow get stronger on a dark desk (white-ish border, more opaque shadow). The canvas is at least as wide as the window, with the page centered. Desk color: `default` face background with its HSL lightness shifted 8% (darker if light, lighter if dark), so that pages stand out also when they match the theme. Hex colors are parsed without a frame (`color-values-from-color-spec`), because a text terminal frame rounds them. |
| Equation at point | `typst-canvas-inline-math` (default on). While point is between the delimiters of an equation (`$...$`, inline or display), an overlay after the end of the equation's last line shows it rendered: an `after-string` of `"\n"` (with `cursor`, so that the cursor stays at the end of the line) and a canvas. Font size: the `default` face's pixel size × `typst-canvas-inline-math-scale` (1.25: math fonts have smaller lowercase letters than code fonts), at most the window width. It shows the caret too. It updates with the caret (debounced point motion, each new result) and disappears when point leaves math. While the newest text does not compile, the last image of the same equation shows dimmed. |
| Presentation | `typst-canvas-present`: one page at a time, as large as fits the window, centered on black, in a new fullscreen frame (`typst-canvas-present-frame`; else the selected window, alone, with the window configuration restored on quit). No mode line, header line or cursor; black `default` and `fringe` faces. Keys: `SPC`/`n`/right next, `DEL`/`p`/left previous, digits + `RET` go to, `<home>`/`<end>`, `q` quit. Starts at the page of the caret, else the top page of the preview. Live: the slide comes with each result of the source's session. Slides render only while a window shows the presentation. Closing its frame (also through the window manager) ends it. |
| Stats | Preview header line: status (`success` "ok", `shadow` "compiling", `error` "N errors" + `warning` "stale" when the pages are from the last good compile, `warning` "N warnings"), "p 2/5" (the caret page, else the top page of the window), compile ms and render ms (`shadow`), zoom. |

## Rust (`src/`)

- `lib.rs`: `#[module(name = "typst-canvas-dyn", defun_prefix = "typst-canvas", separator = "--")]`. Thin defuns only.
- `world.rs`: `PreviewWorld` implements `typst::World` and `typst_ide::IdeWorld`.
  - Main file text comes from the buffer (`Source::replace` for incremental reparse). Other files: read from disk under the root.
  - Other files: `typst_kit::files::FileStore<SystemFiles>`, reset before each compile so disk changes show.
  - Fonts: `typst-kit` font search (system + embedded), loaded once per process (`LazyLock`), on first use (a compile thread).
  - Packages: `typst_kit::packages::SystemPackages` with `SystemDownloader` (Typst Universe download into the cache dir).
  - `compile` returns a `Document`: the `PagedDocument`, the main `Source` it came from, the `Source` and path of each other file that the compile read (`World::source` records them), and the library. `Document` is itself a `World` and an `IdeWorld` over these sources. So click jumps resolve spans without the preview world: it has newer text after a failed compile, and the compile thread owns it.
  - Diagnostics: byte ranges in the main file, with the main `Source` of the compile (`Diagnostics`). A diagnostic in another file goes to the innermost main-file call site in its trace, with the file name in the message.
- `session.rs`: `Session`, owned by Lisp as a `user-ptr`. The Lisp thread never waits for the session thread, which can be in a long compile (a package download, the first font scan).
  - The thread owns the world. `Arc<Shared>` holds the request slot, the newest output, and the notify writer.
  - Stop (also `Drop`, which runs in a GC finalizer): set the stop flag, and drop the notify writer. No join: the thread finishes its current request, then exits. It writes the notification while it holds the writer's lock, so no write follows `stop`. Lisp deletes the pipe process next, and in batch mode, a write to a deleted pipe kills Emacs (`SIGPIPE`).
  - Request slot: `Mutex<Slot>` + `Condvar`. A request carries the new text (optional), the theme colors (optional), the view (fit width px, zoom, desk color), and the slide view (page, window width and height px; while a presentation shows). A theme change compiles again, also without new text. A new request replaces an unserved one, but keeps its text if it has none. Each request gets an ID.
  - Output: the thread publishes a new `Arc<Output>` after each request: the ID of the newest served request (Lisp compares it with the newest sent ID to show "compiling"), the last good document (`Arc`), rendered pages (`Arc<PageImage>`), the slide (`Arc<PageImage>`, if the request asked for one), diagnostics, compile ms, render ms.
  - Shown output: `session-status` (once per notification) takes the newest output as the shown one. The other defuns read the shown one, so all that one notification handler reads agrees (page count, page images, caret, slide), also if the thread publishes meanwhile. Defuns hold an `Arc`, never a lock guard, while they call Lisp: a GC hook or the debugger can call the session again.
  - Lisp-thread state: the caret, the newest text sent as a parsed `Source` (`Source::replace` reparses incrementally), and the equation image with its stale flag.
  - Positions: the shown document can be from older text than the newest text sent. `offset::TextMap` maps caret offsets from the newest text to the document's, and jump targets and diagnostics the other way.
  - Notify: after each served request, the thread writes `"\n"` to the pipe. Write errors are ignored.
  - A panic in Typst becomes an error diagnostic. The thread continues. The world stays marked as changed, so the next request compiles, also without a change of its own.
- `render.rs`: `typst_render::render` → premultiplied RGBA → `0xAARRGGBB` composited onto the page, plus margin, shadow, caret.
  - Each `PageImage` has a key (page hash via `typst::utils::hash128(&Page)`, scale, window width, desk color) and a serial. A re-render reuses images with the same key, also if their page moved. Lisp copies a page only if its serial changed.
  - Scale: the widest page fits the window width minus margins, times the zoom. A pixel budget (16 M pixels) caps the whole image of each page: page, margin, and padding to the window width (`budget_scale`). Pages without width get a fallback scale. Window sizes are clamped to 16384 px.
  - `rasterize` refuses an image over the budget: `typst_render::render` limits only the width of its pixmap. A page without width at an infinite scale gave a 1 × 4294967295 pixmap (17 GB).
  - Pages without a reusable image render in parallel (`thread::scope`, up to one thread per core), on threads with the 8 MiB stack of the session thread. A reflow re-renders all later pages.
  - `render_slide`: one page, scaled to fit the slide view in both directions, centered on black, without border or shadow. Its key also has the view height (0 for preview pages), so page turns back and forth and unchanged edits reuse it. A view over 36 M pixels (an 8K screen) is scaled down.
- `math.rs`: the equation at the cursor.
  - `equation_at`: walk up from the leaf at the cursor to the innermost `SyntaxKind::Equation` whose delimiters enclose it.
  - `cut_out`: in layout order, keep the frame items between the `Tag::Start` of the `EquationElem` with that node's span and the `Tag::End` with its location, in a copy of the page frame that keeps only those items and the groups (transform, clip) that hold them. Bounds: `TextItem::bbox` (Y flipped), `Shape::bbox`, image size, through the group transforms. Tags, not spans: content from a `#let` binding has spans outside the equation.
  - `render`: a hard frame of the bounds plus 0.3 em padding, with the pruned page frame pushed at minus its origin, rendered by `typst_render::render` with the page's fill. Scale: buffer pixels per em / the largest font size in the equation, capped by the width limit and the pixel budget. Rendering again, not cropping the page image: the page image's scale follows the preview width and zoom.
  - `Session::equation`: find the equation in the newest text. If the last good document has exactly that text, cut out and render (with the caret, if on that page), else mark the last image stale if it is of an equation at the same byte offset, else drop it. `EquationImage::dimmed` mixes the background (top left pixel) over it.
- `offset.rs`: UTF-8 byte ↔ Emacs char offset conversion. `TextMap`: from the common prefix and suffix of two texts, offsets in the prefix stay, offsets in the suffix move by the length difference, and offsets in the changed middle are clamped to the changed middle of the other text. At an insertion point, an offset goes after the inserted text.
- `sync.rs`: click jumps (`jump_from_click` with the `Document` as world; a target in the main file is mapped to the newest text), and the caret. `typst_ide::jump_from_cursor` returns only the start of the text node, without a font size, so `sync::caret` does its node lookup and frame walk, but stops at the glyph of the cursor (`Glyph::span.1` is the glyph's byte offset in its node).
  - The caret lives in `Session` (Lisp thread only), in points. `present_page` draws it with the current image's scale.
  - Math shapes letters as styled chars ("x" as "𝑥", `pi` as "π"), whose UTF-8 can be longer than the source, so glyph ranges are clamped to the node. If no glyph contains the cursor, the caret goes after the node's last glyph that starts before it (node end, collapsed spaces, inside an identifier). Text, math text and math identifiers get a caret.

Invariant: only defuns (Lisp thread) touch canvas memory, and only inside `with_canvas_data`. The
thread touches only Rust-owned buffers. Canvas size mismatch → skip copy, never panic.

## Lisp (`typst-canvas.el`)

- `typst-canvas-mode`: minor mode for a `.typ` buffer. Starts a session, a pipe process, the preview window, the Flymake backend, `after-change-functions` and `post-command-hook` handlers.
  - Changes schedule a zero-delay timer that sends the whole text once per command. The text is the whole buffer, also when it is narrowed. Raw bytes (invalid UTF-8, unibyte buffers) go out as U+FFFD, one char each, so positions stay; a message says so once per buffer. The module takes only Unicode strings.
  - Teardown stops the session before it deletes the pipe process: in batch mode, Emacs does not ignore `SIGPIPE`.
  - Flymake: the backend stores the newest report function, and reports when diagnostics change, while `flymake-mode` is on. A report function that signals is dropped. It has `flymake-always-safe`, because the user started the compile with the mode, not Flymake.
  - Handlers that Emacs calls outside commands (pipe filter, timers, hooks) run in `typst-canvas--with-guard`: an error shows once in the echo area. Emacs pauses after an error in a process filter, and removes some hook functions that signal.
  - `change-major-mode-hook` turns the mode off: a new major mode kills the local state, but not the session, process, preview and timers.
  - A clone (`clone-buffer`, `clone-indirect-buffer`) copies all local variables. `clone-buffer-hook` and `clone-indirect-buffer-hook` make it forget them (`typst-canvas--source-variables`), so it starts with the mode off. An indirect clone shares the text, so its changes go to the session of the original.
  - Narrowing: diagnostics are clamped to the whole buffer, and a click jump outside the region widens.
- `typst-canvas-preview-mode`: `special-mode` for `*typst-canvas: NAME*`. Keys: `+ - 0 t g q`, `n`/`p` pages, `<left>`/`<right>` hscroll, `mouse-1`/`RET` jump.
  - `window-size-change-functions` re-renders when the window body width changes.
- Each page is one line: an image char with a `typst-canvas-page` text property (0-based index), and a newline. Page lines are added or removed at the end.
- Canvas specs get an uninterned `:id`: Emacs finds canvases by `eq` spec, but its image cache matches specs by `equal`. Resize: `plist-put` of `:data-width`/`:data-height` on the same spec.
- Equation at point: `typst-canvas--update-equation` runs at the end of `typst-canvas--update-caret`, so with each result and after point motion. It does nothing while a request is pending: that result calls it again. `typst-canvas--update-caret` does nothing while the text timer is pending: the session maps positions to the newest text sent, which does not have the newest changes yet. `post-command-hook` schedules the update when point moved, also with `typst-canvas-follow-cursor` off.
- `typst-canvas-present-mode`: `special-mode` for `*typst-canvas presentation: NAME*`, with one canvas. The source buffer's requests add `[PAGE WIDTH HEIGHT]` while a window shows it; its notify handler copies a new slide serial. `window-size-change-functions` asks for a new slide on resize, and the buffer-local `window-buffer-change-functions` when a window starts or stops showing it. Killing the buffer (`q`) drops the slide from requests, and deletes the frame or restores the windows. `delete-frame-functions` (while a presentation exists) kills the presentations of a deleted frame, e.g. one that the window manager closed.
  - The presentation frame has black 8 px fringes on both sides: without a right fringe, Emacs keeps the last text column for the end of the line, and cuts the slide. The left one keeps the slide centered.
  - A new fullscreen frame is 640x612 at first; the resize handler re-renders when fullscreen takes effect.
- Flymake reports pass the whole buffer as `:region`, so each report replaces the last one. Without it, reports after the first in one Flymake check only add diagnostics, and a fixed error stays marked until the next check (after idle time).

## Demo (`typst-canvas-demo.el`)

`typst-canvas-demo` (autoloaded from `typst-canvas.el`) opens `examples/showcase.typ` in a buffer that does not visit the file, turns on the mode, and runs a script of steps from timers: `type` (one char per step, jittered delay, longer after words and sentences), `snippet`, `goto`, `pause`, theme switch and restore, zoom.

- Typed text stays valid Typst while it grows, so most keys update the preview: `(`, `[` and `*` are typed with their closer (like `electric-pair-mode`), display math starts as the snippet `$  $`, and the table row goes after the last table argument, where no comma is missing.
- Timers do not run `post-command-hook`, so each step calls `typst-canvas--on-post-command` to move the caret.
- Any command stops it (`pre-command-hook`), and so does killing its buffer. Stopping restores the themes from before the switch, and keeps the buffer and the mode.
- `typst-canvas-demo-speed` scales all delays; the tests run the whole script at speed 1000.

## Testing

- `cargo test`: Rust unit tests (offsets and `TextMap`, pixel conversion, image size bounds, compile, diagnostics, jumps, session thread). Size bounds are checked as sizes, without rendering: a broken bound could allocate gigabytes. A test hook in the session thread (`after_compile`) keeps it busy, or injects a panic.
- `bin/test.sh`: ERT in batch mode (`EMACS=emacs-32-gtk`).
- `bin/latency.sh`: edit-to-screen latency under Xvfb (`bin/latency.el`), on `examples/showcase.typ`. Prints median, min and max of the total, compile, render, Lisp and redisplay times.
- `bin/screenshot.sh`: GUI under Xvfb (`bin/screenshot.el`), saves `target/screenshot-{1..9}.png`: light theme with the caret, a compile error (stale pages), `modus-vivendi` loaded at run time, the pulse right after a click jump, zoom + hscroll, the equation at point, the same equation dimmed by an error, `examples/slides.typ` presented (slide 1), slide 4 after an edit in the source.
- `bin/screencast.sh`: records `typst-canvas-demo` under Xvfb (1600x900, `modus-vivendi`). `bin/screencast.el` starts ffmpeg (`x11grab`, lossless) when the first pages show and stops it after the demo, so the video spans the demo only. The script then encodes `target/screencast.mp4` and `target/screencast.gif` (12 fps, 1000 px wide, one palette from the changing pixels). `bin/screencast.sh inline-math` hides the preview window (`display-buffer-no-window`) and types an equation, then moves through math and out (1280x720, `target/inline-math.{mp4,gif}`, 960 px wide).

## Phases

1. Done. Core: scaffold, world, session thread, pipe notify, multi-page canvases, fit/zoom, diagnostics (header line + Flymake), tests, scripts.
2. Done. Sync and polish: backward/forward sync, theme, page decoration, zoom hscroll, header line. (Phase 1 already reuses page images by key.)
3. Done. Showcase: demo document, self-typing demo command, screencast (MP4 + GIF), README. Also a latency benchmark, and the optimizations it showed (`opt-level` 2, parallel page rendering).
   - 3b. Done. Equation at point (cut out of the laid-out page), presentation mode, `examples/slides.typ`.
4. Done. Review: fixes for blocking on the compile thread, consistent outputs, positions after errors, image size bounds, handler errors, raw bytes, major-mode changes, clones, hidden presentations, narrowing.

## Known limitations

- `stop` does not wait for the thread. After a stop, the thread can run on, and hold its world and outputs, until its current compile ends (e.g. a package download).
- A notification write blocks while the pipe is full (64 KiB that Emacs did not read), and `stop` waits for it. This needs as many requests without a read.
- While the newest text does not compile, positions in its changed part map to the edges of that part in the last good text.
- A document resolves spans only in the files that its compile read as sources. Spans of other files (none in practice) do not jump.
- Changes in an indirect buffer that is not a clone (`make-indirect-buffer` without CLONE) do not reach the session until the base buffer changes.
- Pages too large for the pixel budget at `MIN_PIXEL_PER_PT` get a smaller scale. Slides in a window over 36 M pixels are smaller than the window.

## Typst 0.15 API notes

- `typst_ide::jump_from_click(world: &dyn IdeWorld, document: &PagedDocument, position: &PagedPosition) -> Option<Jump>`.
  `PagedPosition { page: NonZeroUsize /* 1-based */, point: Point /* pt, from the page's top left */ }`.
  `Jump::File(FileId, usize /* byte offset */) | Jump::Url(Url) | Jump::Position(PagedPosition)`.
  Pixel → point: `(x - page.x) / pixel_per_pt`. `PageImage` stores `page` (its `Rect` in the image) and `pixel_per_pt`.
- `typst_ide::jump_from_cursor(document: &PagedDocument, source: &Source, cursor: usize /* byte */) -> Vec<PagedPosition>`.
  Only `Text`/`MathText` leaves match. The point is the start of the first glyph with that span, on the text baseline (the glyph box is `y - size .. y`).
- Both resolve spans against sources. After a failed compile, the world's main source is newer than the last good document's spans, so jumps use the document, with its own sources, as world.
- Theme: `library.styles.set(PageElem::fill, Smart::Custom(Some(color.into())))` and `library.styles.set(TextElem::fill, color.into())`, with `Color::from_u8(r, g, b, 255)`. Set them on `Library::default()`, then replace `PreviewWorld::library` (`LazyHash::new`). See `typst-ide/src/tests.rs`.
- Frames hash cheaply: `Frame` items are an `Arc<LazyHash<..>>`, so `hash128(&page)` reuses cached item hashes.
