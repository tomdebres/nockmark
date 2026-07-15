#!/usr/bin/env bash
# GPU spike (M1.5): measure the GoldenMiner closed-source CUDA prover's
# self-reported proving rate, mining against their real pool with a
# throwaway pubkey. Numbers are UNVERIFIED (pool/self-reported) — labelled
# as such; a verified GPU workload class needs fake-pool work injection
# (see the M1 results doc follow-ups).
#
# Designed for throwaway GPU boxes: RunPod / Vast.ai containers (root, no
# sudo needed, NVIDIA drivers provided) or any Ubuntu box with an NVIDIA
# GPU. The box/pod should be destroyed after the run.
#
# One-liner for a RunPod web terminal:
#   curl -sSfL https://raw.githubusercontent.com/tomdebres/nockmark/main/tock/gpu-spike.sh | bash
# Optional args: <pubkey> [duration-seconds]  (defaults: nockmark burner, 900s)
set -euo pipefail

# Nockmark throwaway benchmarking address (burner; override with $1).
PUBKEY=${1:-XaJktdYiva2QsDL2pyJxmzQCeNdUgELdNvfDjy6JNiCCTZHH4KjMnh}
DUR=${2:-900}
VER=v0.4.3

WORK=${TMPDIR:-/tmp}/nockmark-gpu-spike
mkdir -p "$WORK" && cd "$WORK"
echo "== nockmark gpu-spike: workdir $WORK, duration ${DUR}s, prover $VER =="

# Authorize the nockmark bench key so the orchestrating session can ssh in
# to collect logs (public key — safe to embed; box is throwaway).
mkdir -p ~/.ssh && chmod 700 ~/.ssh
grep -q "nockmark-bench" ~/.ssh/authorized_keys 2>/dev/null || cat >> ~/.ssh/authorized_keys <<'EOF'
ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCUM4mbWn3ZOr2ZXECRyvXzs2jMe692vAbYEWhF4OZ0+dmQhpu+i3+KwYs/emcOg5caiBwlkIzvIAPE0m4vL9dWjHrC7cr3VONH12Kz51X/aHigXdsfOBuPJhtG+cYq6BN1kMar6vh4keyEWjyRzd4U4EyH9AkrsUeCJXnwLF1rOh89lu4bgAkVzmkZif4VpK1nbHaqd+mCwEvgkmxv8xoTns3L5k02HV31MdwCy08+Yaz/meIxr1o5fH+RIKkBfBjtaz6WPSiH8cBOjK90aevgaRFJroCFVS/rSDIcqg5I/Dtn4ez3yyJth4qMgzydZozjUn1/wL+i/UYDHVHWyQ4N nockmark-bench
EOF

command -v nvidia-smi >/dev/null || { echo "no nvidia-smi — not a GPU box?"; exit 1; }
nvidia-smi -L

curl -sSfLO "https://github.com/GoldenMinerNetwork/golden-miner-nockchain-gpu-miner/releases/download/$VER/golden-miner-pool-prover"
chmod +x golden-miner-pool-prover

# GPU utilization + power log, sampled every 15s
nvidia-smi --query-gpu=timestamp,utilization.gpu,power.draw,memory.used \
  --format=csv -l 15 > gpu-util.csv &
SMI_PID=$!

# Run the prover for the window; it talks to Golden Miner's pool itself.
timeout "$DUR" ./golden-miner-pool-prover \
  --pubkey="$PUBKEY" --name=nockmark-gpu-spike --mode=gpu 2>&1 | tee prover.log || true
kill "$SMI_PID" 2>/dev/null || true

echo "== rate lines (self-reported, unverified) =="
grep -iE "proof|rate|/s|submit|share|accept" prover.log | tail -60 | tee rate-summary.txt || true
echo "== gpu utilization (tail) =="
tail -5 gpu-util.csv || true
echo "== done: copy prover.log gpu-util.csv rate-summary.txt from $WORK before destroying the pod =="
