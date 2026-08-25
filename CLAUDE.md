# nockmark — working notes

Trustless STARK proving benchmark registry. Design spec and milestones in
`docs/`; `docs/superpowers/` and `docs/writeups/` hold the deep material.

The global `~/.claude/CLAUDE.md` carries the worktree doctrine and the
parallelism ladder — this file is only the nockmark-specific deltas.

## Worktrees here — the anti-pattern this repo already hit

On 2026-08-25 nockmark was consolidated from 25 stacked branches + 8 in-repo
worktrees (150 GB) down to `main` (= `origin/main`) plus one live worktree.
Those 8 worktrees were being used as **durable milestone checkpoints**
(`m4-econ-autorefresh` … `m7-econ`) — that is exactly the mistake. A worktree
is scratch space for a task in flight, not a place to keep a milestone. Git
history keeps milestones; the worktree gets torn down.

- **New work branches from `main`** in a fresh Orca worktree
  (`orca worktree create --name <task> --no-parent --base-branch main`), and is
  removed (`orca worktree rm --force`) within days of merge. `main` fast-forwards
  and never diverges.
- **Never recreate per-milestone worktrees.** One task, one branch, one
  worktree, disposable.

## Big outputs live outside the checkout

The bulk was never source — it was `registry/target` and `tock/target` (cargo
caches) and `registry/.data.roswell/` (the registry node's event-log SQLite +
PMA checkpoints, 3–8 GB each). None of it belongs in a worktree.

- Registry node state and benchmark output go in **`~/data/nockmark-runs/<name>/`**,
  never inside a checkout. The preserved pre-consolidation state is parked there
  now — copy from it if a run needs prior registry state.
- `target/` and `.data.roswell/` are gitignored; keep them so. `tock/assets/*.jam`
  are regenerable via `tock/setup-bench.sh` — don't commit them.
- A worktree must stay cheap to create and delete. If it's carrying gigabytes,
  something that should be in `~/data/` leaked into it.

## Naming

Name the worktree after the task and the branch after the worktree. A branch
called `tomdebres/m6-canonical-moe` in a worktree named `m6-phase-b` tells the
sidebar nothing — keep them the same.
