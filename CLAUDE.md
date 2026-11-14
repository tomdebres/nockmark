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
caches) and `registry/.data.roswell/` (3–8 GB each). None of it belongs in a
worktree. `.data.roswell` is the **roswell verifier's** NockApp scratch, made in
whatever directory the registry runs from; it is not registry state. The
registry kernel's own state (event log + PMA) lives in `--data-dir`
(`./registry-data` by default, `/data` on Railway).

- Registry node state and benchmark output go in **`~/data/nockmark-runs/<name>/`**,
  never inside a checkout. The six pre-consolidation copies (m4–m6, 32 GB of
  PMA) were deleted on 2026-11-14 to free disk (Tom's call). The live registry
  had moved past their formats, and step 2 had already used them once for its
  boot test. For prior registry state in today's (ed1edfcc) format, copy
  `m7p-local/registry-data` or `m7s4-local/registry-data` (25 MB each), or
  write a fresh dir through the current binary. A `.data.roswell` taken from a
  crate directory is verifier scratch, not registry state, and boots under no
  registry binary.
- Each registry test parks a NockApp data dir in `$TMPDIR` on purpose
  (`std::mem::forget`). `./verify` runs the tests under a TMPDIR of its own
  and removes it on exit. A bare `cargo test` in registry/ still leaves
  `.tmpXXXXXX` dirs behind (about 25 MB each), so delete them afterwards.
- `target/` and `.data.roswell/` are gitignored; keep them so. `tock/assets/*.jam`
  are regenerable via `tock/setup-bench.sh` — don't commit them.
- A worktree must stay cheap to create and delete. If it's carrying gigabytes,
  something that should be in `~/data/` leaked into it.

## Naming

Name the worktree after the task and the branch after the worktree. A branch
called `tomdebres/m6-canonical-moe` in a worktree named `m6-phase-b` tells the
sidebar nothing — keep them the same.
