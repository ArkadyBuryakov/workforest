#!/bin/sh
# Build the `workforest` executable the editor plugins ship (editors/vscode,
# editors/idea): an optimized build of this checkout, one self-contained
# file — what it needs at run time (the shell integration, the config
# starter) is compiled in.
#
#   packaging/binary/build.sh [OUTDIR]      # default: dist/binary
#
# The binary runs on the OS and architecture it was built for: each of
# linux-x64, linux-arm64, darwin-x64, darwin-arm64 is built on a matching
# runner (.github/workflows/binaries.yml).
#
# Not a replacement for the packaged installs: no `wf` alias, no man pages,
# no shell-init in the user's rc. It is the plugins' fallback for people who
# have not installed workforest themselves.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/dist/binary}

# --locked: the dependency versions Cargo.lock records, never newer ones.
cargo build --manifest-path "$root/Cargo.toml" --release --locked --bin workforest

target_dir=${CARGO_TARGET_DIR:-$root/target}
mkdir -p "$out"
cp "$target_dir/release/workforest" "$out/workforest"

"$out/workforest" --version
