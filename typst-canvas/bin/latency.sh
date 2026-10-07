#!/usr/bin/env bash
# Measure the latency from an edit in examples/showcase.typ to the redisplay that shows it, in a
# GUI frame under Xvfb (see bin/latency.el). Needs xvfb-run, jq, and an Emacs 32 built with Cairo,
# e.g. EMACS=emacs-32-gtk bin/latency.sh.

set -euo pipefail

here=$(cd "$(dirname "$BASH_SOURCE")" && pwd)
root=$(cd "$here/.." && pwd)
# Honor CARGO_TARGET_DIR and other Cargo configuration.
target_dir=$(cd "$root" && cargo metadata --no-deps --format-version 1 | jq -r .target_directory)
module_dir=$target_dir/debug

(cd "$root" && cargo build)
ln -f -s libtypst_canvas.so "$module_dir/typst-canvas-dyn.so"

xvfb-run --auto-servernum --server-args="-screen 0 1600x900x24" \
    "${EMACS:-emacs}" -Q -L "$module_dir" -L "$root" -l "$here/latency.el"
