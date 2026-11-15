# H100 NVL: the first datacentre GPU on the board (ZK)

2026-11-15, 19:09–20:23 UTC. One community RunPod **NVIDIA H100 NVL** (94 GB,
PCIe card; RunPod also lists PCIe and SXM variants, neither of which had
community stock). Tom chose "Measure an H100 (ZK)" on 2026-11-15.

Method as in `../2026-11-13-pipelined-sweep/`: tock at main `01c1fc4`
(nockchain `ed1edfcc`), pipelined client, k = 16, self-calibrated against
the live registry (`https://nockmark.xyz`). One binary was built with both
CUDA backends for compute capability 9.0 (`ZK_POW_CUDA_ARCH=90`,
`AI_POW_CUDA_ARCH=compute_90 AI_POW_CUDA_CODE=sm_90`, `--features
gpu,zk-gpu`; sha256 `0590b4dd…0bc136b6b`). The pod built `miner.jam` with
hoonc at the pin, and its sha256 `1f6c1f32…387c0e` matched the local jam.
The registry verified every win and ranked the run.

**What this row is not.** An H100 is compute capability 9.0. Upstream's
open-source CUDA V5 search runs on it, so the ZK row is a real figure,
comparable with the consumer cards. Upstream's #159 dense-peak kernel, the
production AI kernel, runs only on CC 12.0 and refused this card (below). So
this row does **not** show what the large AI fleets run, and it does not
answer whether datacentre GPUs set AI difficulty.

## The row

| card / host | statement | run | calibrated | sustained | verified | luck | verified ÷ calibrated | at luck 1 | proof | tail | host CPU (quota) | pod $/h | wall | NOCK/day |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|
| H100 NVL | ZK v5 | zk 9 | 9.71 MH/s | 9.53 | **11.74** | 1.34 | 1.21 | 0.92 | 33.7 s | 57.7 s | EPYC 9654 (16.15) | 2.59 | 13.0 min | 14.49 |

Columns as in the 2026-11-13 sweep. Tier 2²⁹, 4 provers, no failed proofs,
rank 2 on the ZK board (behind the RTX 5090, ahead of the 4090). The run was
lucky: 16 wins took 0.75× the expected hashes. At luck 1 it is **8.96 MH/s**.
NOCK/day is the board's figure at 20:23 UTC (ZK difficulty 8.60×10¹³,
reward 1,228.8, NOCK $0.01103).

## Against the consumer cards, in the write-up's terms

From `h100_tables.py` (`tables.md`, `tables.json`). The consumer rows are the
2026-11-13 sweep's. "Write-up snapshot" is the 07:53 UTC 13 November
economics the write-up priced everything at
(`../../econ/2026-11-13-snapshot/`); "today" is `economics.json` here.

| card | calibrated MH/s | verified MH/s | luck | at luck 1 | NOCK/day, write-up snapshot (verified / luck 1) | NOCK/day, today | community $/h | MH/s per $/h (luck 1) | board W | kH/J (luck 1) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| H100 NVL | 9.71 | 11.74 | 1.34 | 8.96 | 13.3 / 10.1 | 14.5 / 11.1 | 2.59 | 3.5 | 309 | 29.0 |
| RTX 5090 | 17.28 | 17.32 | 1.11 | 15.75 | 19.6 / 17.8 | 21.4 / 19.4 | 0.69 | 22.8 | 476 | 33.1 |
| RTX 4090 | 14.69 | 7.66 | 0.55 | 13.44 | 8.7 / 15.2 | 9.5 / 16.6 | 0.34 | 39.5 | 450 | 29.9 |
| RTX 3090 | 6.28 | 6.42 | 1.13 | 5.73 | 7.3 / 6.5 | 7.9 / 7.1 | 0.22 | 26.1 | 308 | 18.6 |

- **The H100 NVL hashes Tip5 at 0.57× a 5090** (luck 1), below a 4090.
  Per rent dollar it is 0.15× a 5090. Per joule of board power it is close
  (0.88×). The ZK search is integer arithmetic: it gets nothing from the
  H100's tensor cores or HBM.
- **It does not pay.** At luck 1 it earns $0.12 a day against $62.16 a day
  of community rent (0.2 %). Owned, at the write-up's 18.16 ¢/kWh, its 309 W
  costs $1.35 a day in board power alone; it breaks even below 1.7 ¢/kWh, or
  above a NOCK price of $0.133 at the snapshot difficulty.
- **Upstream publishes no H100 ZK figure.** #192, the CUDA V5 miner, quotes
  only 8× RTX 5090 (144.36 MH/s) and 8 × RTX 4090 (116.18 MH/s).

## AI on the H100

