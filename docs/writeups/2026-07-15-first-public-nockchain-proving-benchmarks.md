# First public Nockchain proving benchmarks

*Tom de Bres — 2026-07-17*

Nockchain is a zkPoW chain: miners earn NOCK by producing STARK proofs of
Nock execution, and the fastest prover wins. Which makes it strange that the
most-asked question in every mining channel — **"what hardware proves
fastest?"** — has never had a public, verifiable answer. Pool dashboards show
members their own rates; everything else is Discord folklore.

Here are, as far as I know, the first published cross-hardware Nockchain
proving benchmarks, produced with a harness that runs the **real mainnet
prover** — the actual miner kernel a mining node executes — standalone, on a
fixed nonce-parameterized workload, so numbers are comparable across
machines.

## Results

Workload: proof version 2, pow-len 64 (mainnet parameters), miner kernel
compiled from nockchain commit `31b8a015`
(kernel jam `sha256:2e762b17…` — every result records this, so any two
numbers with the same hash proved the exact same workload).

| hardware | 1-thread s/proof | 1-thread proofs/s | all-cores proofs/s | all-cores proofs/day |
|----------|-----------------:|------------------:|-------------------:|---------------------:|
| Apple M1 (4P+4E, 8 GB, macOS) | **20.5** | 0.048 | 0.153 (4 threads) | ~13,200 |
| AWS Graviton4, c8g.4xlarge (16 cores) | 30.4 | 0.033 | 0.514 (16 threads) | ~44,400 |
| AWS Graviton4, c8g.8xlarge (32 cores) | 30.7 | 0.033 | 1.020 (32 threads) | ~88,100 |
| AWS Graviton4, c8g.16xlarge (64 cores) | 30.6 | 0.033 | **1.977** (64 threads) | **~170,800** |
| AMD EPYC 9R14, c7a.4xlarge (16 cores) | 34.0 | 0.029 | 0.465 (16 threads) | ~40,100 |
| Intel Xeon 8488C, c7i.4xlarge (8 cores / 16 SMT) | 30.1 | 0.033 | 0.294 (16 threads) | ~25,400 |

Proof artefacts are ~116 KB jammed; verification of a proof takes ~0.5 s —
the prove/verify asymmetry that makes zkPoW work.

## Three findings

**1. A laptop core beats a server core — by a lot.** The 2020 Apple M1
proves 20.5 s/proof single-threaded; AWS's newest Graviton4 takes 30.4 s and
AMD's EPYC 9R14 (Zen 4c) 34.0 s. Proving is dominated by sequential field
arithmetic and hashing inside one kernel: high IPC and clock win, core count
doesn't help a single proof. Modern high-clock desktop CPUs (Ryzen 9,
M-series Pro/Max) should be very competitive per-core — benchmarks welcome.

**2. Throughput scales almost perfectly with physical cores — but
hyperthreading is nearly worthless.** Mining runs one independent proving
kernel per thread, and it shows: 16 Graviton4 cores give 15.6× the
single-core rate, 32 cores give 31.3×, and 64 cores give 60.5× — two
proofs per second from one machine, with per-proof latency rising only
~2–5% under full load. SMT is the exception: on the Intel
box, doubling from 8 threads (one per physical core) to 16 SMT threads
added just +11% throughput while per-proof latency ballooned from 30 s to
54 s — the prover saturates each core's execution units, leaving nothing
for a sibling hyperthread. Size your mining threads to physical cores;
for proofs/day, physical cores × per-core speed is an excellent model.

**3. For cloud miners: Graviton4 beat EPYC on this workload.** Faster per
core, faster in aggregate, and cheaper per hour (c8g vs c7a). If you're
renting, ARM is currently the better proofs-per-dollar on AWS — worth
re-checking as instance types evolve.

Reproducibility: per-proof spread within a run is under ±1% on the
dedicated cloud boxes and ±3% on macOS; the M1 numbers reproduced across
two sessions within 5%.

## What this means in NOCK

Your expected reward rate is simply your share of network proving:

```
NOCK/day ≈ (your rate ÷ network rate) × daily emission
```

Emission today: the chain is past the Aletheia activation (we observed
pool work at height ~105,300), so blocks pay **2,048 NOCK** (2^27 nicks);
with ASERT retargeting at its 150-second ideal block time that's ~576
blocks/day — matching the ~1,150 blocks the chain actually produced in
the two days between drafting and publishing this — for ≈ **1.18M
NOCK/day** network-wide, about $22k/day at NOCK ≈ $0.019.

