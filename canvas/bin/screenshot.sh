#!/usr/bin/env bash
# Run the demo in a GUI frame under Xvfb, and save two screenshots 0.5 s apart to target/. Needs
# xvfb-run and an Emacs 32 built with Cairo, e.g. EMACS=emacs-32-gtk bin/screenshot.sh.

set -euo pipefail

here=$(cd "$(dirname "$BASH_SOURCE")" && pwd)
root=$(cd "$here/.." && pwd)
module_dir=$root/target/debug

(cd "$root" && cargo build)
ln -f -s libemacs_canvas_demo.so "$module_dir/canvas-demo-dyn.so"

xvfb-run --auto-servernum --server-args="-screen 0 800x600x24" \
    "${EMACS:-emacs}" -Q -L "$module_dir" -L "$root" --eval "
(progn
  (require 'canvas-demo)
  (canvas-demo)
  (delete-other-windows)
  (defun canvas-demo-screenshot (file)
    (let ((png (x-export-frames nil 'png)))
      (with-temp-file file
        (set-buffer-multibyte nil)
        (insert png))))
  (run-with-timer 1.0 nil #'canvas-demo-screenshot \"$root/target/screenshot-1.png\")
  (run-with-timer 1.5 nil #'canvas-demo-screenshot \"$root/target/screenshot-2.png\")
  (run-with-timer 2.0 nil #'kill-emacs 0))"

echo "Saved $root/target/screenshot-1.png and $root/target/screenshot-2.png"
