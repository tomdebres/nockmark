# M3 kickoff — trustless public submissions

Start here (fresh session): design spec `docs/superpowers/specs/2026-07-15-nockmark-design.md`
(§milestones, M3), then the M2 final-review carry-forwards below. Session memory
has the full project state under `nockmark-m0-outcome`.

Where things stand: M0 GO → M1 benchmarks published-ready → **M2 LIVE** at
https://nockmark-registry-production.up.railway.app (Railway, volume-backed,
seeded with one verified M1-Mac run; replay rejection verified in production).

## M3 scope (from the design spec + M2 review carry-forwards)

1. **`tock bench --submit`** — client flow: fetch challenge → prove k → submit
   bundle; keep `--local` working offline. (The seeding flow in the deploy
   runbook is the manual version of this.)
2. **Timing enforcement** — kernel already stores `issued-at`/`submitted-at`;
   reject `elapsed_ms > (submitted_at − issued_at)` and surface server-window
   rates. This is THE trust gap before public submissions (see "Known
   limitations" in `docs/superpowers/specs/2026-07-15-m2-deploy-runbook.md`).
3. **Abuse hardening** — rate limiting (Railway edge or in-app), explicit body
   limits, hardware/prover-version string caps.
4. **k sizing** — bump K_DEFAULT from 2 (spike value) toward the spec's
   "minutes of proving" target (6–10) once submission UX exists.
5. **Announce** — mining community (Discord/Telegram) + publish the M1 write-up
   (`docs/writeups/…`, one read-through by Tom pending).
6. Economics peek (difficulty → NOCK/day) per spec.

Build discipline that worked for M2: superpowers plan → subagent-driven
execution with per-task reviews (ledger pattern in `.superpowers/sdd/progress.md`
on the m0-prover-spike branch). Gotchas doc'd in memory: TempDir-drop SIGABRT,
!Send verifier future, openrsync, hoonc quirks, sandbox SIGKILL on kernel boots.
