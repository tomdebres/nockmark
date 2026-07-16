# M3 announcement drafts (Tom posts these manually)

Pre-flight checklist (human):
- [ ] **URGENT (before Fri Jul 18, 12:25 +01:00):** the Railway `/data` volume is pending deletion (an M3-deploy wipe attempt could not be completed — the CLI token lacked ssh scope, and `volume delete` turned out to be a scheduled soft-delete that blocks detach/re-add). Run `railway login`, then in the dashboard cancel the volume deletion — or attach a fresh `/data` volume and re-run `tock bench --submit`. Until resolved, a redeploy/restart after Friday would lose the leaderboard.
- [ ] After restoring the volume: optionally wipe the old M2 row (run `railway ssh "rm -rf /data/*"` then restart, then reseed) — it now displays its honest server-window rate, so keeping it is also fine.
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
