# M2 Registry Deployment Runbook

> **Deployed 2026-07-16 on Railway** (now the primary path): project
> `nockmark-registry`, live at https://nockmark-registry-production.up.railway.app
> with a persistent volume at `/data`. The repo's `Dockerfile` + `railway.json`
> encode the whole build (jams ship from `deploy/assets/`); redeploy = `railway up`.
> The VPS instructions below remain valid as the self-hosted alternative.

Deploy a Nockmark registry to a Linux VPS. The registry verifies STARK proofs against the Nockchain kernel and maintains a public leaderboard.

## VPS Requirements

**Hardware:** 2 vCPU, 4 GB RAM, Ubuntu 24.04 (x86_64 or aarch64)

**Rationale:** Verification takes ~0.5 s/proof (the kernel, not the registry, is the bottleneck). Kernel boots (~3 s each on cold) are infrequent. The registry itself does not prove. 2 vCPU is sufficient for foreground proof verification + leaderboard queries.

## Prerequisites on the Build Machine

Before deploying, have on hand:
- The pinned nightly Rust toolchain (`nightly-2026-04-03`) from static.rust-lang.org
- The nockchain checkout (same commit as your benchmarks, e.g., `31b8a015`)
- The miner.jam kernel (pre-compiled; ~116 KB when jammed)
- The roswell.jam kernel (pre-compiled; test harness exposing `%verify-proof`)

## Build Steps (on the VPS)

### Step 1: Install the Rust toolchain

SSH into the VPS and detect your architecture:

```bash
ARCH=$(uname -m)   # x86_64 or aarch64
case "$ARCH" in
  x86_64) RUST_TRIPLE=x86_64-unknown-linux-gnu ;;
  aarch64) RUST_TRIPLE=aarch64-unknown-linux-gnu ;;
  *) echo "unsupported arch $ARCH" >&2; exit 1 ;;
esac

sudo apt-get update && sudo apt-get install -y \
  build-essential clang pkg-config libssl-dev git curl xz-utils

# Download and install the pinned nightly toolchain (no rustup)
TC="$HOME/rust-nightly"
mkdir -p "$TC" /tmp/rust-dl && cd /tmp/rust-dl
for c in rustc cargo rust-std; do
  curl -sSfLO "https://static.rust-lang.org/dist/2026-04-03/$c-nightly-$RUST_TRIPLE.tar.xz"
  tar xf "$c-nightly-$RUST_TRIPLE.tar.xz"
  "./$c-nightly-$RUST_TRIPLE/install.sh" --prefix="$TC" --disable-ldconfig >/dev/null
done
export PATH="$TC/bin:$PATH"
export RUST_MIN_STACK=8388608
cargo --version  # verify installation
```

### Step 2: Clone and prepare sources

```bash
cd "$HOME"
# Tom's fork is the guaranteed source of the pinned commit 31b8a015
# (upstream is https://github.com/nockchain/nockchain.git; tock/setup-bench.sh
# uses the same fork default, overridable via NOCKCHAIN_REPO).
git clone --filter=blob:none https://github.com/tomdebres/nockchain.git nockchain
git -C nockchain checkout 31b8a015

# Mirror the nockmark layout: <root>/nockchain and <root>/nockmark/m0-prover-spike
mkdir -p nockmark/m0-prover-spike
cd nockmark/m0-prover-spike

# Clone the nockmark registry
git clone <nockmark-repo-url> .
# OR: tar xzf nockmark.tar.gz
```

### Step 3: Build hoonc

```bash
cd "$HOME/nockchain"
export PATH="$HOME/rust-nightly/bin:$PATH"
export RUST_MIN_STACK=8388608
cargo build --release -p hoonc
```

(This takes ~5 minutes.)

### Step 4: Build registry.jam

Back in the nockmark repo, run the build script:

```bash
cd "$HOME/nockmark/m0-prover-spike"
export PATH="$HOME/rust-nightly/bin:$PATH"
export RUST_MIN_STACK=8388608
bash scripts/build-registry-jam.sh
```

