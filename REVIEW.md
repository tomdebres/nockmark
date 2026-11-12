# Review

What the bug reviewer (`.github/workflows/claude-code-review.yml`) checks a
nockmark PR against. Nockmark publishes mining rates that "nobody can inflate":
the registry issues the challenge, verifies every proof itself and times the
run on its own clock. A bug that lets a number onto the board unchecked breaks
the one claim the project makes.

## What failure looks like here

- **A number the registry didn't check.** A rate, count, timing or tier taken
  from the client instead of computed from verified work and the server's
  clock; a proof, hash or AI certificate accepted without its verifier; a
  check that fails open (an error or a missing field read as a pass).
- **A challenge used twice or out of its lane.** A challenge must be pending,
  unexpired and unused, re-checked under the store lock at commit, and spent
  once; a nonce or win counted twice; a run accepted for another track,
  statement or tier than the one it was issued for.
- **Consensus drift from the pin.** The nockchain crates are git deps pinned by
  `rev`. A re-pin bumps every `rev` in `tock/Cargo.toml` and
  `registry/Cargo.toml`, `NOCKCHAIN_PIN` (`tock/src/miner.rs`), the
  Dockerfile and `scripts/gpu_pod.py` together, and `deploy/assets/*.jam` are
  built at that rev. A mismatch verifies against the wrong rules.
- **Registry state that won't load.** The kernel's event log and PMA
  (`--data-dir`, `/data` on Railway) outlive every deploy. A change to
  `registry.jam` or the kernel state must still boot the existing state. Run
  stores append only through `registry/src/jsonl.rs`.
- **A public endpoint that does unbounded work.** The registry is on the open
  internet: a body over the 4 MiB limit, a proof count, tier or attempts value
  a client can make arbitrarily large, verifier time spent before the cheap
  checks, or the per-IP limiter bypassed.
- **Earnings that mislead.** `/economics` publishes inputs and estimates: a
  wrong unit (Anthropos-era), reward, miner share or block time is a wrong
  number on the page. A code default mirrored in an env var outlives the fork
  that changed it.
- **Secrets in the open.** `NOCKMARK_ECON_API_KEY` and any Railway or GitHub
  token never in logs, responses, the board or the repo.
- **The shared Mac's disk.** Tests and scripts leave nothing large in the
  checkout or `$TMPDIR`: `./verify` runs the cargo tests under its own
  TMPDIR, removed on exit; benchmark output goes in `~/data/`.

## Findings

The reviewer reports bugs only: behaviour that is wrong, a crash, lost or
corrupted data, a security hole, or a rule above broken. Style, naming, wording
of comments, refactors and missing tests are not findings and are dropped.

- **Important**: a bug this PR introduces that would reach the board, the
  registry's state or a deploy. Blocks the merge until fixed.
- **Nit**: a real bug, but minor or unlikely. Reported, never blocks.
- **Pre-existing**: a bug in code the PR didn't change. Code already on main
  never blocks.

Round 1 reviews the whole diff. Later rounds review only the commits since the
last reviewed one, and check whether the open Important findings were fixed.
The cap is for rounds that don't converge: a push after round 3 that still left
an Important finding open gets "round cap reached: rethink the approach", label
`review-cap`, and the check stays red. A reviewer that can't run (token,
outage, timeout, turn limit) passes with a warning, so an outage never blocks
every merge; it fails only on findings. `scripts/review-round.mjs` does the
bookkeeping; it is devkit's, copied.
