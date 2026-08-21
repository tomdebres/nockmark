#!/usr/bin/env python3
"""Drive ONE long-lived RunPod GPU pod for `tock ai-bench --gpu` (M6 B2b).

`runpod_harness` is the API layer — deploy, ssh, terminate, stray sweep — and
is not reimplemented here. What this adds is the thing a *development* session
needs and a one-shot sweep does not: a pod that survives between invocations,
so a compile error costs one `build` (seconds to re-dispatch) instead of a
redeploy (minutes, plus a fresh cargo registry download).

    ./gpu_pod.py up                  # sweep strays, deploy, bootstrap, keep
    ./gpu_pod.py bootstrap           # re-run the bootstrap on the live pod
    ./gpu_pod.py push                # overlay the LOCAL tock/ onto the clone
    ./gpu_pod.py build               # cargo build --release --features gpu
    ./gpu_pod.py run '<shell>'       # anything, with the toolchain on PATH
    ./gpu_pod.py pods                # what is live on the account, if anything
    ./gpu_pod.py down                # terminate + assert nothing is left

The safety trade this makes, stated plainly: `runpod_harness.deploy` registers
an atexit teardown the instant a pod exists, and `up` deliberately unregisters
it so the pod outlives the process. That is the ONLY place teardown is
disarmed, and three things fence it in — the unregister happens last, after
bootstrap, so any failure before it still tears the pod down; every `up`
pre-flight-sweeps `nockmark-` pods, so a leaked pod dies at the next
invocation; and `down` re-checks `list_pods()` and shouts if anything survived.

`push` exists because the branch under development is deliberately unpushed.
The pod clones nockmark at `main` for its layout and history, then this
overwrites `tock/` from the working tree, so what runs on the GPU is exactly
what is on this laptop's disk — no "did I remember to push?" failure mode.
"""

import base64
import io
import json
import os
import sys
import tarfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import runpod_harness as rp  # noqa: E402

# The 3090 is the development GPU: $0.22/hr, Ampere, and every CUDA arch flag
# below follows from that choice. 4090 is compute_89/sm_89, 5090 is
# compute_120/sm_120 — both cost 3-6x more per hour and neither tells you
# anything a 3090 has not already told you about whether the code COMPILES.
GPU_TYPE = os.environ.get("NOCKMARK_GPU_TYPE", "NVIDIA GeForce RTX 3090")
CUDA_ARCH = os.environ.get("AI_POW_CUDA_ARCH", "compute_86")
CUDA_CODE = os.environ.get("AI_POW_CUDA_CODE", "sm_86")

# The image already carries nvcc 12.8 and the driver; only the Rust side and
# the protobuf compiler (nockapp-grpc's build script) are missing.
IMAGE = "nvidia/cuda:12.8.1-devel-ubuntu24.04"

NOCKCHAIN_REPO = "https://github.com/tomdebres/nockchain.git"
NOCKCHAIN_COMMIT = "c8d6b13e"
NOCKMARK_REPO = "https://github.com/tomdebres/nockmark.git"
TOOLCHAIN_DATE = "2026-04-03"
TRIPLE = "x86_64-unknown-linux-gnu"

# tock's path deps read `../../../nockchain` from `<repo>/tock/`, i.e.
# `<repo>/../../nockchain` — so the nockmark checkout must sit exactly two
# levels below the directory that holds nockchain. Same layout the registry
# Dockerfile builds (`/build/nockchain` + `/build/nockmark/<worktree>`).
ROOT = "/build"
NOCKMARK = f"{ROOT}/nockmark/m6-gpu"
TOCK = f"{NOCKMARK}/tock"

STATE = os.path.expanduser(
    os.environ.get("NOCKMARK_POD_STATE", "~/.cache/nockmark/gpu-pod.json")
)

# Everything a remote command needs: the tarball-installed toolchain, nvcc,
# the arch flags upstream's build.rs reads, and the 8 MB stack the ai-pow
# provers want (RUST_MIN_STACK, same as every other Nockmark build).
ENV = (
    "export PATH=/opt/rust/bin:/usr/local/cuda/bin:$PATH; "
    "export CUDA_HOME=/usr/local/cuda; "
    f"export AI_POW_CUDA_ARCH={CUDA_ARCH} AI_POW_CUDA_CODE={CUDA_CODE}; "
    "export RUST_MIN_STACK=8388608; "
    "export CARGO_TERM_COLOR=never; "
)


