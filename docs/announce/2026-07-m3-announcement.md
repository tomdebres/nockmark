# M3 announcement drafts (Tom posts these manually)

Pre-flight checklist (human):
- [ ] **URGENT — production has NO persistent volume right now.** During the M3 deploy, a wipe-and-reseed attempt hit a CLI auth wall (`railway ssh`/`volume files` "Unauthorized" — token lacks ssh scope) and `railway volume delete` turned out to be a scheduled soft-delete (fires Fri Jul 18, 12:25 +01:00). The follow-up detach was applied as a staged change on the next deploy: the service now gets an **ephemeral** `/data` (empirically verified — a seeded run survived `railway restart` but was wiped by `railway redeploy`), while `railway volume list` still phantom-claims the volume is attached, which blocks `railway volume add`. **Fix (dashboard):** `railway login`, open the project → volumes, then either cancel the pending deletion and re-attach `nockmark-registry-volume` (its last state still holds the M2 seed + first M3 run), or delete it outright and attach a fresh 5GB volume at `/data`. Then redeploy and reseed (one command, ~3 min):
      `cd tock && ./target/release/tock bench --kernel assets/miner.jam --submit https://nockmark-registry-production.up.railway.app`
      (with the usual PATH/RUST_MIN_STACK exports). Until this is fixed, every redeploy resets the leaderboard — do not announce before it's resolved.
- [ ] Read through docs/writeups/2026-07-15-first-public-nockchain-proving-benchmarks.md (pending since M1) and publish it (repo link is fine)
- [ ] Verify the leaderboard shows the reseeded M3 runs
- [ ] Set NOCKMARK_DIFFICULTY / NOCKMARK_BLOCK_REWARD_NOCK on Railway (current values from https://nockblocks.com) so /economics is live. Note: no public JSON difficulty endpoint was found on NockBlocks (the official Block Explorer API is gRPC-first), so leave NOCKMARK_ECON_URL unset and refresh the value manually for now.
- [ ] Post Discord + Telegram drafts below; ask a mod about pinning

## Discord (Nockchain server, #mining or #ecosystem)

**Nockmark is open: a proving benchmark registry that can't lie.**

"What hardware proves fastest?" now has a trustless answer. Nockmark is a
public registry of Nockchain STARK proving benchmarks where the rates are
cryptographically verified — not self-reported:

- Your machine proves k=8 real mining workloads against a server-issued
  challenge nonce (no precomputing).
- The registry verifies every proof and computes your rate from the
  server-observed clock — the published number is a lower bound nobody
  can inflate, including you.

One command to get on the board:
`tock bench --submit https://nockmark-registry-production.up.railway.app`
(setup: https://github.com/tomdebres/nockmark — bare machine to leaderboard
in ~15 min, ~3 min of proving)

Leaderboard: https://nockmark-registry-production.up.railway.app
Also: first public cross-hardware Nockchain proving benchmarks write-up
(M1/M2/EC2/Graviton numbers) in the repo. Feedback and PRs welcome —
especially runs from hardware we haven't seen.

## Telegram (mining groups — shorter)

Nockmark is live: verified Nockchain proving benchmarks. Your rate is
computed from a server-side challenge→submit window over STARK-verified
proofs, so the leaderboard can't be gamed by self-reporting. One command:
`tock bench --submit https://nockmark-registry-production.up.railway.app`
— repo: https://github.com/tomdebres/nockmark. Post your rig's rate.
