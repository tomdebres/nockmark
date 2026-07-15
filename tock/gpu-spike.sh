#!/usr/bin/env bash
# GPU spike (M1.5): measure the GoldenMiner closed-source CUDA prover's
# self-reported proving rate on a GPU box, mining against their real pool
# with a throwaway pubkey. Numbers are UNVERIFIED (pool-reported) — labelled
# as such; the verified-GPU-workload design is a separate project (fake-pool
# work injection, see findings doc).
#
# Run ONLY on a throwaway cloud box (closed binary): the box is terminated
# after the run.
#
# Usage: bash gpu-spike.sh <nock-pubkey> [duration-seconds]
set -euxo pipefail
PUBKEY=${1:?usage: gpu-spike.sh <pubkey> [duration]}
DUR=${2:-900}
VER=v0.4.3

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
kill $SMI_PID || true

echo "=== rate lines ==="
grep -iE "proof|rate|/s|submit|share|accept" prover.log | tail -60 | tee rate-summary.txt
echo "=== gpu util tail ==="
tail -5 gpu-util.csv
