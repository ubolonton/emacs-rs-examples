#!/usr/bin/env bash
# Run the ERT tests in batch mode. Needs an Emacs 32 built with a window system, e.g.
# EMACS=emacs-32-gtk bin/test.sh. Batch mode is enough: canvases do not need a frame.

set -euo pipefail

here=$(cd "$(dirname "$BASH_SOURCE")" && pwd)
root=$(cd "$here/.." && pwd)
module_dir=$root/target/debug

case $(uname) in
    Linux) ext=so ;;
    Darwin) ext=dylib ;;
    *) echo "Unsupported system: $(uname)" >&2; exit 1 ;;
esac

(cd "$root" && cargo build)
ln -f -s "libemacs_canvas_demo.$ext" "$module_dir/canvas-demo-dyn.so"

"${EMACS:-emacs}" -Q -batch -L "$module_dir" -L "$root" -l ert -l "$root/test.el" \
    -f ert-run-tests-batch-and-exit
