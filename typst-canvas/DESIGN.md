# typst-canvas: Design

Live Typst preview inside Emacs 32. A Rust module compiles the buffer text with the `typst` crate on
a background thread, rasterizes pages with `typst-render`, and copies the pixels into Emacs canvas
images (`Value::with_canvas_data`). No external process, no PDF viewer, no browser.

Target: `emacs-32-gtk` (GUI build; canvases work in `-batch` too). Typst crates: 0.15.x.

## Features

| Area | Behavior |
|------|----------|
| Live | Each edit sends the buffer text to the compile thread. Latest text wins; stale requests are dropped. The UI never blocks on a compile. |
| Notify | The thread writes to a pipe process (`Env::open_channel`) when a new result is ready. The pipe filter refreshes the preview. No polling timer. |
| Pages | One canvas per page, stacked in a preview buffer. Only pages whose frame hash changed are re-rendered and re-copied. |
| Zoom | Default: fit page width to the preview window body. `+`/`-`/`0` zoom; window resize re-renders. |
| Errors | Diagnostics go to Flymake in the source buffer. The preview keeps the last good render and shows the error count in its header line. |
| Backward sync | `mouse-1` on a page: `typst_ide::jump_from_click` → move point in the source window and pulse the region. Links open with `browse-url`; internal links scroll the preview. |
| Forward sync | Point movement in the source (debounced): `typst_ide::jump_from_cursor` → draw a caret + line highlight in the page pixels, scroll the preview to keep it visible. |
| Theme | `typst-canvas-match-theme` (default on): page fill = `default` face background, text fill = foreground, set via `Library` styles (no source rewriting, so spans stay valid). Line and table strokes get the text color too; other default strokes stay black. Toggle with `t` (buffer-local in the preview). `enable-theme-functions`/`disable-theme-functions` re-apply it while a session exists. |
| Look | Each canvas = page + margin in the desk color + 1-pixel border + soft drop shadow, drawn in Rust. Border and shadow get stronger on a dark desk (white-ish border, more opaque shadow). The canvas is at least as wide as the window, with the page centered. Desk color: `default` face background with its HSL lightness shifted 8% (darker if light, lighter if dark), so that pages stand out also when they match the theme. Hex colors are parsed without a frame (`color-values-from-color-spec`), because a text terminal frame rounds them. |
| Stats | Preview header line: status, page count, compile ms, render ms, zoom. |

## Rust (`src/`)

- `lib.rs`: `#[module(name = "typst-canvas-dyn", defun_prefix = "typst-canvas", separator = "--")]`. Thin defuns only.
- `world.rs`: `PreviewWorld` implements `typst::World` and `typst_ide::IdeWorld`.
  - Main file text comes from the buffer (`Source::replace` for incremental reparse). Other files: read from disk under the root.
  - Other files: `typst_kit::files::FileStore<SystemFiles>`, reset before each compile so disk changes show.
  - Fonts: `typst-kit` font search (system + embedded), loaded once per process (`LazyLock`), on first use (a compile thread).
  - Packages: `typst_kit::packages::SystemPackages` with `SystemDownloader` (Typst Universe download into the cache dir).
  - Diagnostics: converted to char ranges in the main file. A diagnostic in another file goes to the innermost main-file call site in its trace, with the file name in the message.
- `session.rs`: `Session`, owned by Lisp as a `user-ptr`. Drop stops and joins the thread.
  - All shared state is in one `Arc<Shared>`: request slot, world, output.
  - Request slot: `Mutex<Slot>` + `Condvar`. A request carries the new text (optional), the theme colors (optional), and the view (fit width px, zoom, desk color). A theme change compiles again, also without new text. A new request replaces an unserved one, but keeps its text if it has none. Each request gets an ID.
  - World: `Mutex<PreviewWorld>`; the thread locks it for update + compile. Phase 2: the Lisp thread locks it briefly for jumps.
  - Output: `Mutex<Output>` with the ID of the newest served request (Lisp compares it with the newest sent ID to show "compiling"), the last good document (`Arc`), rendered pages (`Arc<PageImage>`), diagnostics, compile ms, render ms.
  - Notify: after each served request, the thread writes `"\n"` to the pipe. Write errors are ignored.
  - A panic in Typst becomes an error diagnostic. The thread continues.