This copies hoonc, the nockchain hoon tree, and the registry.hoon file, then compiles. Output: `tock/assets/registry.jam`.

Note: the script's own `export PATH=...` line references a macOS toolchain dir (`~/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin`); on Linux that export is a no-op and the hoonc absolute path (`$NOCKCHAIN/target/release/hoonc`) is what matters — or adjust the PATH line for the Linux triple as setup-bench.sh does.

### Step 5: Ensure roswell.jam is present (required before the binary build)

The registry binary build (Step 6) compiles roswell.jam in via `include_bytes!` — `tock/assets/roswell.jam` MUST exist first. If you have miner.jam but not roswell.jam:

```bash
cd "$HOME/nockchain"
export PATH="$HOME/rust-nightly/bin:$PATH"
export RUST_MIN_STACK=8388608
target/release/hoonc --new --data-dir /tmp/hoonc-roswell \
  --output roswell.jam hoon/apps/roswell/roswell.hoon hoon
# Move it to the assets dir referenced by registry/.cargo/config.toml
mv roswell.jam "$HOME/nockmark/m0-prover-spike/tock/assets/"
```

### Step 6: Compile the registry binary

```bash
cd "$HOME/nockmark/m0-prover-spike/registry"
export PATH="$HOME/rust-nightly/bin:$PATH"
export RUST_MIN_STACK=8388608
cargo build --release
```

The `.cargo/config.toml` in the registry crate pins `KERNEL_JAM_PATH = ../tock/assets/roswell.jam` (relative). The roswell.jam kernel is **compiled into the binary** via `include_bytes!`; roswell.jam is NOT needed at runtime.

## Runtime Artifacts

Copy these files from the build VPS to `/opt/nockmark` on the registry VPS:

- `registry/target/release/nockmark-registry` — the registry binary
- `tock/assets/registry.jam` — the registry kernel (loaded at runtime via `--kernel` flag)

**Note:** `roswell.jam` is **not** needed at runtime. It was compiled into the `nockmark-registry` binary as part of the verifier's dependency chain. Only `registry.jam` is loaded by the registry at startup.

```bash
# On the registry VPS:
sudo mkdir -p /opt/nockmark/data
sudo cp /path/to/nockmark-registry /opt/nockmark/
sudo cp /path/to/registry.jam /opt/nockmark/
sudo chown -R nobody:nogroup /opt/nockmark
```

## systemd Unit

Create `/etc/systemd/system/nockmark-registry.service`:

