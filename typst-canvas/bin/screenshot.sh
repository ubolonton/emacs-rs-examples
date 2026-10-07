#!/usr/bin/env bash
# Open examples/sample.typ with `typst-canvas-mode' in a GUI frame under Xvfb, and save screenshots
# to target/screenshot-{1..5}.png (see bin/screenshot.el). Needs xvfb-run and an Emacs 32 built
# with Cairo, e.g. EMACS=emacs-32-gtk bin/screenshot.sh. Also needs jq.

set -euo pipefail

here=$(cd "$(dirname "$BASH_SOURCE")" && pwd)
root=$(cd "$here/.." && pwd)
# Honor CARGO_TARGET_DIR and other Cargo configuration.
target_dir=$(cd "$root" && cargo metadata --no-deps --format-version 1 | jq -r .target_directory)
module_dir=$target_dir/debug

(cd "$root" && cargo build)
ln -f -s libtypst_canvas.so "$module_dir/typst-canvas-dyn.so"
mkdir -p "$root/target"

xvfb-run --auto-servernum --server-args="-screen 0 1280x800x24" \
    "${EMACS:-emacs}" -Q -L "$module_dir" -L "$root" -l "$here/screenshot.el"