- **dense-peak `--gpu`: refused, as expected.** Upstream's #159 kernel
  failed its preflight with `peak source session creation failed with CUDA
  status 801` (cudaErrorNotSupported), and tock exited 2 before any mint.
  Nothing was submitted. Log: `h100-nvl/peak-gpu-refusal.live.txt`.
- **canonical-moe `--gpu`: not measured.** The binary has the backend for
  sm_90, but the pod had already overspent the cap (below), so it was taken
  down instead. On every consumer card that path earned 0.002–0.005
  NOCK/day, thousands of times under ZK.
- **Upstream's only H100 figures are for the AI puzzle, in an unmerged PR.**
  #164 (Gemma 4 inference mining, open, last updated 2026-09-22) claims
  159.0 TMAC/s for an H100 80 GB and 258.97 TMAC/s for an H100 SXM in
  "isolated production mining". Neither is measured here. Priced at the
  snapshot AI difficulty, they would earn 5.0–8.2 NOCK/day, still under this
  card's ZK 10.1–13.3.

## The host

- **GPU:** NVIDIA H100 NVL 94 GB (PCI device 0x2321), VBIOS 96.00.74.00.11,
  driver 570.133.20, PCIe gen 5 ×16 (`h100-nvl/nvidia-smi-q.txt`).
- **Power:** the card's default and set limit is 310 W (it allows 200–400 W).
  While grinding it drew 309 W on average, and the software power cap was
  active in 128 of the 134 busy samples. The SM clock ran 1,710–1,785 MHz
  (mean 1,744; maximum 1,785), so the cap cost about 2 % of clock. The
  sustained rate was 1.8 % under calibration.
- **CPU:** AMD EPYC 9654, 192 threads, 1.5 TB RAM, a cgroup v1 quota of
  16.15 CPUs. The ZK run's header says `cgroup_cpu_max none`, because
  `sweep_run.sh` read only cgroup v2's `cpu.max`. It now falls back to v1,
  and the refusal run's header shows the quota. Proofs took 33.7 s, between
  the 4090 host's 19.1 s and the 5090 host's 37.8 s.

## Pods and cost

**$3.13–3.19 against a $2.00 cap: overspent by about $1.15.** The balance
went from $4.928 to $1.794 ($3.13; it has lagged teardown by a few cents
before), and the pod's 73.9 min at $2.59/h come to $3.19. The API then showed
`pods: []` at $0/h.

| phase (UTC) | minutes | $ at 2.59/h |
|---|---:|---:|
| deploy, CUDA check, bootstrap, build, jam (19:09:43–19:21:07) | 11.4 | 0.49 |
| ZK run zk 9 (19:21:07–19:34:07) | 13.0 | 0.56 |
| **idle: the run had finished, unseen (19:34:07–20:22:57)** | 48.8 | 2.11 |
| dense-peak refusal, fetch, teardown (to 20:23:37) | 0.7 | 0.03 |

- **Why it overspent.** The local watcher polled the pod for the run's
  `exit` line. The pod's command wrapper runs under `set -euo pipefail`, and
  the first probe read the run's `.tlog` file, which `sweep_run.sh` merges
  and deletes at exit. Once the run ended, that probe failed, the poll exited
  before printing the `exit` line, and the watcher waited out its full hour.
  No limit tied pod lifetime to the cap. The canonical-moe run was dropped
  rather than spend more.
- **Stock.** No community H100 of any variant could be deployed from 17:43 to
  19:09 UTC: about 330 attempts across PCIe ($1.99/h), NVL ($2.59/h) and SXM
  ($2.69/h). Secure cloud had PCIe at $2.89/h and SXM at $4.39/h. The NVL was
  the first community card offered.
- **Prep.** `gpu_pod.py up` now runs the CUDA check before the bootstrap.
  This host passed (driver 570.133.20). The tock build took 2 min 40 s on 16
  CPUs; hoonc and the jam took about 10 min and were the critical path.

## Files

- `h100-nvl/zk-gpu.json`: tock's `--report`, with calibration and the per-win
  timeline.
- `h100-nvl/zk-gpu.live.txt` and `zk-gpu.samples.txt`: the host header plus
  timestamped log, and nvidia-smi with CPU ticks every 5 s
  (`scripts/sweep_run.sh`).
- `h100-nvl/peak-gpu-refusal.live.txt`: the dense-peak `--gpu` refusal.
- `h100-nvl/nvidia-smi-q.txt`: power and clock detail, taken at 19:22 during
  the run's calibration.
- `h100-nvl/pod.json`: the pod's $/h.
- `summary.json` and `summary.md`: `scripts/sweep_summary.py` over this
  directory.
- `h100_tables.py`, `tables.md` and `tables.json`: the comparison above.
- `board-zk.json`, `board-ai.json`, `economics.json` and `fetched_at.txt`:
  the board and its economics at 20:23 UTC.