Here the trouble starts: **the "network rate" is not published in a unit
you can verify.** Nockchain's own year-in-review quotes ~3M "proofs/s".
Interpreted as full STARK proofs — the thing this article benchmarks —
that would mean ~100 million server cores securing an $80M-cap chain,
which is absurd. Pool software reports in the same inflated unit (see the
GPU postscript below), so the honest reading is that "proofs/s" in the
wild counts *PoW attempts or sub-proof work units*, and CPU proofs/day
cannot be converted to NOCK/day using any publicly verifiable number.
What can be said with confidence:

- **CPU mining earns effectively nothing.** Whatever the unit, a machine
  producing on the order of one work-unit per 30 s competes against a
  network producing millions per second. Even the 64-core box's share
  rounds to well under a cent per day.
- **Consumer GPUs clear their power bill — barely, and only in the
  pool's own unit.** Measured in that unit (which is what pools pay on),
  an RTX 4090 at ~334 "p/s" earns ~334/3M × 1.18M ≈ **131 NOCK/day ≈
  $2.50**, against $1.00–1.50/day of electricity at its measured 420 W.
  A halving of the NOCK price or a doubling of network rate puts it back
  to a coin-flip.

The proper conversion — network rate derived from on-chain difficulty,
in the same STARK-proof unit this article measures — is what the Nockmark
registry's [`/economics`](https://nockmark-registry-production.up.railway.app/economics)
endpoint now reports (difficulty operator-refreshed from the explorer
until a public JSON feed exists). Until the wild-unit numbers reconcile
with it, treat every NOCK/day calculator with suspicion.

## A GPU postscript

Public GPU miners exist (GoldenMiner's closed-source CUDA prover,
NockPool's GPU prover), so we measured one: GoldenMiner v0.4.3 on a rented
RTX 4090, mining live pool work for 15 minutes at 100% GPU utilization and
~420 W. It self-reported a steady **326–334 "p/s"** — a number produced by
a closed binary, in an unverifiable unit, that does not square with any
plausible STARK-on-GPU speedup over the open CPU prover (typical published
gains elsewhere are 10–50×, not the ~10,000× a naive unit reading implies).
That is not an accusation — it is an illustration. The only proving
numbers for this chain that anyone can independently verify are the CPU
benchmarks in the table above, because the workload, inputs, and artefacts
are all public and replayable. Making *verified* numbers the norm — for
GPUs too, once their provers can be driven with challenge inputs — is the
point of the registry this work feeds into.

## Method (and why you can trust the numbers)

The prover isn't a Rust library — it's a Hoon computation running in a
NockVM kernel accelerated by Rust jets. The bench harness (`tock`) boots the
same miner kernel a mining node uses, one instance per thread, and pokes it
with `[version header nonce target pow-len]` exactly as the node's mining
driver does. Three choices make runs comparable and hard to game:

- **Real workload:** the mainnet miner kernel at a pinned commit, fingerprinted
  by sha256 in every result. No simplified stand-in.
- **Nonce-parameterized inputs:** each proof's nonce is derived from a seed by
  a versioned rule (`fnv1a-splitmix64-v1`), so inputs can't be precomputed
  and no two proofs in a run share an input.
- **Difficulty-independent timing:** target is set to the maximum, so every
  attempt completes and produces its proof; we time the proving computation,
  not the lottery.

Caveats: hardware descriptors are self-reported (auto-detected); these
timings are *lower bounds on latency, not audited claims*. The
trust-minimized version — challenge-response benchmarking, where the
registry issues the nonce and verifies every submitted proof server-side —
is now live; see below.

## Reproduce it / what's next

The harness is a small Rust binary that needs a nockchain checkout and two
compiled kernel jams; a provisioning script does a fresh Linux box
end-to-end in ~15 minutes. Run it, and send me your numbers — especially
modern desktop CPUs and big-core-count servers.

Harness, provisioning script, raw result JSONs, and methodology docs:
**https://github.com/tomdebres/nockmark** (`tock/` and `bench-results/`).

These benchmarks were milestone one of **Nockmark**. The registry itself
is now live: submissions are *cryptographically verified* — you fetch a
challenge nonce, prove against it, submit the proofs, and the registry
verifies every STARK and computes your rate from server-side elapsed
time. A leaderboard that cannot lie, on the chain whose whole thesis is
verifiable computation:

- Leaderboard: **https://nockmark-registry-production.up.railway.app/leaderboard**
- Submit your machine:
  `tock bench --kernel assets/miner.jam --submit https://nockmark-registry-production.up.railway.app`
