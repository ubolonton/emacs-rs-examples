#!/usr/bin/env bash
# Run the ERT tests in batch mode. Needs an Emacs 32 built with a window system, e.g.
# EMACS=emacs-32-gtk bin/test.sh. Batch mode is enough: canvases do not need a frame. An optional
# argument is an ERT selector, e.g. bin/test.sh '"::mode"'. Also needs jq. Rust unit tests: cargo test.

set -euo pipefail

here=$(cd "$(dirname "$BASH_SOURCE")" && pwd)
root=$(cd "$here/.." && pwd)
# Honor CARGO_TARGET_DIR and other Cargo configuration.
target_dir=$(cd "$root" && cargo metadata --no-deps --format-version 1 | jq -r .target_directory)
module_dir=$target_dir/debug

case $(uname) in
    Linux) ext=so ;;
    Darwin) ext=dylib ;;
    *) echo "Unsupported system: $(uname)" >&2; exit 1 ;;
esac

(cd "$root" && cargo build)
ln -f -s "libtypst_canvas.$ext" "$module_dir/typst-canvas-dyn.so"

"${EMACS:-emacs}" -Q -batch -L "$module_dir" -L "$root" -l ert -l "$root/test.el" \
    --eval "(ert-run-tests-batch-and-exit ${1:-t})"