- `render.rs`: `typst_render::render` → premultiplied RGBA → `0xAARRGGBB` composited onto the page, plus margin, shadow, caret.
  - Each `PageImage` has a key (page hash via `typst::utils::hash128(&Page)`, scale, window width, desk color) and a serial. A re-render reuses images with the same key, also if their page moved. Lisp copies a page only if its serial changed.
  - Scale: the widest page fits the window width minus margins, times the zoom. A pixel budget per page caps it.
- `offset.rs`: UTF-8 byte ↔ Emacs char offset conversion.
- `sync.rs` (Phase 2): click/cursor jumps.

Invariant: only defuns (Lisp thread) touch canvas memory, and only inside `with_canvas_data`. The
thread touches only Rust-owned buffers. Canvas size mismatch → skip copy, never panic.

## Lisp (`typst-canvas.el`)

- `typst-canvas-mode`: minor mode for a `.typ` buffer. Starts a session, a pipe process, the preview window, the Flymake backend, `after-change-functions` (Phase 2: `post-command-hook`) handlers.
  - Changes schedule a zero-delay timer that sends the whole text once per command.
  - Teardown stops the session before it deletes the pipe process: in batch mode, Emacs does not ignore `SIGPIPE`.
  - Flymake: the backend stores the newest report function, and reports when diagnostics change. It has `flymake-always-safe`, because the user started the compile with the mode, not Flymake.
- `typst-canvas-preview-mode`: `special-mode` for `*typst-canvas: NAME*`. Keys: `+ - 0 t g q`, `n`/`p` pages. Phase 2: `mouse-1` jump.
  - `window-size-change-functions` re-renders when the window body width changes.
- Each page is one line: an image char with a `typst-canvas-page` text property (0-based index), and a newline. Page lines are added or removed at the end.
- Canvas specs get an uninterned `:id`: Emacs finds canvases by `eq` spec, but its image cache matches specs by `equal`. Resize: `plist-put` of `:data-width`/`:data-height` on the same spec.

## Testing

- `cargo test`: Rust unit tests (offsets, pixel conversion, compile, diagnostics, session thread).
- `bin/test.sh`: ERT in batch mode (`EMACS=emacs-32-gtk`).
- `bin/screenshot.sh`: GUI under Xvfb, saves PNGs to `target/`.

## Phases

1. Core: scaffold, world, session thread, pipe notify, multi-page canvases, fit/zoom, diagnostics (header line + Flymake), tests, scripts.
2. Sync and polish: backward/forward sync, theme, page decoration. (Phase 1 already reuses page images by key.)
3. Showcase: demo document, self-typing demo command, screencast (GIF), README.
4. Review: code review pass, fixes, docs.

## Typst 0.15 API notes for Phase 2

- `typst_ide::jump_from_click(world: &dyn IdeWorld, document: &PagedDocument, position: &PagedPosition) -> Option<Jump>`.
  `PagedPosition { page: NonZeroUsize /* 1-based */, point: Point /* pt, from the page's top left */ }`.
  `Jump::File(FileId, usize /* byte offset */) | Jump::Url(Url) | Jump::Position(PagedPosition)`.
  Pixel → point: `(x - page_x) / pixel_per_pt` (store `page_x`, `page_y`, `pixel_per_pt` in `PageImage`).
- `typst_ide::jump_from_cursor(document: &PagedDocument, source: &Source, cursor: usize /* byte */) -> Vec<PagedPosition>`.
  Only `Text`/`MathText` leaves match. The point is the start of the first glyph with that span, on the text baseline (the glyph box is `y - size .. y`).
- Both resolve spans against the world's current sources. After a failed compile, the last good document has spans of older text, so a jump can miss or be wrong.
- Theme: `library.styles.set(PageElem::fill, Smart::Custom(Some(color.into())))` and `library.styles.set(TextElem::fill, color.into())`, with `Color::from_u8(r, g, b, 255)`. Set them on `Library::default()`, then replace `PreviewWorld::library` (`LazyHash::new`). See `typst-ide/src/tests.rs`.
- Frames hash cheaply: `Frame` items are an `Arc<LazyHash<..>>`, so `hash128(&page)` reuses cached item hashes.
