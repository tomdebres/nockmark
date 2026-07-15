#!/usr/bin/env bash
# Provision a fresh Linux box (Ubuntu 24.04, x86_64 or aarch64) to run tock
# benches against the Nockchain prover, then run the standard matrix.
#
# Usage (from your Mac, in the nockmark repo root):
#   scp -i key.pem -r tock ubuntu@<ip>:tock
#   scp -i key.pem tock/assets/miner.jam tock/setup-bench.sh ubuntu@<ip>:
#   ssh -i key.pem ubuntu@<ip> 'bash setup-bench.sh 2>&1 | tee setup.log'
#   scp -i key.pem 'ubuntu@<ip>:bench-results/*.json' bench-results/
# (scp -r tock also copies tock/target and tock/assets — harmless but slow;
#  add --exclude via rsync if you prefer: rsync -a --exclude target tock/ ubuntu@<ip>:tock/)
#
# Pin these to what the local benches used (see the findings doc / bench JSON):
NOCKCHAIN_REPO=${NOCKCHAIN_REPO:-https://github.com/zorp-corp/nockchain}
NOCKCHAIN_COMMIT=${NOCKCHAIN_COMMIT:-31b8a015}
NOCKMARK_REPO=${NOCKMARK_REPO:-}   # if empty, tock sources must be scp'd to ~/tock
TOOLCHAIN_DATE=2026-04-03
K_SINGLE=${K_SINGLE:-6}

set -euxo pipefail

ARCH=$(uname -m)   # x86_64 | aarch64
case "$ARCH" in
  x86_64) RUST_TRIPLE=x86_64-unknown-linux-gnu ;;
  aarch64) RUST_TRIPLE=aarch64-unknown-linux-gnu ;;
  *) echo "unsupported arch $ARCH" >&2; exit 1 ;;
esac

sudo apt-get update -qq
sudo apt-get install -y -qq build-essential clang pkg-config libssl-dev git curl xz-utils

# --- pinned nightly toolchain, standalone (no rustup) --------------------
TC="$HOME/rust-nightly"
if [ ! -x "$TC/bin/cargo" ]; then
  mkdir -p "$TC" /tmp/rust-dl && cd /tmp/rust-dl
  for c in rustc cargo rust-std; do
    curl -sSfLO "https://static.rust-lang.org/dist/$TOOLCHAIN_DATE/$c-nightly-$RUST_TRIPLE.tar.xz"
    tar xf "$c-nightly-$RUST_TRIPLE.tar.xz"
    "./$c-nightly-$RUST_TRIPLE/install.sh" --prefix="$TC" --disable-ldconfig >/dev/null
  done
fi
export PATH="$TC/bin:$PATH"
export RUST_MIN_STACK=8388608
cargo --version

# --- nockchain at the pinned commit --------------------------------------
cd "$HOME"
if [ ! -d nockchain ]; then
  git clone --filter=blob:none "$NOCKCHAIN_REPO" nockchain
fi
git -C nockchain checkout "$NOCKCHAIN_COMMIT"

# --- tock (this repo's harness) -------------------------------------------
# Path deps expect nockchain at ../../../nockchain relative to tock/, i.e.
# layout <root>/nockmark/m0-prover-spike/tock and <root>/nockchain.
mkdir -p nockmark/m0-prover-spike
if [ -n "$NOCKMARK_REPO" ]; then
  [ -d nockmark-src ] || git clone "$NOCKMARK_REPO" nockmark-src
  cp -r nockmark-src/tock nockmark/m0-prover-spike/tock
elif [ -d "$HOME/tock" ]; then
  cp -r "$HOME/tock" nockmark/m0-prover-spike/tock
else
  echo "no tock sources: set NOCKMARK_REPO or scp the tock/ dir to ~/tock" >&2
  exit 1
fi

# miner.jam must have been scp'd to ~ (42 MB beats a 23-minute hoonc run)
mkdir -p nockmark/m0-prover-spike/tock/assets
cp "$HOME/miner.jam" nockmark/m0-prover-spike/tock/assets/

cd nockmark/m0-prover-spike/tock
cargo build --release
cargo test --release

# --- standard bench matrix -------------------------------------------------
CORES=$(nproc)
OUT="$HOME/bench-results"
mkdir -p "$OUT"
HOSTTAG=$(uname -m)-${CORES}c
./target/release/tock bench --seed m1-baseline --kernel assets/miner.jam \
  -k "$K_SINGLE" -t 1 --json > "$OUT/$HOSTTAG-t1.json"
./target/release/tock bench --seed m1-baseline --kernel assets/miner.jam \
  -k $((CORES * 2)) -t "$CORES" --json > "$OUT/$HOSTTAG-t$CORES.json"

echo "done — results in $OUT:"
ls -l "$OUT"
