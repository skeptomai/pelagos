# Remora Watcher Process Model

This document describes the process tree, thread inventory, namespace isolation, and
known limitations of Remora's detached container runtime model.

---

## Overview

When you run `remora run -d <image>`, the CLI does **not** stay resident as a daemon.
Instead it forks a lightweight _watcher process_ that owns the container's lifetime, then
exits immediately (printing the container name). The user's shell is free; the watcher
runs invisibly in the background.

---

## Process Tree

### Without PID Namespace

```
remora run -d (parent)
  └─ fork ──► watcher   [setsid → new session leader]
                 └─ cmd.spawn ──► container process   [PID 1 on host?  No — just some PID]
```

When the parent calls `fork()`, the resulting watcher child calls `setsid()` so it
becomes the leader of a new session, detached from the terminal. When the parent exits,
the watcher is re-parented to init (PID 1 on the host). The watcher then `spawn()`s the
container process.

### With PID Namespace (Enabled — see § "Double-fork")

```
remora run -d (parent)
  └─ fork ──► watcher   [setsid]
                 └─ cmd.spawn ──► intermediate P   [host PID namespace; waits for C]
                                       └─ fork ──► C (PID 1 in container)   [new PID namespace]
```

The library's `pre_exec` hook handles the double-fork transparently. The watcher sees
only one child (the intermediate P) and calls `child.wait()` on it. P calls `waitpid(C)`
and exits with C's exit status. `PR_SET_PDEATHSIG` ensures C receives `SIGKILL` if P is
killed unexpectedly.

---

## Who Is PID 1?

| Mode | PID 1 in container |
|------|--------------------|
| No PID namespace | **Nobody** — the container process has a host PID like 12345; kernel zombie-reaping semantics do not apply |
| PID namespace enabled | **The container's entry point** (e.g. `sleep 60`) — it is literally PID 1 inside the namespace and must reap children |

Without a PID namespace the container process is just a host process in a chroot+mount
jail. Zombie reaping falls to the host's init. This is fine for simple processes but
problematic for any process that spawns children (shells, language runtimes, daemons).

With a PID namespace the entry point becomes PID 1. If the process does not reap
zombies (`waitpid`), zombie accumulation can occur. In practice, using a small init
shim (like `tini`) as the entry point is recommended for multi-child containers.

---

## Namespace Isolation

### Rootfs-based runs (`remora run --rootfs …`)

```
Namespace::UTS | Namespace::MOUNT | Namespace::PID
```

UTS, mount, and PID namespace are always enabled.

### OCI image-based runs (`remora run <image>`)

**Before the fix in this commit:** only `MOUNT` (added by `with_image_layers`) and
`NET` (added by `with_network`) were active — no UTS, **no PID**.

**After the fix:** `Namespace::UTS | Namespace::PID` are added explicitly in
`build_image_run`, matching the rootfs path. Image-based containers now get full PID
and UTS isolation.

---

## Thread Inventory Per Container

The watcher process maintains a small, fixed set of threads:

| Thread | Purpose | Lifetime |
|--------|----------|----------|
| **Main thread** | `child.wait()` — blocks until container exits; then writes final `state.json`, cleans up DNS | Container lifetime |
| **stdout relay** | Reads container stdout pipe → appends to `stdout.log` | Until container stdout pipe closes |
| **stderr relay** | Reads container stderr pipe → appends to `stderr.log` | Until container stderr pipe closes |
| **Health monitor** | Polls container health every `interval_secs`; writes `HealthStatus` to `state.json` | Container lifetime (if image has `HEALTHCHECK`) |

Each health **probe** spawns one additional short-lived thread:

| Thread | Purpose | Lifetime |
|--------|----------|----------|
| **Probe thread** | Runs `exec_in_container`; sends result over channel | Duration of probe (≤ `timeout_secs`) |

The probe thread is necessary to enforce `timeout_secs`: the health monitor spawns it
and waits on a channel with `recv_timeout`. If the probe hangs, the channel times out
and the probe thread is abandoned (it will eventually terminate when its child process
dies or is killed by the OS).

