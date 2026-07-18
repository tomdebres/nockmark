# M3 announcement drafts (Tom posts these manually)

Pre-flight checklist (human):
- [x] Production `/data` persistent — volume restored 2026-07-17 (dashboard activity-feed Restore via the email link), original data intact, persistence proven across two redeploys.
- [x] Write-up published 2026-07-17 (with 2026-07-18 GPU sweep table): docs/writeups/2026-07-15-first-public-nockchain-proving-benchmarks.md
- [x] Leaderboard shows both verified runs (original data survived — no reseed needed).
- [x] Econ vars set 2026-07-17 (difficulty 604011175 = 2^29.17, reward 2048); /economics live; refresh difficulty manually from nockblocks.com now and then.
- [x] Domain: https://nockmark.xyz live (Cloudflare → Railway, TLS valid); board page has OG tags for unfurls.
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
`tock bench --submit https://nockmark.xyz`
(setup: https://github.com/tomdebres/nockmark — bare machine to leaderboard
in ~15 min, ~3 min of proving)

Leaderboard: https://nockmark.xyz
Also: the first public cross-hardware Nockchain proving write-up — Apple
M1, Graviton4, EPYC, Xeon, plus a four-GPU pool-unit comparison
(3090/4090/5090/A100):
https://github.com/tomdebres/nockmark/blob/main/docs/writeups/2026-07-15-first-public-nockchain-proving-benchmarks.md

GPU miner authors: the registry verifies any prover that can take a
challenge nonce and emit its proofs. Expose a bench mode and your cards
get verified numbers on the board — the only kind anyone can trust.

Feedback and PRs welcome — especially runs from hardware we haven't seen.

## Telegram (mining groups — shorter)

Nockmark is live: verified Nockchain proving benchmarks. Your rate is
computed from a server-side challenge→submit window over STARK-verified
proofs, so the leaderboard can't be gamed by self-reporting. One command:
`tock bench --submit https://nockmark.xyz`
— repo: https://github.com/tomdebres/nockmark. Post your rig's rate.
