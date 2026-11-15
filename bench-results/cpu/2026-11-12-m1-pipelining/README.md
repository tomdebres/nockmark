# Pipelined vs serial proving, ZK v5 on the M1 Max CPU

2026-11-12, M7 step 5 part 2, before the GPU sweep. `tock zk-bench` at a fixed
tier (2²³) with k = 8, three runs in a row against a local registry
(`--k 8`), release build of this branch, `zk-runs.sh`. The Mac was shared: two
Android emulators and Chrome took about 1.5 cores throughout (1-minute load
average median 101, `load.log`), so the absolute rates are low. The runs are
comparable with each other.

| run | proving | grind threads | hashes / expected | grind to last win | sustained | proofs (mean) | after last win | server window |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `zk-serial` | serial | 10 | 0.794 | 315.9 s | 168.7 kH/s | 21.2 s | 169.3 s | 492.7 s |
| `zk-pipelined` | pipelined, 1 prover | 10 | 0.847 | 354.0 s | 160.6 kH/s | 29.5 s | 31.9 s | 392.5 s |
| `zk-pipelined-t9` | pipelined, 1 prover | 9 | 0.727 | 301.3 s | 161.8 kH/s | 26.5 s | 41.7 s | 349.7 s |

## What it shows

- **The window shrinks as predicted.** Serial, the registry's window is the
  grind plus all 8 proofs (315.9 + 169.3 s); pipelined, the grind plus one
  proof made beside it (354.0 + 31.9 s). The rest, 6.7–7.5 s, is the upload
  and both verifiers.
- **At luck 1** (expected hashes over each run's sustained rate, plus its
  measured tail): serial 574.7 s, pipelined 456.5 s, a ratio of 0.79. As a
  fraction of the uncontended rate (the serial run's 168.7 kH/s), the ranked
  rate recovers 0.69 serial and 0.87 pipelined. The model gives 0.70
  (R/(R+1), R = 2.35 here) and 0.87 (kR/(kR+1) × the grind slowdown, with
  the contended proof time).
- **The CPU trade-off.** On a CPU the prover takes a core from the grinder.
  The grind ran 4.8 % slower (168.7 → 160.6 kH/s), and proofs made beside
  the grind took 26–32 s instead of 21 s. Pipelining still wins by 26 %.
- **Nine grind threads instead of ten** changed nothing measurable (161.8 vs
  160.6 kH/s). So the default stays every core plus one prover. The t9 run's
  longer tail is two wins found 0.3 s apart, proved one after the other.
- All three runs were accepted by the local registry, which verified every
  proof with both verifiers.