### Thread count per container

- **Minimum (no HEALTHCHECK):** 3 threads (main + stdout relay + stderr relay)
- **With HEALTHCHECK:** 4 threads at rest; 5 during an active probe

### Scalability

The thread-per-fd relay approach is simple and correct for modest workloads. For N
simultaneously running containers, the watcher processes consume O(N) threads across N
separate processes (each watcher is its own process). This is not a concern in practice
for ≤ hundreds of containers; for very large deployments an epoll-based relay would be
more efficient. This is documented as future work.

---

## `state.pid` and the Intermediate Process

With PID namespace enabled, `child.pid()` returns the PID of the _intermediate process_
(P), not PID 1 (C). This is the value stored as `state.pid` in `state.json` and used by:

- **`remora ps`** — shows P's PID in the PID column
- **`remora stop`** — sends `SIGTERM` to P; P exits; `PR_SET_PDEATHSIG` sends `SIGKILL` to C
- **`remora exec`** — joins P's namespaces (see caveat below)
- **`check_liveness`** — checks `kill(P, 0)`; returns false only after P exits (which happens when C exits)

This is correct: P is alive exactly as long as C is alive. Liveness and stop semantics
work as expected.

---

## `remora exec` and PID Namespace Caveat

`exec_in_container` (and `remora exec`) discover namespaces by comparing
`/proc/{pid}/ns/*` against `/proc/1/ns/*`. When PID namespace is active:

- `/proc/P/ns/mnt` — **container's mount namespace** ✓ (P unshared MOUNT before forking C)
- `/proc/P/ns/net` — **container's network namespace** ✓
- `/proc/P/ns/uts` — **container's UTS namespace** ✓
- `/proc/P/ns/pid` — **host PID namespace** — same inode as `/proc/1/ns/pid` because P
  itself is in the host PID namespace (only P's *children* enter the new namespace)

As a result, `remora exec` currently does **not** join the container's PID namespace.
The exec'd process sees the container's filesystem, network, and UTS, but its PID
namespace is the host's. Inside an exec'd shell, `ps` will show host PIDs.

**Future improvement:** to join the container's PID namespace, `discover_namespaces`
should also check `/proc/P/ns/pid_for_children` (available since Linux 3.8), which
points to the new PID namespace that C inhabits. This would allow exec'd processes to
be proper members of the container's PID namespace.

---

## Health Monitor Namespace Access

The health monitor calls `exec_in_container(P.pid, probe_cmd)`. As described above, it
joins P's mount, net, and UTS namespaces — which are the container's namespaces. Health
probes run in the container's filesystem and network context, which is exactly what is
needed. The probe not being in the container's PID namespace does not affect correctness.

---

## Signal Propagation

| Signal | Sent to | Effect |
|--------|---------|--------|
| `SIGTERM` (from `remora stop`) | P (intermediate) | P dies → C gets `SIGKILL` via `PR_SET_PDEATHSIG` |
| `SIGKILL` | P | Same effect |
| `SIGTERM` | C (PID 1) | C handles or ignores per its signal handlers |

If the watcher itself dies unexpectedly (e.g. OOM kill), P is re-parented to host init.
P's `PR_SET_PDEATHSIG` on C was set relative to P's parent *at the time of the
`prctl` call* — so P dying does not trigger pdeathsig for C in this scenario. This is a
known limitation; a subreaper (`PR_SET_CHILD_SUBREAPER`) set on the watcher would
address it. This is documented as future work.

---

## Summary of Known Limitations

| Limitation | Impact | Planned fix |
|-----------|--------|-------------|
| `remora exec` does not join container PID namespace | `ps` in exec'd shell shows host PIDs | Check `pid_for_children` in `discover_namespaces` |
| Probe timeout does not SIGKILL the probe child | Hung probes consume a thread until OS reaps them | Explicit SIGKILL on timeout |
| Thread-per-fd log relay | O(2) threads per container for I/O | epoll-based relay (future) |
| Watcher death does not propagate to PID 1 | Container orphaned if watcher dies | `PR_SET_CHILD_SUBREAPER` on watcher |
