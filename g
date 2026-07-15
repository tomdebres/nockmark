#!/usr/bin/env bash
# Self-daemonizing wrapper: fetches gpu-spike.sh and runs it detached, so
# web-terminal disconnects don't kill the run.
curl -sSfL https://raw.githubusercontent.com/tomdebres/nockmark/main/tock/gpu-spike.sh -o /tmp/nockmark-gpu-spike.sh
pkill -f nockmark-gpu-spike.sh 2>/dev/null || true
pkill -f golden-miner-pool-prover 2>/dev/null || true
nohup bash /tmp/nockmark-gpu-spike.sh "$@" > /tmp/nockmark-gpu-spike.out 2>&1 < /dev/null &
echo "== spike detached (pid $!) — safe to close this terminal =="
echo "check progress later with:  tail -5 /tmp/nockmark-gpu-spike.out"
echo "final summary appears in:   /tmp/nockmark-gpu-spike.out (after ~16 min)"
