#!/usr/bin/env bash
# Decide which packages to build, and their version. Writes `packages` (JSON array) and `version` to
# $GITHUB_OUTPUT. A tag `<package>-v<version>` selects one package, and must match its Cargo.toml.
# Other refs build all packages, and use the short commit hash as the version.

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
all='["canvas","typst-canvas"]'

if [[ ${GITHUB_REF_TYPE:-} == tag ]]; then
    tag=$GITHUB_REF_NAME
    package=${tag%-v*}
    version=${tag##*-v}
    if ! jq -e --arg p "$package" 'index($p)' <<<"$all" >/dev/null; then
        echo "Unknown package in tag: $tag" >&2
        exit 1
    fi
    crate_version=$(cd "$root/$package" && cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
    if [[ $version != "$crate_version" ]]; then
        echo "Tag version $version does not match $package/Cargo.toml version $crate_version" >&2
        exit 1
    fi
    packages=$(jq -c -n --arg p "$package" '[$p]')
    version=v$version
else
    packages=$all
    version=$(git -C "$root" rev-parse --short HEAD)
fi

echo "packages=$packages" >>"${GITHUB_OUTPUT:-/dev/stdout}"
echo "version=$version" >>"${GITHUB_OUTPUT:-/dev/stdout}"
