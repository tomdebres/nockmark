# Contributing

## Runs

Runs from hardware not yet on the board are the most useful contribution:
datacentre GPUs above all (an H100 on the AI puzzle, other H100 variants, the
B200), but any card or CPU the board lacks. A run needs no pull request. Build
`tock`, run `zk-bench` or `ai-bench` with `--submit https://nockmark.xyz`, and
the registry ranks it once its proofs verify. The [README](README.md) and
[tock/README.md](tock/README.md) have the commands.

If a run fails, or its rate looks wrong for the hardware, open an issue with
the command, the client's output and the card or CPU.

## Code

### Building

- Run cargo from inside `tock/` or `registry/`, never with `--manifest-path`
  from elsewhere. Cargo reads `.cargo/config.toml` from the directory it runs
  in, and the registry's sets `KERNEL_JAM_PATH`; without it the build fails.
- The toolchain is pinned (`nightly-2026-04-03`) and the build needs `protoc`.
  [tock/README.md](tock/README.md) has the prerequisites and the one-time
  kernel-jam build.
- The nockchain crates are git dependencies pinned to one `rev`. A re-pin
  bumps every `rev` together, with `NOCKCHAIN_PIN` in `tock/src/miner.rs`.

### Checks

- `./verify --fast`: syntax checks, under a second.
- `./verify`: the fast checks, then the tock and registry test suites. It
  builds both crates into one `./target` and takes about 7 minutes warm; a
  cold run needs about 12 GB of free disk. On Linux the registry tests need
  `sudo sysctl -w vm.overcommit_memory=1` (the verifier kernel maps a large
  stack up front).

Run `./verify` before you open a pull request, and say in it what you ran and
what came back. CI runs the same `./verify`.

### Pull requests

- CI on a pull request from a fork runs only after the maintainer approves
  the workflow run, so expect a short wait before the first checks appear.
- A pull request from outside also needs the maintainer's approval of its
  head commit before it can merge (the "Risk tier" check), whatever it
  touches.
- Pull requests land by rebase: keep each commit one meaningful change, with
  the why in its message.
- Security bugs go through [SECURITY.md](SECURITY.md), not a pull request.

## Licence

Unless you say otherwise, anything you submit for inclusion is dual-licensed
under MIT or Apache 2.0, as the project is, with no further terms.
