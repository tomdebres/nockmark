# M3 announcement drafts (Tom posts these manually)

Pre-flight checklist (human):
- [ ] **Production `/data` is ephemeral until the old volume is replaced — do not announce before then.** Full story: an M3-deploy wipe attempt soft-deleted `nockmark-registry-volume` (reaper fires **Fri Jul 18, 12:25 +01:00**); a staged detach then landed with a redeploy, so containers get an ephemeral `/data` (verified over ssh: no mount, overlay fs) while the control plane phantom-claims attachment — which blocks `railway volume add`, and the pending-deletion record is immovable via CLI/API (detach no-ops, delete is idempotent, GraphQL mutations return Not Authorized). CLI auth itself is fixed (browserless login 2026-07-16; ssh works). **Two ways out, either is fine:**
      1. **Do nothing until Friday 12:25** — the reaper frees the record; then (Claude can do all of it): `railway volume add -m /data` → `railway redeploy --yes` → reseed → verify a row survives one more redeploy.
      2. **Before Friday, desktop dashboard** → project canvas → the volume block → cancel the pending deletion (restores it with the M2 seed + first M3 run) → redeploy; then wipe/reseed as desired.
      Reseed one-liner (M1 Mac, usual PATH/RUST_MIN_STACK exports):
      `cd tock && ./target/release/tock bench --kernel assets/miner.jam --submit https://nockmark-registry-production.up.railway.app`
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
