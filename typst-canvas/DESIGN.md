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
| Errors | Diagnostics go to Flymake in the source buffer. The preview keeps the last good render and shows the error count in its header line. |
| Backward sync | `mouse-1` on a page (hand pointer), or `RET` (the middle row of the visible part of the page at point, at several columns until one hits): `typst_ide::jump_from_click` → select the source window, go to the char, pulse the word (or the line). Other files open with `find-file-other-window`. Links open with `browse-url`; internal links scroll the preview. |
| Forward sync | `typst-canvas-follow-cursor` (default on). Point movement in the source (`post-command-hook`, debounced 0.1 s), and each new result: find the caret (page, point, font size) → draw a bar in the `cursor` face color and a translucent line band onto the canvas at copy time, so cached page images stay clean. Only the old and new caret pages are copied again. If the caret's line is not visible, scroll so that it is 1/3 from the top (`window-start` + pixel vscroll). No caret when point is not in laid-out text (e.g. in code). |
| Theme | `typst-canvas-match-theme` (default on): page fill = `default` face background, text fill = foreground, set via `Library` styles (no source rewriting, so spans stay valid). Line and table strokes get the text color too; other default strokes stay black. Toggle with `t` (buffer-local in the preview). `enable-theme-functions`/`disable-theme-functions` re-apply it while a session exists. |
| Look | Each canvas = page + margin in the desk color + 1-pixel border + soft drop shadow, drawn in Rust. Border and shadow get stronger on a dark desk (white-ish border, more opaque shadow). The canvas is at least as wide as the window, with the page centered. Desk color: `default` face background with its HSL lightness shifted 8% (darker if light, lighter if dark), so that pages stand out also when they match the theme. Hex colors are parsed without a frame (`color-values-from-color-spec`), because a text terminal frame rounds them. |
| Stats | Preview header line: status (`success` "ok", `shadow` "compiling", `error` "N errors" + `warning` "stale" when the pages are from the last good compile, `warning` "N warnings"), "p 2/5" (the caret page, else the top page of the window), compile ms and render ms (`shadow`), zoom. |

## Rust (`src/`)

- `lib.rs`: `#[module(name = "typst-canvas-dyn", defun_prefix = "typst-canvas", separator = "--")]`. Thin defuns only.
- `world.rs`: `PreviewWorld` implements `typst::World` and `typst_ide::IdeWorld`.
  - Main file text comes from the buffer (`Source::replace` for incremental reparse). Other files: read from disk under the root.
  - Other files: `typst_kit::files::FileStore<SystemFiles>`, reset before each compile so disk changes show.
  - Fonts: `typst-kit` font search (system + embedded), loaded once per process (`LazyLock`), on first use (a compile thread).
  - Packages: `typst_kit::packages::SystemPackages` with `SystemDownloader` (Typst Universe download into the cache dir).
  - `compile` returns a `Document`: the `PagedDocument` and the main `Source` it came from. After a failed compile, the world has newer text than the last good document, so jumps resolve spans against the document's `Source`.
  - Diagnostics: converted to char ranges in the main file. A diagnostic in another file goes to the innermost main-file call site in its trace, with the file name in the message.
- `session.rs`: `Session`, owned by Lisp as a `user-ptr`. Drop stops and joins the thread.
  - All shared state is in one `Arc<Shared>`: request slot, world, output.
  - Request slot: `Mutex<Slot>` + `Condvar`. A request carries the new text (optional), the theme colors (optional), and the view (fit width px, zoom, desk color). A theme change compiles again, also without new text. A new request replaces an unserved one, but keeps its text if it has none. Each request gets an ID.
  - World: `Mutex<PreviewWorld>`; the thread locks it for update + compile. A click jump locks it on the Lisp thread (for other files), so it waits for a running compile.
  - Output: `Mutex<Output>` with the ID of the newest served request (Lisp compares it with the newest sent ID to show "compiling"), the last good document (`Arc`), rendered pages (`Arc<PageImage>`), diagnostics, compile ms, render ms.
  - Notify: after each served request, the thread writes `"\n"` to the pipe. Write errors are ignored.
  - A panic in Typst becomes an error diagnostic. The thread continues.
- `render.rs`: `typst_render::render` → premultiplied RGBA → `0xAARRGGBB` composited onto the page, plus margin, shadow, caret.
  - Each `PageImage` has a key (page hash via `typst::utils::hash128(&Page)`, scale, window width, desk color) and a serial. A re-render reuses images with the same key, also if their page moved. Lisp copies a page only if its serial changed.
  - Scale: the widest page fits the window width minus margins, times the zoom. A pixel budget per page caps it.
  - Pages without a reusable image render in parallel (`thread::scope`, up to one thread per core). A reflow re-renders all later pages.
- `offset.rs`: UTF-8 byte ↔ Emacs char offset conversion.
- `sync.rs`: click jumps (`jump_from_click` with a `Snapshot` world whose main file is the document's `Source`), and the caret. `typst_ide::jump_from_cursor` returns only the start of the text node, without a font size, so `sync::caret` does its node lookup and frame walk, but stops at the glyph of the cursor (`Glyph::span.1` is the glyph's byte offset in its node).
  - The caret lives in `Session` (Lisp thread only), in points. `present_page` draws it with the current image's scale.

Invariant: only defuns (Lisp thread) touch canvas memory, and only inside `with_canvas_data`. The
thread touches only Rust-owned buffers. Canvas size mismatch → skip copy, never panic.

