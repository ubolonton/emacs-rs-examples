#!/usr/bin/env bash
# Record `typst-canvas-demo' in a dark theme under Xvfb (see bin/screencast.el), and save
# target/screencast.mp4 and target/screencast.gif. Needs xvfb-run, ffmpeg, jq, and an Emacs 32
# built with Cairo, e.g. EMACS=emacs-32-gtk bin/screencast.sh.

set -euo pipefail

here=$(cd "$(dirname "$BASH_SOURCE")" && pwd)
root=$(cd "$here/.." && pwd)
# Honor CARGO_TARGET_DIR and other Cargo configuration.
target_dir=$(cd "$root" && cargo metadata --no-deps --format-version 1 | jq -r .target_directory)
module_dir=$target_dir/debug

screen=1600x900
raw=$root/target/screencast-raw.mkv
mp4=$root/target/screencast.mp4
gif=$root/target/screencast.gif
gif_width=1000
gif_fps=12
# Few encoder threads: the encodes are not urgent, and other work shares the machine.
threads=2

(cd "$root" && cargo build)
ln -f -s libtypst_canvas.so "$module_dir/typst-canvas-dyn.so"
mkdir -p "$root/target"

xvfb-run --auto-servernum --server-args="-screen 0 ${screen}x24" \
    "${EMACS:-emacs}" -Q -L "$module_dir" -L "$root" -l "$here/screencast.el"

ffmpeg -loglevel error -y -i "$raw" -threads "$threads" \
    -c:v libx264 -preset slow -crf 26 -tune animation -pix_fmt yuv420p -movflags +faststart \
    "$mp4"
# One palette for the whole GIF, from the pixels that change (`stats_mode=diff`). Ordered dither
# and `diff_mode=rectangle` keep unchanged areas identical between frames, which keeps it small.
ffmpeg -loglevel error -y -i "$raw" -threads "$threads" \
    -filter_complex "fps=$gif_fps,scale=$gif_width:-1:flags=lanczos,split[frames][copy];
        [copy]palettegen=stats_mode=diff[palette];
        [frames][palette]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" \
    "$gif"
rm "$raw"
ls -l "$mp4" "$gif"