```ini
[Unit]
Description=Nockmark Registry
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=nobody
Group=nogroup
Environment=RUST_MIN_STACK=8388608
ExecStart=/opt/nockmark/nockmark-registry --listen 127.0.0.1:8080 --kernel /opt/nockmark/registry.jam --data-dir /opt/nockmark/data
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Enable and start the service:

```bash
sudo systemctl daemon-reload
sudo systemctl enable nockmark-registry
sudo systemctl start nockmark-registry
sudo systemctl status nockmark-registry
```

## Reverse Proxy (Caddy)

Install Caddy (if not present):

```bash
sudo apt-get install -y caddy
```

Add this block to `/etc/caddy/Caddyfile`:

```
nockmark.example {
  reverse_proxy 127.0.0.1:8080
  # No rate limiting is configured by default (see "Known limitations"
  # below). Consider adding Caddy's rate_limit directive (requires the
  # caddy-ratelimit plugin) or a connection cap, e.g.:
  # rate_limit {
  #   zone dynamic_run {
  #     key {remote_host}
  #     events 10
  #     window 1m
  #   }
  # }
}
```

Replace `nockmark.example` with your actual domain. Reload:

```bash
sudo caddy reload
```

Caddy auto-renews HTTPS certificates.

## Seeding the Registry

> As of M3 the supported path is `tock bench --submit <registry-url>` —
> the curl flow below is the manual equivalent, kept for debugging.

Once the registry is live, seed it with a proof to populate the leaderboard.

### From a Bench Machine (with tock and miner.jam)

1. **Mint a challenge:**
   ```bash
   curl -X POST https://nockmark.example/challenge | jq .
   # Response: {"nonce": "1234567890"}
   ```

2. **Run tock to prove k=2 proofs against the nonce:**
   ```bash
   NONCE="1234567890"  # from challenge
   export PATH="$HOME/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin:$PATH"
   export RUST_MIN_STACK=8388608
   
   # --header-seed MUST be the challenge nonce string: the registry's binding
   # check derives the expected header from it — without the flag proofs fail
   # with WrongHeader. Note: each tock prove boots its own kernel (~3 s each);
   # these two commands boot twice.
   cd "$HOME/nockmark/m0-prover-spike"
   ./target/release/tock prove --nonce "$NONCE/0" --header-seed "$NONCE" \
     --kernel tock/assets/miner.jam --out proof0.jam
   ./target/release/tock prove --nonce "$NONCE/1" --header-seed "$NONCE" \
     --kernel tock/assets/miner.jam --out proof1.jam
   ```

3. **Base64-encode the proofs and submit:**
   ```bash
   P0=$(base64 -w0 < proof0.jam)
   P1=$(base64 -w0 < proof1.jam)
   
   curl -X POST https://nockmark.example/run \
     -H "Content-Type: application/json" \
     -d "{
       \"nonce\": \"$NONCE\",
       \"hardware\": \"bench-box-name\",
       \"prover_version\": \"31b8a015\",
       \"elapsed_ms\": 42000,
       \"proofs\": [\"$P0\", \"$P1\"]
     }"
   ```

4. **Verify the run is on the leaderboard:**
   ```bash
   curl https://nockmark.example/leaderboard | jq .
   ```

The e2e test (`registry/tests/e2e.rs`) shows this exact flow; see its source for details.

## Troubleshooting

**`nockmark-registry` fails to start:** Check logs with `journalctl -u nockmark-registry -f`. Verify registry.jam is readable at `/opt/nockmark/registry.jam`.

**Proof verification fails:** The verifier kernel is roswell.jam, compiled into the binary at build time — check that the `tock/assets/roswell.jam` present during `cargo build --release` was the correct one (same nockchain commit as the provers). Kernel crashes usually show as `SEGV` in journalctl.

**Caddy can't reach 127.0.0.1:8080:** Check that nockmark-registry is listening with `ss -tuln | grep 8080`.

## Notes

- The nockchain checkout on the build VPS is READ-ONLY during the build. Never modify it.
- RUST_MIN_STACK=8388608 must be set during both build and runtime (kernel stack requirement).
- The registry kernel (registry.jam) is loaded via command-line `--kernel` at runtime and persists state across requests (kernel pokes). The state is persisted to `--data-dir` between restarts.
- Ensure `/opt/nockmark/data` is writable by the `nobody` user.

## M3 status of the M2 known limitations

All three M2 carry-forwards are closed as of M3:

- **Timing is enforced and the ranked rate is server-side.** The kernel
  rejects submissions whose claimed `elapsed_ms` exceeds the
  server-observed window (`submitted_at − issued_at`), and the leaderboard
  ranks by `k / server_window` — client `elapsed_ms` is displayed as
  `self_reported_pps` only. Note the window includes proof verification
  (~0.5 s/proof), so published rates are slightly conservative lower
  bounds — by design.
- **Rate limiting is in-app.** `POST /challenge` and `POST /run` are
  limited to 10/min per IP (first `X-Forwarded-For` entry; Railway sets
  it). The Caddy `rate_limit` suggestion below remains optional
  belt-and-braces for VPS deploys.
- **Request bodies are explicitly capped at 4 MiB**, and `hardware` /
  `prover_version` strings at 128/64 bytes.

Remaining (acceptable for M3, revisit post-v1): the challenge map grows
monotonically (bounded to ≤14.4k mints/day by the rate limit; expired
entries are rejected but not purged), and hardware descriptors remain
self-reported by design.
