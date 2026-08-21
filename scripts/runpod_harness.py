#!/usr/bin/env python3
"""RunPod orchestration for Nockmark GPU benchmarks.

Safety rules this file exists to enforce, learned the hard way:
  * termination is registered with atexit BEFORE the pod can start billing,
    so every exit path (exception, KeyboardInterrupt, normal return) tears
    the pod down;
  * deploy failures raise a dedicated exception — never SystemExit, which
    a broad `except` will happily swallow along with a successful exit and
    retry into a second live pod;
  * a pre-flight sweep terminates anything left over from a previous run,
    so a crashed session cannot quietly bill for hours.
"""

import atexit
import re
import json
import os
import signal
import subprocess
import sys
import time

API = "https://api.runpod.io/graphql"
NAME_PREFIX = "nockmark-"
PUBKEY = open(os.path.expanduser("~/.ssh/nockmark_runpod.pub")).read().strip()


class RunPodError(RuntimeError):
    """Any GraphQL-level failure. Deliberately not SystemExit."""


def _teardown_on_signal(signum, _frame):
    """atexit does NOT run on SIGTERM, and a killed orchestrator leaves a
    pod billing by the hour. Convert the signal into a normal exit so the
    registered teardown actually fires."""
    print(f"[harness] signal {signum} — tearing down", file=sys.stderr, flush=True)
    sys.exit(128 + signum)


for _sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    signal.signal(_sig, _teardown_on_signal)


def gql(query, variables=None):
    payload = json.dumps({"query": query, **({"variables": variables} if variables else {})})
    # api.runpod.io Cloudflare-403s urllib; curl with a browser UA works.
    proc = subprocess.run(
        ["curl", "-s", "-m", "60", "-X", "POST", API,
         "-H", "Content-Type: application/json",
         "-H", f"Authorization: Bearer {os.environ['RUNPOD_API_KEY']}",
         "-H", "User-Agent: Mozilla/5.0",
         "--data-binary", payload],
        capture_output=True, text=True,
    )
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise RunPodError(f"non-JSON response: {proc.stdout[:200]}")
    if "errors" in data:
        raise RunPodError(json.dumps(data["errors"])[:400])
    return data["data"]


def list_pods():
    return gql("query { myself { pods { id name desiredStatus costPerHr } } }")["myself"]["pods"]


def terminate(pod_id):
    try:
        gql("mutation($input: PodTerminateInput!) { podTerminate(input: $input) }",
            {"input": {"podId": pod_id}})
        print(f"[harness] terminated {pod_id}", flush=True)
    except RunPodError as e:
        # Never mask the original failure with a teardown failure, but do
        # make the leak loud — a pod left running bills by the hour.
        print(f"[harness] WARNING could not terminate {pod_id}: {e}", file=sys.stderr, flush=True)


def sweep_strays():
    strays = [p for p in list_pods() if p["name"].startswith(NAME_PREFIX)]
    for p in strays:
        print(f"[harness] sweeping stray pod {p['id']} ({p['name']}, ${p['costPerHr']}/hr)", flush=True)
        terminate(p["id"])
    return len(strays)


DEPLOY = """
mutation($input: PodFindAndDeployOnDemandInput!) {
  podFindAndDeployOnDemand(input: $input) { id machineId imageName costPerHr }
}"""

POD_STATUS = """
query($podId: String!) {
  pod(input: {podId: $podId}) {
    id desiredStatus
    runtime { uptimeInSeconds ports { ip isIpPublic privatePort publicPort type } }
  }
}"""


def deploy(gpu_type, name, image="nvidia/cuda:12.8.1-devel-ubuntu24.04",
           container_disk=40, attempts=3):
    """Deploy exactly one pod. Retries only on genuine failure."""
    spec = {
        "cloudType": "COMMUNITY", "gpuCount": 1, "gpuTypeId": gpu_type,
        "volumeInGb": 0, "containerDiskInGb": container_disk,
        "minVcpuCount": 8, "minMemoryInGb": 24,
        "name": name, "imageName": image,
        "ports": "22/tcp", "startSsh": True,
        # The key is embedded literally rather than read from $PUBLIC_KEY:
        # RunPod only injects that variable for its own templates, so on a
        # bare CUDA image authorized_keys ends up empty and every ssh
        # attempt fails until the orchestrator times out.
        "dockerArgs": (
            'bash -c "apt-get update -qq && apt-get install -y -qq openssh-server >/dev/null 2>&1 && '
            'mkdir -p /run/sshd /root/.ssh && '
            f"echo '{PUBKEY}' > /root/.ssh/authorized_keys && "
            'chmod 700 /root/.ssh && chmod 600 /root/.ssh/authorized_keys && '
            '/usr/sbin/sshd -D -p 22"'
        ),
    }
    last = None
    for attempt in range(1, attempts + 1):
        try:
            pod = gql(DEPLOY, {"input": spec})["podFindAndDeployOnDemand"]
        except RunPodError as e:
            last = e
            print(f"[harness] deploy attempt {attempt} failed: {e}", flush=True)
            # Community capacity is flaky ("machine does not have the
            # resources"); secure cloud costs more but actually has stock.
            if attempt == attempts - 1:
                print("[harness] falling back to SECURE cloud", flush=True)
                spec["cloudType"] = "SECURE"
            time.sleep(5)
            continue
        if pod is None:
            last = RunPodError("deploy returned null (no capacity)")
            print(f"[harness] deploy attempt {attempt}: no capacity", flush=True)
            if attempt == attempts - 1:
                spec["cloudType"] = "SECURE"
            time.sleep(5)
            continue
        # Register teardown the instant the pod exists, before any work.
        atexit.register(terminate, pod["id"])
        print(f"[harness] deployed {pod['id']} on {gpu_type} at ${pod['costPerHr']}/hr", flush=True)
        return pod
    raise RunPodError(f"all {attempts} deploy attempts failed; last: {last}")


