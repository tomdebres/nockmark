# Mining Nockchain after Logos: two puzzles, and one of them pays for itself

*Tom de Bres — 2026-08-22*

In July I published the first cross-hardware Nockchain proving benchmarks.
Since then the chain has hard-forked three times. The last of those, **Logos**
(protocol 016, live at block 126,000), did something no other proof-of-work
chain has done: it added a **second, independent mining puzzle** whose work is
INT8 matrix multiplication — the same primitive that runs model inference.

So "what hardware mines Nockchain fastest?" is now two questions, and the
answers point in different directions. This is what a benchmark registry
looks like when it has to measure both.

Everything below was produced by [nockmark.xyz](https://nockmark.xyz), which
runs the real mainnet miner, issues its own challenges, and verifies every
proof and certificate server-side before a number reaches the board. No rate
here is self-reported.

## What Logos changed

Before Logos, mining Nockchain meant one thing: produce STARK proofs of Nock
execution (**ZK-PoW**). Logos added **AI-PoW** alongside it — grind an INT8
tiled matmul, hash the result, and win if the hash clears a target. Winning
requires a recursive STARK certificate proving you really did the committed
matmul, so it is proof-of-work in the strict sense: the work is verifiable and
cannot be faked.

The two puzzles retarget independently — ZK blocks aim for 214 s, AI blocks
for 500 s, combining to the ~150 s cadence the chain had before — and fork
choice weighs them against each other at a fixed exchange rate of
**25,750,000,000 MAC-equivalents per ZK proof attempt**. That constant is what
makes the two puzzles comparable at all, and it is the unit this write-up
leans on throughout.

## Results

Every figure is a **verified lower bound**: computed by the registry from its
own clock, from certificates it checked, over a challenge it issued. Client
timings are displayed but never ranked.

### ZK-PoW — STARK proving

| hardware | verified proofs/s |
|---|---:|
| AWS Graviton4 (Neoverse-V2, 16c) | **0.219** |
| Intel Xeon Gold 6455B (16c) | 0.219 |
| Intel Xeon Platinum 8488C (16c) | 0.207 |
| AMD EPYC 9R14 (16c) | 0.196 |
| Apple M1 Max (10c) | 0.047 |

These are CPUs, and that is a limitation rather than a finding: Nockchain's
GPU STARK provers are closed source and report rates in their own pool units,
which cannot be checked against a challenge the registry issued. They are
absent from the board rather than taken on trust.

### AI-PoW — INT8 matmul

| hardware | verified MAC-equiv/s | ZK-attempt-equiv/s |
|---|---:|---:|
| NVIDIA RTX 5090 | **143.9 G** | 5.59 |
| NVIDIA RTX 4090 | 99.8 G | 3.88 |
| Apple M1 Max (CPU) | 0.019 G | 0.0007 |

## Four findings

**1. On AI-PoW, one GPU is worth thousands of CPU cores.** The RTX 5090 posts
143.9 GMAC/s against an M1 Max's 0.019 — roughly 7,700×. That is the expected
shape for a matmul workload, and it is the opposite of the ZK result from
July, where a laptop core beat a server core because STARK proving is
dominated by sequential field arithmetic. **The two puzzles reward completely
different machines**, which is presumably the point of having both.

**2. The two puzzles can finally be compared, and AI-PoW dominates.** At the
consensus exchange rate, a 5090 mining AI-PoW does **5.59 ZK-attempt-
equivalents per second**, against 0.219 proofs/s for the fastest 16-core
server CPU on ZK — about **25× the fork-choice weight**. Nobody could state
this before Logos, because there was nothing to compare.

**3. The mining path matters a thousandfold more than the hardware.** This is
the finding I did not expect. Nockchain admits (at least) two ways to mine
AI-PoW. The one benchmarked above grinds the canonical block shape for its own
sake. The other mines *inside real model inference* — a Gemma-4 31B server,
where the mining tickets ride the matmul the host is computing anyway for its
own users. Upstream measures that path at roughly **310 TMAC/s on the same
RTX 5090**: about **1,666×** the canonical shape.

The gap is not a better kernel. It is that in the second case the matmul is
work you were doing regardless, so mining is close to free. The difficulty
anchor confirms which path the protocol expects: at the inference rate about
**119 GPUs** equal the entire AI-PoW network — matching the ~100 GPUs the
Logos design notes anticipate — whereas the canonical shape would need
roughly **198,000**.

**Nockchain's AI-PoW is therefore not designed to reward mining. It is
designed to reward compute that was already doing something useful**, and to
penalise burning a GPU on nothing else. I am not aware of another chain whose
proof-of-work has that property.

**4. Pure-play mining does not pay; dual-use does.** See below.

## What it earns

*(Economics section — filled from live registry figures.)*

## How the numbers are produced

The registry mints a challenge, the client proves or grinds against it, and
the registry verifies every artifact before recording anything:

- **ZK-PoW**: k=8 proofs of a server-issued nonce, each verified by a
  verifier kernel compiled from the same pinned nockchain tree.
- **AI-PoW**: k=16 jackpot wins, each carrying a compact recursive
  certificate. The registry re-derives the entire statement — commitments,
  routing, opened indices, jackpot — from the challenge and ordinal alone. The
  submission carries no statement metadata, so there is nothing to forge.

The ranked rate divides by the **server-observed window**, so it always sits
below a client's own measurement — it includes network time and, on the AI
track, certificate proving. That is deliberate: a published number that cannot
be inflated is worth more than a flattering one.

Two properties keep it honest across machines. Clients pick a difficulty
tier, which cannot inflate a score — an easy tier earns proportionally less
credit against the same fixed proving cost, and a hard one has to actually be
ground out. And each run's grind is sized against its own measured proving
time, so every host is diluted by the same fraction rather than by whatever
its CPU happens to be. Ranked figures reproduce true relative throughput to
**0.3%**; independent runs of the same card on different hosts agree to
**1.4%**.

## Caveats

- **Hardware descriptors are self-reported.** Rates are not.
- **AI-PoW rates are statistical.** Wins are a Poisson process; at k=16 the
  sampling error is ±25%, shown on the board.
- **Earnings are a snapshot.** Difficulty and price both drift, and neither
  estimate accounts for electricity, hardware, or pool fees.
- **The inference-mining figures are upstream's**, not ours. Benchmarking that
  path independently needs 60 GB+ of VRAM and a 31B checkpoint; it is the next
  thing I want on the board.

## Run it yourself

```sh
tock bench --kernel assets/miner.jam --submit https://nockmark.xyz     # ZK
tock ai-bench --statement canonical-moe --submit https://nockmark.xyz  # AI
```

Hardware not yet on the board is the most useful contribution — especially
GPUs, and especially anyone able to run the inference-mining path.
