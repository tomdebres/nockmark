# Which puzzle should this card mine?

*Tom de Bres — 2026-11-13*

Nockchain now has two mining puzzles, and a block goes to whichever is solved.
Both pay the miner the same 1,228.8 NOCK. So anyone with a GPU has one
practical question: which puzzle should it work on?

**Short answer: ZK, on every machine we measured on both puzzles.** An RTX
5090 earns about 2.7× more on ZK than on mainnet's production AI kernel,
which of these cards only the 5090 can run. A 3090 or 4090 earns thousands
of times more on ZK, because the only AI path open to those cards is the
generic one. And at today's price none of it pays: a 5090 on ZK earns about
$0.24 a day, against $16.56 a day to rent one, or an estimated $2 a day of
electricity to run one you own.

Every measured rate below was **verified by [nockmark.xyz](https://nockmark.xyz)**:
the registry issued the challenge, checked every proof and certificate
itself, and timed the run on its own clock. Nothing here is a vendor or
client figure; the luck-1 figures are derived from those runs.

## The two puzzles, in one paragraph each

**ZK.** Since block 147,500 (Anthropos) a ZK attempt is one **Tip5** hash
(Tip5 is Nockchain's STARK-friendly hash function) of a small puzzle object
built from the block commitment and a nonce. A miner hashes nonces until one
falls under the target, and only then builds the full STARK proof for that
nonce. So the ZK rate is Tip5 hashes per second. Upstream ships an
open-source CUDA miner for it.

**AI.** The AI puzzle is INT8 matrix multiplication: grind matmul tiles, hash
each result, and win when a hash clears the target; the win then needs a
recursive STARK certificate of the matmul. The unit is the **MAC**
(multiply–accumulate, one INT8 multiply and add). Mainnet's production shape
is `dense-peak`, ground by upstream's #159 CUDA kernel, which runs only on
compute capability 12.0: of the cards here, only the RTX 5090. The others can
mine AI on a GPU only through the generic `canonical-moe` path.

Consensus compares the two at a fixed exchange rate of **14,285,714
MAC-equivalents per Tip5 hash**.

## The answer, per card

Verified rates from the live board, one run per card, all with the pipelined
client (below):

| card | ZK: verified Tip5 hashes/s | NOCK/day | best AI path: verified MAC-eq/s | NOCK/day | ZK pays |
|---|---:|---:|---|---:|---:|
| RTX 5090 | 17.3 M | **19.6** | dense-peak, 2.34×10¹⁴ | 7.39 | **2.7×** |
| RTX 4090 | 7.66 M¹ | **8.67** | canonical-moe, 1.38×10¹¹ | 0.0044 | 2,000× |
| RTX 3090 | 6.42 M | **7.28** | canonical-moe, 6.22×10¹⁰ | 0.0020 | 3,700× |
| M1 Max (CPU, 10 cores) | 237 k | **0.27** | canonical-moe, 1.93×10⁸ | 0.000006 | 44,000× |
| Threadripper PRO 5995WX (CPU, 17.9-core quota) | not measured | — | dense-peak, 3.13×10¹⁰ | 0.0010 | — |

¹ An unlucky run: 16 wins took 1.81× the expected hashes. A replay showed
it was luck, not a missed winner; see [the 4090 row](#the-4090-row).

At k = 16 wins a run lands within about ±25 % of its machine's true rate by
luck alone, so the same rows corrected to **luck 1** (the expected work at the
run's own sustained rate, plus its measured tail):

| card | ZK at luck 1 | NOCK/day | AI at luck 1 | NOCK/day | ZK pays |
|---|---:|---:|---:|---:|---:|
| RTX 5090 | 15.7 MH/s | 17.8 | 1.94×10¹⁴ MAC-eq/s | 6.13 | 2.9× |
| RTX 4090 | 13.4 MH/s | 15.2 | 1.23×10¹¹ | 0.0039 | 3,900× |
| RTX 3090 | 5.73 MH/s | 6.49 | 5.68×10¹⁰ | 0.0018 | 3,600× |
| M1 Max | 141 kH/s | 0.16 | 1.78×10⁸ | 0.0000056 | 28,000× |

NOCK/day is rate × 86,400 ÷ difficulty × reward, at the values
[nockmark.xyz/economics](https://nockmark.xyz/economics) read from
[nockscan](https://nockscan.com/api/v1/mining-metrics) at **07:53 UTC on
13 November 2026**:

- ZK difficulty 9.37×10¹³ Tip5 hashes per block (2^46.41);
- AI difficulty 3.35×10²¹ MAC-equivalents per block (2^71.51);
- reward 1,228.8 NOCK to the miner (80 % of 1,536);
- NOCK $0.01222 ([CoinGecko](https://www.coingecko.com/en/coins/nockchain)).

Both difficulties move every block, so treat the earnings as a snapshot. The
ratio between the puzzles moves less, because it depends on how the two
difficulties move against each other.

### What that means for each card

- **RTX 5090: ZK, by about 2.7×** (2.9× at luck 1). It is the one consumer
  card that can run mainnet's AI kernel, and that kernel is the right AI path
  for it: 2.34×10¹⁴ MAC-eq/s against 1.51×10¹¹ on `canonical-moe`. ZK still
  wins. Even at upstream's own #159 figure of 335 TMAC/s, dense-peak would earn
  10.6 NOCK/day, still under ZK's 17.8–19.6.
- **RTX 4090 and RTX 3090: ZK, by three orders of magnitude.** Upstream's peak
  kernel refuses anything but compute capability 12.0, so these cards' only
  GPU route into the AI puzzle is `canonical-moe`, at 0.002–0.004 NOCK/day.
- **CPUs: ZK.** An M1 Max earns 0.16–0.27 NOCK/day on ZK (luck 1 and
  verified), against 6×10⁻⁶ on `canonical-moe`. A 17.9-core x86 pod on
  `dense-peak` earned 0.001 NOCK/day; its ZK rate was not measured.

## Why ZK wins

The two puzzles share one reward but not one market.

| | ZK | AI |
|---|---:|---:|
| network rate (nockscan, 07:53 UTC) | 1.87×10¹¹ Tip5 hashes/s | 1.57×10¹⁹ MAC/s |
| same, in Tip5-hash equivalents | 1.87×10¹¹ | 1.10×10¹² |
| share of all work | 15 % | 85 % |
| share of blocks (ideal 500 s vs 214 s) | 30 % | 70 % |
| work per block, Tip5-hash equivalents | 9.37×10¹³ | 2.35×10¹⁴ |
| RTX 5090s that would match the network (our verified rates) | about 10,800 | about 67,000 |

An AI block costs about **2.5×** the consensus work of a ZK block for the same
reward. A 5090 on dense-peak does 1.35×10⁷ MAC per Tip5 hash it would do on
ZK, within 6 % of the exchange rate, so it earns almost exactly that 2.5× gap
less on AI. A 3090 or 4090 has no fast AI path at all.

AI difficulty is set by much larger fleets than ZK's: the AI network equals
about 67,000 RTX 5090s running upstream's own peak kernel, six times the ZK
network in the same cards. Whoever supplies that work is willing to be paid
less per unit of it, most plausibly because their hardware does INT8 matmul
far more cheaply per MAC than a consumer card. We have not measured
datacentre GPUs, so that last step is inference, not a result.

## Does it pay? No

At today's price nothing on the board covers its costs.

| card | rent, community $/h | rented, $/day | ZK earns, $/day (verified / luck 1) |
|---|---:|---:|---:|
| RTX 5090 | 0.69 | 16.56 | 0.240 / 0.218 |
| RTX 4090 | 0.34 | 8.16 | 0.106 / 0.186 |
| RTX 3090 | 0.22 | 5.28 | 0.089 / 0.079 |

Rented, a 5090 earns about 1.4 % of its hire.

**Owned hardware: an estimate.** Electricity is the only running cost counted
here, and it is estimated, not metered:

- **Power:** the card's own board power from `nvidia-smi`, averaged over the
  sweep's 5 s samples while the GPU was at least 90 % busy on ZK: 5090 476 W
  (500 W cap), 4090 450 W (450 W cap), 3090 308 W (310 W cap). The rest of the
  host is not counted, so the true cost is higher.
- **Tariff:** 18.16 ¢/kWh, the U.S. average residential price for August 2026
  ([EIA Electric Power Monthly, Table 5.6.A](https://www.eia.gov/electricity/monthly/epm_table_grapher.php?t=epmt_5_6_a),
  released 23 October 2026).

| card | kWh/day | electricity, $/day | ZK earns, $/day (luck 1) | breaks even below | or above a NOCK price of |
|---|---:|---:|---:|---:|---:|
| RTX 5090 | 11.4 | 2.07 | 0.218 | 1.9 ¢/kWh | $0.116 |
| RTX 4090 | 10.8 | 1.96 | 0.186 | 1.7 ¢/kWh | $0.129 |
| RTX 3090 | 7.4 | 1.34 | 0.079 | 1.1 ¢/kWh | $0.207 |

So an owned card pays for its electricity only below about 2 ¢/kWh, or at
about ten times today's NOCK price at the same difficulty (17 times for a
3090). That ignores the card itself, the host's power, and the fact that a
solo miner at a fraction of a block per day sees long dry spells, not a
smooth daily rate.

## The 4090 row

The 4090's ZK row needed 1.81× the expected hashes for its 16 wins, a
1-in-300 run. That could have meant upstream's CUDA search misses winners on
that card, so we checked. On another 4090 we replayed the run's own
challenge over the whole nonce range it searched (15.6 billion nonces). We
found every winner at a 2²⁰ tier (14,931, against 14,846 expected) and kept
the ones that also met the run's 2²⁹ target. There are exactly 16, and they
are the 16 the run found. On sampled ranges the device matched two CPU
oracles winner for winner (8,229 of them).

So the card missed nothing: that challenge simply held few winners, and any
correct miner would have needed the same work. The row stands. At luck 1
the 4090 does 13.4 MH/s on ZK, about 15 NOCK/day. Details:
[`bench-results/gpu/2026-11-13-4090-probe/`](../../bench-results/gpu/2026-11-13-4090-probe/).

## How the numbers are made

- **Trustless.** The registry mints a challenge; the client grinds and proves
  against it; the registry verifies every win before it records anything.
  Each ZK win is a `%5` proof whose puzzle digest the registry re-derives from
  the proof itself, verified twice: by the native Rust verifier and by the
  consensus Hoon verifier. Each AI win is checked by upstream's own consensus
  verifier under mainnet's current rules (`ConsensusBatch`).
- **k = 16 wins per run, at a tier.** A tier is the benchmark's difficulty,
  stated as expected attempts per win, always a power of two. The client picks
  it so that grinding one win takes about two proofs' worth of time. The
  credit is k × the tier's expected attempts, divided by the server's own
  window from mint to commit. No client timing is trusted.
- **Luck.** Wins are a Poisson process, so 16 of them pin a rate to about
  ±25 %. The board shows that band; the luck-1 table above removes it.
- **Pipelined client.** The client proves each win while the GPU keeps
  grinding, as upstream's miner does. On GPU rows the board now recovers
  86–94 % of a card's raw rate at luck 1 (the old serial client: 61–68 %).
  The rest is the last proof, the upload and the two verifiers. The cards'
  raw ZK rates are within 4 % of upstream's own figures for its CUDA miner
  (#192: 4090 14.5 MH/s, 5090 18 MH/s).
- **Current rules only.** Only ZK `%5` rows and AI rows under the hardened
  proof rules are ranked, and only rows from the pipelined client (committed
  after 2026-11-13 00:00 UTC by the registry's clock). Older rows stay listed,
  unranked.

## Caveats

- **One run per card**, on rented community pods. Host CPUs differ (EPYC 7663,
  Threadripper PRO 5995WX, Core i5-14600KF), which moves proof time, though
  the tier rule cancels most of that.
- **Power caps.** The pods ran their cards at 500 W (5090), 450 W (4090) and
  310–330 W (3090). A 5090 at its stock 575 W would hash somewhat faster.
- **The 4090 ZK row was unlucky, not faulty** ([above](#the-4090-row)); its
  luck-1 figure is the fairer one.
- **Not measured:** datacentre GPUs (H100, B200 and the like), which are
  plausibly what sets AI difficulty; upstream's own miners run end to end;
  inference mining (upstream's Gemma PR #164, still unmerged); ZK on x86 CPU
  hosts; and multi-GPU hosts.
- **Earnings are a snapshot** of difficulty and price, and exclude pool fees
  and hardware.

## Links

- The board: [nockmark.xyz](https://nockmark.xyz), and its API at
  [nockmark.xyz/api](https://nockmark.xyz/api) (earnings inputs at
  [/economics](https://nockmark.xyz/economics)).
- Raw results: [`bench-results/gpu/2026-11-13-pipelined-sweep/`](../../bench-results/gpu/2026-11-13-pipelined-sweep/)
  (per-run reports, timestamped logs, GPU samples) and the snapshot and script
  behind every figure here,
  [`bench-results/econ/2026-11-13-snapshot/`](../../bench-results/econ/2026-11-13-snapshot/).
- How it got here: [the M7 plan](../superpowers/specs/2026-11-11-m7-anthropos.md).

Run your own card:

```sh
cd tock
ZK_POW_CUDA_ARCH=89 cargo build --release --features zk-gpu   # 86 Ampere, 89 Ada, 120 Blackwell
./target/release/tock zk-bench --kernel assets/miner.jam --gpu --submit https://nockmark.xyz
```

Build steps are in [tock/README.md](../../tock/README.md). Datacentre GPUs
are the rows this board most needs.