## Lisp (`typst-canvas.el`)

- `typst-canvas-mode`: minor mode for a `.typ` buffer. Starts a session, a pipe process, the preview window, the Flymake backend, `after-change-functions` and `post-command-hook` handlers.
  - Changes schedule a zero-delay timer that sends the whole text once per command.
  - Teardown stops the session before it deletes the pipe process: in batch mode, Emacs does not ignore `SIGPIPE`.
  - Flymake: the backend stores the newest report function, and reports when diagnostics change. It has `flymake-always-safe`, because the user started the compile with the mode, not Flymake.
- `typst-canvas-preview-mode`: `special-mode` for `*typst-canvas: NAME*`. Keys: `+ - 0 t g q`, `n`/`p` pages, `<left>`/`<right>` hscroll, `mouse-1`/`RET` jump.
  - `window-size-change-functions` re-renders when the window body width changes.
- Each page is one line: an image char with a `typst-canvas-page` text property (0-based index), and a newline. Page lines are added or removed at the end.
- Canvas specs get an uninterned `:id`: Emacs finds canvases by `eq` spec, but its image cache matches specs by `equal`. Resize: `plist-put` of `:data-width`/`:data-height` on the same spec.
- Flymake reports pass the whole buffer as `:region`, so each report replaces the last one. Without it, reports after the first in one Flymake check only add diagnostics, and a fixed error stays marked until the next check (after idle time).

## Demo (`typst-canvas-demo.el`)

`typst-canvas-demo` (autoloaded from `typst-canvas.el`) opens `examples/showcase.typ` in a buffer that does not visit the file, turns on the mode, and runs a script of steps from timers: `type` (one char per step, jittered delay, longer after words and sentences), `snippet`, `goto`, `pause`, theme switch and restore, zoom.

- Typed text stays valid Typst while it grows, so most keys update the preview: `(`, `[` and `*` are typed with their closer (like `electric-pair-mode`), display math starts as the snippet `$  $`, and the table row goes after the last table argument, where no comma is missing.
- Timers do not run `post-command-hook`, so each step calls `typst-canvas--on-post-command` to move the caret.
- Any command stops it (`pre-command-hook`), and so does killing its buffer. Stopping restores the themes from before the switch, and keeps the buffer and the mode.
- `typst-canvas-demo-speed` scales all delays; the tests run the whole script at speed 1000.

## Testing

- `cargo test`: Rust unit tests (offsets, pixel conversion, compile, diagnostics, session thread).
- `bin/test.sh`: ERT in batch mode (`EMACS=emacs-32-gtk`).
- `bin/latency.sh`: edit-to-screen latency under Xvfb (`bin/latency.el`), on `examples/showcase.typ`. Prints median, min and max of the total, compile, render, Lisp and redisplay times.
- `bin/screenshot.sh`: GUI under Xvfb (`bin/screenshot.el`), saves `target/screenshot-{1..5}.png`: light theme with the caret, a compile error (stale pages), `modus-vivendi` loaded at run time, the pulse right after a click jump, zoom + hscroll.
- `bin/screencast.sh`: records `typst-canvas-demo` under Xvfb (1600x900, `modus-vivendi`). `bin/screencast.el` starts ffmpeg (`x11grab`, lossless) when the first pages show and stops it after the demo, so the video spans the demo only. The script then encodes `target/screencast.mp4` and `target/screencast.gif` (12 fps, 1000 px wide, one palette from the changing pixels).

## Phases

1. Done. Core: scaffold, world, session thread, pipe notify, multi-page canvases, fit/zoom, diagnostics (header line + Flymake), tests, scripts.
2. Done. Sync and polish: backward/forward sync, theme, page decoration, zoom hscroll, header line. (Phase 1 already reuses page images by key.)
3. Done. Showcase: demo document, self-typing demo command, screencast (MP4 + GIF), README. Also a latency benchmark, and the optimizations it showed (`opt-level` 2, parallel page rendering).
4. Review: code review pass, fixes, docs.

## Typst 0.15 API notes

- `typst_ide::jump_from_click(world: &dyn IdeWorld, document: &PagedDocument, position: &PagedPosition) -> Option<Jump>`.
  `PagedPosition { page: NonZeroUsize /* 1-based */, point: Point /* pt, from the page's top left */ }`.
  `Jump::File(FileId, usize /* byte offset */) | Jump::Url(Url) | Jump::Position(PagedPosition)`.
  Pixel → point: `(x - page.x) / pixel_per_pt`. `PageImage` stores `page` (its `Rect` in the image) and `pixel_per_pt`.
- `typst_ide::jump_from_cursor(document: &PagedDocument, source: &Source, cursor: usize /* byte */) -> Vec<PagedPosition>`.
  Only `Text`/`MathText` leaves match. The point is the start of the first glyph with that span, on the text baseline (the glyph box is `y - size .. y`).
- Both resolve spans against sources. After a failed compile, the world's main source is newer than the last good document's spans, so jumps use the document's own `Source`.
- Theme: `library.styles.set(PageElem::fill, Smart::Custom(Some(color.into())))` and `library.styles.set(TextElem::fill, color.into())`, with `Color::from_u8(r, g, b, 255)`. Set them on `Library::default()`, then replace `PreviewWorld::library` (`LazyHash::new`). See `typst-ide/src/tests.rs`.
- Frames hash cheaply: `Frame` items are an `Arc<LazyHash<..>>`, so `hash128(&page)` reuses cached item hashes.
