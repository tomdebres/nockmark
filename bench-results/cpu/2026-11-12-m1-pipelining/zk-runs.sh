#!/bin/bash
# Local ZK CPU timing runs, M7 step 5 pt 2: same tier (2^23), k=8 from the local registry.
# Usage: RUN_DIR=<dir holding bin/tock> [KERNEL=<miner.jam>] ./zk-runs.sh
# Needs a local registry on 127.0.0.1:8099. Logs and reports land in RUN_DIR.
R=${RUN_DIR:?set RUN_DIR to a directory holding bin/tock}
J=${KERNEL:-$(git -C "$(dirname "$0")" rev-parse --show-toplevel)/tock/assets/miner.jam}
( while true; do echo "$(date +%s) $(sysctl -n vm.loadavg)"; sleep 15; done ) > $R/load.log 2>&1 &
SAMPLER=$!
run() { name=$1; shift
  echo "=== $name $(date -u +%H:%M:%S)"
  RUST_MIN_STACK=8388608 $R/bin/tock zk-bench --kernel $J --attempts 8388608 -k 8 --submit http://127.0.0.1:8099 --report $R/$name.json "$@" > $R/$name.log 2>&1
  echo "exit $? $(date -u +%H:%M:%S)"; tail -12 $R/$name.log
}
run zk-serial --serial
run zk-pipelined
run zk-pipelined-t9 --threads 9
kill $SAMPLER
