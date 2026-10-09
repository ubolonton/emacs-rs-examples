#!/usr/bin/env bash
# Build a package's module in release mode, and bundle it with the Lisp code into
# OUT_DIR/<package>-<version>-<target>.tar.gz, plus a .sha256 file. Needs jq.
# Usage: package.sh PACKAGE TARGET VERSION OUT_DIR

set -euo pipefail

package=$1 target=$2 version=$3
mkdir -p "$4"
out_dir=$(cd "$4" && pwd)
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

case $package in
    canvas)
        lib=emacs_canvas_demo module=canvas-demo-dyn
        files=(canvas-demo.el README.md) ;;
    typst-canvas)
        lib=typst_canvas module=typst-canvas-dyn
        files=(typst-canvas.el typst-canvas-demo.el README.md examples) ;;
    *) echo "Unknown package: $package" >&2; exit 1 ;;
esac

case $target in
    *-linux-*) ext=so ;;
    *-apple-*) ext=dylib ;;
    *) echo "Unsupported target: $target" >&2; exit 1 ;;
esac

cd "$root/$package"
cargo build --release --locked --target "$target"
target_dir=$(cargo metadata --no-deps --format-version 1 | jq -r .target_directory)

name=$package-$version-$target
staging=$target_dir/dist/$name
rm -rf "$staging"
mkdir -p "$staging"
cp -R "${files[@]}" "$staging/"
# Emacs also loads modules with the .so suffix on macOS (MODULES_SECONDARY_SUFFIX), so one name works
# on all systems, as in bin/test.sh.
cp "$target_dir/$target/release/lib$lib.$ext" "$staging/$module.so"

tar -C "$target_dir/dist" -czf "$out_dir/$name.tar.gz" "$name"
(cd "$out_dir" && shasum -a 256 "$name.tar.gz" >"$name.tar.gz.sha256")