def load_state():
    try:
        with open(STATE) as f:
            return json.load(f)
    except FileNotFoundError:
        sys.exit("no pod: run `gpu_pod.py up` first")


def save_state(state):
    os.makedirs(os.path.dirname(STATE), exist_ok=True)
    with open(STATE, "w") as f:
        json.dump(state, f)


# Marker the remote script prints as its very last act. It must not appear in
# the script's own source, because a PTY echoes everything typed into it — see
# `remote` below for the full trap.
DONE = "NM_" + "DONE"


def _upload_lines(data: bytes, path: str):
    """Shell lines that reconstruct `data` at `path`, safe for a PTY.

    A PTY in canonical mode discards input past its ~4 KB line buffer, so one
    giant `echo` silently truncates; base64 in ≤1000-byte `printf`s does not.
    scp is not an alternative — OpenSSH's sftp mode against RunPod's proxy is
    the trap the July sweep already paid for.
    """
    blob = base64.b64encode(data).decode()
    chunks = [blob[i:i + 1000] for i in range(0, len(blob), 1000)]
    lines = [f"rm -f {path}.b64"]
    lines += [f"printf %s '{c}' >> {path}.b64" for c in chunks]
    lines.append(f"base64 -d {path}.b64 > {path}")
    return lines


def remote(command, timeout_s=3600):
    """Run `command` on the saved pod, with the build environment loaded.

    The command is **uploaded and run as a file**, never typed at the remote
    shell, and that is not fastidiousness — it is the fix for a failure that
    reported success. RunPod's proxy only offers an INTERACTIVE shell over a
    PTY, and two things go wrong when a multi-line script is fed to one:

      * `set -e` in an interactive shell kills the session at the first
        non-zero status, so the remaining lines never run and their output
        never appears;
      * the PTY echoes the input before the shell consumes it, so
        `runpod_harness.ssh` finds its own `__NM_RC__$?__` sentinel in the
        ECHO, parses `$?` as "not a digit", and returns **rc 0**. A bootstrap
        that executed nothing at all therefore looked like a clean success,
        and only failed three steps later with "rustc: command not found".

    So: upload, run under a fresh non-interactive `bash` (where `set -e`
    behaves), and trust an explicit end marker with the real exit status
    rather than the transport's rc.

    `</dev/null` is the third non-negotiable. The remote shell is being driven
    THROUGH its stdin, so any command that reads stdin eats the lines queued
    behind it — `apt-get` swallowed the end marker and the harness's own
    `exit`, and the session then sat idle at a prompt until the ssh timeout,
    with the work long since finished. Detaching the payload's stdin is what
    makes "the command ended" and "the session ended" the same event.
    """
    state = load_state()
    script = "set -euo pipefail\n" + ENV + "\n" + command + "\n"
    lines = _upload_lines(script.encode(), "/tmp/nm-cmd.sh")
    lines.append(f'bash /tmp/nm-cmd.sh </dev/null 2>&1; echo "{DONE}_$?_"')
    rc, out = rp.ssh(state["host_id"], "\n".join(lines), timeout_s=timeout_s)
    print(out, flush=True)
    # The marker appears twice (echoed input, then real output); the last one
    # is the executed copy, and only it carries a substituted status.
    marker = out.rfind(f"{DONE}_")
    status = out[marker + len(DONE) + 1:].split("_", 1)[0] if marker >= 0 else ""
    if not status.isdigit():
        print(f"[gpu_pod] no end marker (ssh rc={rc}) — treating as failure", flush=True)
        return rc or 1
    return int(status)