def wait_for_ssh(pod_id, timeout_s=600):
    """Block until the pod publishes a public port 22, return (ip, port)."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        rt = gql(POD_STATUS, {"podId": pod_id})["pod"].get("runtime")
        if rt:
            for p in rt.get("ports") or []:
                if p["privatePort"] == 22 and p["isIpPublic"]:
                    print(f"[harness] ssh up at {p['ip']}:{p['publicPort']} "
                          f"after {rt['uptimeInSeconds']}s", flush=True)
                    return p["ip"], p["publicPort"]
        time.sleep(10)
    raise RunPodError(f"pod {pod_id} never published a public port 22 in {timeout_s}s")


SSH_OPTS = [
    "-i", os.path.expanduser("~/.ssh/nockmark_runpod"),
    "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
    "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=15",
]


_ANSI = re.compile(
    r"\x1b\[[0-9;?]*[a-zA-Z]"   # CSI: colours, bracketed-paste toggles
    r"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"  # OSC: window titles
    r"|\x1b[()][A-Z0-9]|[\x0e\x0f]"        # charset selects
)


def _clean(text):
    """Strip terminal control noise and leftover prompts from PTY output."""
    text = _ANSI.sub("", text)
    lines = [ln.rstrip() for ln in text.split("\n")]
    keep = [ln for ln in lines
            if ln.strip() and not re.match(r"^\S*@\S*:.*[#$]$", ln.strip())]
    return "\n".join(keep)


def host_id(pod_id):
    """The SSH-proxy username: `machine.podHostId`, available immediately,
    before the container has a runtime."""
    return gql("query($podId: String!) { pod(input: {podId: $podId}) { machine { podHostId } } }",
               {"podId": pod_id})["pod"]["machine"]["podHostId"]


def ssh(hid, command, timeout_s=3600, quiet=False):
    """Run a command on the pod via RunPod's SSH proxy.

    Community pods get no public port 22 — only the proxy at
    ssh.runpod.io, which authenticates against the account's registered
    public key. It also *requires* a PTY ("Your SSH client doesn't support
    PTY" otherwise), hence -tt; that in turn means CRLF line endings, so
    output is normalised here rather than at every call site.

    Output comes back over stdout rather than scp: OpenSSH's sftp mode
    silently drops brace expansion, which cost an hour in the July sweep.
    """
    # The proxy serves an INTERACTIVE shell: a trailing command argument is
    # ignored and ssh then blocks on stdin forever. So drive it the way a
    # human would — write the command into the shell, echo a sentinel with
    # the exit status, and exit — then carve the real output back out.
    # A PTY echoes everything typed, so each sentinel appears TWICE: once
    # as the echoed command line, once as real output. `stty -echo` kills
    # most of it and rfind takes the executed copy regardless.
    script = (
        "stty -echo 2>/dev/null\n"
        "PS1=''\n"  # otherwise every command's prompt lands in the capture
        "echo __NM_BEGIN__\n"
        f"{command}\n"
        "echo __NM_RC__$?__\n"
        "exit\n"
    )
    try:
        proc = subprocess.run(
            ["ssh", "-tt", *SSH_OPTS, f"{hid}@ssh.runpod.io"],
            input=script, capture_output=True, text=True, timeout=timeout_s,
        )
    except subprocess.TimeoutExpired:
        if not quiet:
            print(f"[harness] ssh timed out after {timeout_s}s", flush=True)
        return 124, ""
    raw = proc.stdout.replace("\r\n", "\n")
    rc, body = 0, raw
    marker = raw.rfind("__NM_RC__")
    if marker >= 0:
        tail = raw[marker + len("__NM_RC__"):]
        digits = tail.split("__", 1)[0].strip()
        # A non-digit here means the sentinel we matched is the PTY's ECHO of
        # the command (literal `$?`), not its output — i.e. the shell never
        # executed anything. Reporting 0 for that is how a bootstrap that ran
        # nothing looks like a clean success; fail loudly instead.
        rc = int(digits) if digits.isdigit() else 125
        begin = raw.rfind("__NM_BEGIN__", 0, marker)
        start = begin + len("__NM_BEGIN__") if begin >= 0 else 0
        body = _clean(raw[start:marker])
    elif not quiet:
        print(f"[harness] ssh: no sentinel; raw tail: {raw[-200:]!r}", flush=True)
        rc = proc.returncode or 1
    if not quiet and rc != 0:
        print(f"[harness] remote rc={rc}: {proc.stderr[:200]}", flush=True)
    return rc, body


def wait_for_ssh_ready(hid, timeout_s=420):
    """Poll until the pod actually EXECUTES a command.

    The probe must be something the shell computes, never a literal: a PTY
    echoes whatever is typed, so `echo NOCKMARK_READY` satisfies a
    `"NOCKMARK_READY" in out` check the moment the proxy echoes it — before
    any shell is running. That false ready is how a bootstrap ends up
    talking to a container that is still starting, returning in seconds
    having run nothing. `$((6*7))` cannot appear in the echo; only real
    execution produces `NOCKMARK_42_READY`.
    """
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        rc, out = ssh(hid, "echo NOCKMARK_$((6*7))_READY", timeout_s=45, quiet=True)
        if rc == 0 and "NOCKMARK_42_READY" in out:
            return True
        time.sleep(10)
    raise RunPodError(f"pod {hid} never executed a command in {timeout_s}s")
