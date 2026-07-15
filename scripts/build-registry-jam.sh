#!/usr/bin/env bash
# Compile hoon/registry.hoon against the nockchain hoon tree WITHOUT
# touching the nockchain checkout: copy its hoon/ into target/, add ours.
set -euo pipefail
export PATH="$HOME/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin:$PATH"
export RUST_MIN_STACK=8388608
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NOCKCHAIN="$ROOT/../../nockchain"
HOONC="$NOCKCHAIN/target/release/hoonc"
[ -x "$HOONC" ] || { echo "build hoonc first: (cd $NOCKCHAIN && cargo build --release -p hoonc)"; exit 1; }
BUILD="$ROOT/target/hoon-build"
rm -rf "$BUILD" && mkdir -p "$BUILD"
cp -r "$NOCKCHAIN/hoon" "$BUILD/hoon"
cp "$ROOT/hoon/registry.hoon" "$BUILD/hoon/apps/registry.hoon"
mkdir -p "$ROOT/tock/assets"
DATA="$ROOT/target/hoonc-data-registry-$$"
cd "$BUILD"
"$HOONC" --new --data-dir "$DATA" --output registry.jam hoon/apps/registry.hoon hoon
mv "$BUILD/registry.jam" "$ROOT/tock/assets/registry.jam" 2>/dev/null || mv registry.jam "$ROOT/tock/assets/registry.jam"
rm -rf "$DATA"
echo "wrote $ROOT/tock/assets/registry.jam"