BOOTSTRAP = f"""
export DEBIAN_FRONTEND=noninteractive
# DPkg::Lock::Timeout, not a bare apt-get: RunPod's SSH proxy answers via
# its OWN agent, so a shell is reachable while the container's startup
# command is still running its apt-get to install sshd. Racing that lock is
# how the first sweep attempt died on every card —
# "E: Could not get lock /var/lib/dpkg/lock-frontend ... held by process
# 549 (apt-get)" — so wait for it instead. Ten minutes is far longer than
# the ~40 s the startup install takes, and costs nothing when it is free.
apt-get -o DPkg::Lock::Timeout=600 update -qq
apt-get -o DPkg::Lock::Timeout=600 install -y -qq --no-install-recommends \
    git protobuf-compiler build-essential curl xz-utils pkg-config libssl-dev ca-certificates
# Pinned nightly from static.rust-lang.org tarballs. The CUDA image has no
# rustup and installing one would be a second, unpinned source of truth; this
# is the same three-component install the registry Dockerfile does.
mkdir -p /opt/rust /tmp/rust-dl
cd /tmp/rust-dl
for c in rustc cargo rust-std; do
  curl -sSfLO "https://static.rust-lang.org/dist/{TOOLCHAIN_DATE}/${{c}}-nightly-{TRIPLE}.tar.xz"
  tar xf "${{c}}-nightly-{TRIPLE}.tar.xz"
  "./${{c}}-nightly-{TRIPLE}/install.sh" --prefix=/opt/rust --disable-ldconfig >/dev/null
done
rm -rf /tmp/rust-dl
mkdir -p {ROOT}/nockmark
cd {ROOT}
[ -d nockchain ] || git clone --quiet --filter=blob:none {NOCKCHAIN_REPO} nockchain
git -C nockchain checkout --quiet {NOCKCHAIN_COMMIT}
[ -d {NOCKMARK} ] || git clone --quiet --filter=blob:none {NOCKMARK_REPO} {NOCKMARK}
git -C {NOCKMARK} checkout --quiet main
/opt/rust/bin/rustc --version
nvcc --version | tail -1
nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader
"""


def up():
    swept = rp.sweep_strays()
    if swept:
        print(f"[gpu_pod] swept {swept} stray pod(s) before deploying", flush=True)
    pod = rp.deploy(GPU_TYPE, f"{rp.NAME_PREFIX}m6-gpu", image=IMAGE, container_disk=60)
    host = rp.host_id(pod["id"])
    rp.wait_for_ssh_ready(host)
    save_state({"pod_id": pod["id"], "host_id": host, "gpu": GPU_TYPE})
    print(f"[gpu_pod] bootstrapping {pod['id']} ({host})", flush=True)
    if remote(BOOTSTRAP, timeout_s=1800) != 0:
        sys.exit("[gpu_pod] bootstrap failed — pod will be torn down")
    # Last statement in the happy path: everything above still tears the pod
    # down on failure, because the atexit hook is armed until this line.
    rp.atexit.unregister(rp.terminate)
    print(f"[gpu_pod] pod {pod['id']} is UP and will persist; `gpu_pod.py down` to stop billing",
          flush=True)


def push():
    """Overlay the local `tock/` onto the pod's clone."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for member in ("Cargo.toml", "Cargo.lock", "src"):
            path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tock", member)
            tar.add(os.path.abspath(path), arcname=member)
    payload = buf.getvalue()
    print(f"[gpu_pod] pushing {len(payload)} bytes of tock/ to {TOCK}", flush=True)
    sys.exit(remote(
        "\n".join(_upload_lines(payload, "/tmp/tock.tgz")
                  + [f"tar xzf /tmp/tock.tgz -C {TOCK}", "echo pushed"]),
        timeout_s=900,
    ))


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    cmd = sys.argv[1]
    if cmd == "up":
        up()
    elif cmd == "bootstrap":
        # Idempotent, and split out from `up` so a bootstrap that fails
        # halfway can be retried without paying for a fresh pod.
        sys.exit(remote(BOOTSTRAP, timeout_s=1800))
    elif cmd == "push":
        push()
    elif cmd == "build":
        sys.exit(remote(f"cd {TOCK} && cargo build --release --features gpu 2>&1 | tail -60"))
    elif cmd == "run":
        sys.exit(remote(" ".join(sys.argv[2:]), timeout_s=5400))
    elif cmd == "pods":
        pods = rp.list_pods()
        print(json.dumps(pods, indent=2) if pods else "[gpu_pod] no live pods")
    elif cmd == "down":
        state = load_state()
        rp.terminate(state["pod_id"])
        os.remove(STATE)
        time.sleep(5)
        left = rp.list_pods()
        print(json.dumps(left, indent=2) if left else "[gpu_pod] no live pods — clean")
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
