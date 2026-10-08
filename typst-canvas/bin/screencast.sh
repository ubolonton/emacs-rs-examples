#!/usr/bin/env bash
# Record `typst-canvas-demo' in a dark theme under Xvfb (see bin/screencast.el). Needs xvfb-run,
# ffmpeg, jq, and an Emacs 32 built with Cairo, e.g. EMACS=emacs-32-gtk bin/screencast.sh.
# Scenarios, as the optional argument:
# - demo (default): the whole demo, with the preview. Saves target/screencast.{mp4,gif}.
# - inline-math: an equation typed in the source buffer alone. Saves target/inline-math.{mp4,gif}.

set -euo pipefail

here=$(cd "$(dirname "$BASH_SOURCE")" && pwd)
root=$(cd "$here/.." && pwd)
# Honor CARGO_TARGET_DIR and other Cargo configuration.
target_dir=$(cd "$root" && cargo metadata --no-deps --format-version 1 | jq -r .target_directory)
module_dir=$target_dir/debug

scenario=${1:-demo}
case $scenario in
    demo) screen=1600x900 name=screencast gif_width=1000 ;;
    inline-math) screen=1280x720 name=inline-math gif_width=960 ;;
    *) echo "Unknown scenario: $scenario (expected demo or inline-math)" >&2; exit 1 ;;
esac
raw=$root/target/$name-raw.mkv
mp4=$root/target/$name.mp4
gif=$root/target/$name.gif
gif_fps=12
# Few encoder threads: the encodes are not urgent, and other work shares the machine.
threads=2

(cd "$root" && cargo build)
ln -f -s libtypst_canvas.so "$module_dir/typst-canvas-dyn.so"
mkdir -p "$root/target"

TYPST_CANVAS_SCREENCAST=$scenario TYPST_CANVAS_SCREENCAST_OUTPUT=$raw \
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
