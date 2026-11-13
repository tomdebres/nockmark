#!/bin/bash
# One instrumented tock run on a sweep pod (M7 step 5, part 2b).
#   sweep-run.sh <name> <tock args...>
# Writes /build/out/<name>.{live.txt,samples.txt,json}: a header (host, GPU,
# binary and jam hashes), every tock output line with a unix timestamp, and a
# 5 s sampler of the GPU (nvidia-smi) and the tock process's CPU ticks.
set -uo pipefail
name=$1; shift
out=/build/out; mkdir -p $out
cd /build/nockmark/tock
live=$out/$name.live.txt; samp=$out/$name.samples.txt
{
  echo "start $(date +%s.%N)"
  echo "nproc $(nproc)"
  echo "cgroup_cpu_max $(cat /sys/fs/cgroup/cpu.max 2>/dev/null || echo none)"
  grep -m1 'model name' /proc/cpuinfo
  grep MemTotal /proc/meminfo
  nvidia-smi --query-gpu=name,driver_version,power.limit,clocks.max.sm,pcie.link.gen.current,pcie.link.width.current --format=csv,noheader
  git -C /build/nockmark rev-parse HEAD
  sha256sum assets/miner.jam target/release/tock 2>/dev/null
  echo "clk_tck $(getconf CLK_TCK)"
  echo "args $*"
} > $live
RUST_MIN_STACK=8388608 ./target/release/tock "$@" --report $out/$name.json \
  > >(while IFS= read -r l; do printf '%s %s\n' "$(date +%s.%N)" "$l"; done >> $live.tlog) 2>&1 &
pid=$!
echo "pid $pid" >> $live
( while kill -0 $pid 2>/dev/null; do
    t=$(date +%s.%N)
    g=$(nvidia-smi --query-gpu=utilization.gpu,clocks.sm,power.draw,clocks_throttle_reasons.active --format=csv,noheader,nounits 2>/dev/null | head -1)
    c=$(awk '{print $14+$15}' /proc/$pid/stat 2>/dev/null)
    echo "$t gpu[$g] cpu_ticks $c"
    sleep 5
  done ) > $samp &
wait $pid; rc=$?
sleep 1
echo "exit $rc $(date +%s.%N)" >> $live
echo "====TLOG" >> $live; cat $live.tlog >> $live; rm -f $live.tlog
echo "SWEEP_RUN_DONE $name rc=$rc"
