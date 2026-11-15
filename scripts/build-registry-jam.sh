#!/usr/bin/env bash
# Compile hoon/registry.hoon against the nockchain hoon tree WITHOUT
# touching the nockchain checkout: copy its hoon/ into target/, add ours.
#
#   NOCKCHAIN=<checkout at the pinned rev, hoonc built> scripts/build-registry-jam.sh
#
# NOCKCHAIN must be a checkout at the rev pinned in tock/Cargo.toml. The
# default (two levels above this repo) only holds for the main checkout; from
# an Orca worktree it points at the wrong folder, so pass it explicitly.
set -euo pipefail
export PATH="$HOME/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin:$PATH"
export RUST_MIN_STACK=8388608
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NOCKCHAIN="${NOCKCHAIN:-$ROOT/../../nockchain}"
HOONC="$NOCKCHAIN/target/release/hoonc"
[ -x "$HOONC" ] || { echo "build hoonc first: (cd $NOCKCHAIN && cargo build --release -p hoonc)"; exit 1; }
BUILD="$ROOT/target/hoon-build"
rm -rf "$BUILD" && mkdir -p "$BUILD"
# -L: hoon/common/hoon.hoon is a relative symlink into crates/. hoonc parses
# every file under the tree it is given, and since ed1edfcc that includes
# hoon/tests/compiler-errors/, deliberately unparseable fixtures that stop the
# build without a jam (and with exit status 0). Nothing imports them, so they
# go; the rest of hoon/tests/ stays (roswell.hoon imports from it).
cp -RL "$NOCKCHAIN/hoon" "$BUILD/hoon"
rm -rf "$BUILD/hoon/tests/compiler-errors"
cp "$ROOT/hoon/registry.hoon" "$BUILD/hoon/apps/registry.hoon"
mkdir -p "$ROOT/tock/assets"
DATA="$ROOT/target/hoonc-data-registry-$$"
cd "$BUILD"
rm -f registry.jam
"$HOONC" --new --data-dir "$DATA" --output registry.jam hoon/apps/registry.hoon hoon
rm -rf "$DATA"
# hoonc can stop without writing a jam and still exit 0: check for the file.
[ -s registry.jam ] || { echo "hoonc wrote no registry.jam (see its log above)" >&2; exit 1; }
mv registry.jam "$ROOT/tock/assets/registry.jam"
echo "wrote $ROOT/tock/assets/registry.jam"
