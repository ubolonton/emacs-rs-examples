**This is a simple vibe-coded demonstration of the Rust binding of Emacs 32's module API `canvas_data`.**

Animates an Emacs 32 canvas image (Info node `(elisp) Canvas Images`) from a Rust background thread.

The render thread draws into its own buffer and swaps it into a shared slot. A Lisp timer calls `canvas-demo--present`, which copies the newest frame into the canvas with `Value::with_canvas_data`, then calls `canvas-refresh`.

Requirements:
- Emacs 32 built with a window system.

``` bash
EMACS=emacs-32-gtk bin/test.sh        # ERT tests, batch mode
EMACS=emacs-32-gtk bin/screenshot.sh  # GUI run under Xvfb, saves target/screenshot-{1,2}.png
```

``` emacs-lisp
(add-to-list 'load-path "/path/to/canvas")               ; canvas-demo.el
(add-to-list 'load-path "/path/to/canvas/target/debug")  ; canvas-demo-dyn.so, made by bin/test.sh
(require 'canvas-demo)
(canvas-demo)  ; Kill the buffer to stop.
```
