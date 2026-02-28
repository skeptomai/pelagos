# OCI Runtime Spec Conformance — Remora

This document tracks the gap between remora's current OCI implementation and full
compliance with the [OCI Runtime Specification v1.2](https://github.com/opencontainers/runtime-spec).
It is the working reference for the conformance epic (GitHub issue #11).

---

## What "OCI Compliance" Means in Practice

There are two distinct targets:

### 1. `opencontainers/runtime-tools` conformance suite (pass/fail)

The official conformance harness. It generates minimal OCI bundles and invokes the runtime
using the standard CLI interface:

```
$RUNTIME create --bundle $BUNDLE_PATH $CONTAINER_ID
$RUNTIME start $CONTAINER_ID
$RUNTIME state $CONTAINER_ID
$RUNTIME kill $CONTAINER_ID SIGTERM
$RUNTIME delete $CONTAINER_ID
```

Tests validate: state transitions, namespace setup, capabilities, mounts, hooks, resources,
devices, sysctl, seccomp. This is the gate for containerd/CRI-O/Kubernetes integration.

### 2. Cold-start benchmark (performance, self-reported)

Not a single canonical registry — each runtime publishes its own numbers using a minimal
bundle. Covered separately in `docs/RUNTIME_STRATEGY_2026.md`. Blocked on Phase 1+2 of
this compliance work.

---

## Gap Analysis by Phase

### Phase 1 — CLI Interface (Prerequisite)

**The conformance harness cannot invoke remora at all without these.**

| Item | Status | Notes |
|------|--------|-------|
| `create --bundle <path>` flag | ❌ | Currently a positional arg; harness requires the flag |
| `create --console-socket <path>` flag | ❌ | Required by spec; needed for terminal containers |
| `create --pid-file <path>` flag | ❌ | Required by spec; containerd uses this to read PID |

Current CLI: `Create { id: String, bundle: PathBuf }` — positional args.

Required CLI: `$RUNTIME create [--bundle <path>] [--console-socket <path>] [--pid-file <path>] <id>`

Fix: restructure the `Create` variant in `main.rs` to use named flags; keep backward-compat
by also accepting bundle as a positional fallback.

---

### Phase 2 — Mount Type Dispatch

**Most OCI bundles specify these mounts; remora silently skips them (falls to bind-mount).**

A standard OCI bundle `config.json` includes:

```json
[
  {"destination":"/proc",          "type":"proc",    "source":"proc"},
  {"destination":"/sys",           "type":"sysfs",   "source":"sysfs"},
  {"destination":"/dev",           "type":"tmpfs",   "source":"tmpfs"},
  {"destination":"/dev/pts",       "type":"devpts",  "source":"devpts"},
  {"destination":"/dev/shm",       "type":"tmpfs",   "source":"tmpfs"},
  {"destination":"/dev/mqueue",    "type":"mqueue",  "source":"mqueue"},
  {"destination":"/sys/fs/cgroup", "type":"cgroup2", "source":"cgroup"}
]
```

Current `oci.rs::build_command` dispatch:
- `"tmpfs"` → `with_tmpfs()` ✓
- anything else → bind mount ✗ (`proc`, `sysfs`, `devpts`, `mqueue`, `cgroup`, `cgroup2`)

Fix: add `with_kernel_mount(fs_type, source, dest, flags, data)` to `container::Command`;
update `oci.rs` to dispatch all mount types correctly. Kernel mounts (proc/sysfs/etc.) go
into a new `kernel_mounts: Vec<KernelMount>` list and are applied in pre_exec after chroot.

Mount type → flags mapping:

| Type | MS_* flags | data |
|------|-----------|------|
| `proc` | `MS_NOSUID\|MS_NOEXEC\|MS_NODEV` | "" |
| `sysfs` | `MS_NOSUID\|MS_NOEXEC\|MS_NODEV` | "" |
| `devpts` | `MS_NOSUID\|MS_NOEXEC` | "newinstance,ptmxmode=0666,mode=0620,gid=5" |
| `mqueue` | `MS_NOSUID\|MS_NOEXEC\|MS_NODEV` | "" |
| `cgroup`/`cgroup2` | `MS_NOSUID\|MS_NOEXEC\|MS_NODEV\|MS_RELATIME` | "" |

Options from the OCI config (`nosuid`, `noexec`, `nodev`, `ro`, etc.) override the defaults.

---

### Phase 3 — Capability Name Table

**OCI bundles specify capabilities by name; remora only maps ~12 of ~40.**

Current `oci_cap_to_flag` in `src/oci.rs` handles:
`CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `FSETID`, `KILL`, `SETGID`, `SETUID`,
`NET_BIND_SERVICE`, `NET_RAW`, `SYS_CHROOT`, `SYS_ADMIN`, `SYS_PTRACE`

Missing (Docker default set includes many of these):
`AUDIT_WRITE`, `DAC_READ_SEARCH`, `IPC_LOCK`, `IPC_OWNER`, `LEASE`, `LINUX_IMMUTABLE`,
`MAC_ADMIN`, `MAC_OVERRIDE`, `MKNOD`, `NET_ADMIN`, `NET_BROADCAST`, `PERFMON`,
`SETFCAP`, `SETPCAP`, `SYS_BOOT`, `SYS_MODULE`, `SYS_NICE`, `SYS_PACCT`,
`SYS_RAWIO`, `SYS_RESOURCE`, `SYS_TIME`, `SYS_TTY_CONFIG`, `SYSLOG`,
`WAKE_ALARM`, `AUDIT_CONTROL`, `AUDIT_READ`, `BPF`, `BLOCK_SUSPEND`,
`CHECKPOINT_RESTORE`, `NET_ADMIN`

Fix: mechanical — add all remaining entries to the `match` in `oci_cap_to_flag`.

---

### Phase 4 — State Output and Signal Table

**Minor spec gaps in `cmd_state` output and `cmd_kill` signal handling.**

| Item | Status | Fix |
|------|--------|-----|
| `annotations` field in state JSON | ❌ | Add `annotations: Option<HashMap<String,String>>` to `OciState`; populate from config |
| Full signal table in `cmd_kill` | ❌ (7 signals) | Add all POSIX signals + Linux extensions |

Runtime-tools sends signals like `SIGWINCH`, `SIGCONT`, `SIGSTOP`, `SIGQUIT`, `SIGPIPE`,
`SIGALRM`, `SIGUSR1`, `SIGUSR2`, `SIGCHLD`, `SIGPWR`, `SIGSYS`, etc.

---

### Phase 5 — Linux Config Fields

**Fields parsed from `config.json` but ignored or unimplemented.**

| Field | Status | Fix |
|-------|--------|-----|
| `linux.rootfsPropagation` | ❌ ignored | Parse; apply `MS_SHARED\|MS_SLAVE\|MS_PRIVATE\|MS_UNBINDABLE` in pre_exec |
| `linux.cgroupsPath` | ❌ ignored | Parse; use as cgroup leaf path instead of auto-generated name |
| `linux.seccomp` OCI format | ✅ via `filter_from_oci` | Already handled |
| `linux.devices` | ✅ | Already handled |
| `linux.sysctl` | ✅ | Already handled |
| `linux.namespaces` with path | ✅ | Already handled via `with_namespace_join` |

---

### Phase 6 — Hooks in Container Namespace

**Most complex phase — `createContainer` and `startContainer` hooks must execute inside the container's namespaces.**

OCI hook types and where they run:

| Hook | Namespace | When |
|------|-----------|------|
| `prestart` (deprecated) | host | after create, before start |
| `createRuntime` | host | after namespaces created ✓ |
| `createContainer` | **container** | after namespaces created, before exec |
| `startContainer` | **container** | after `start` called, before entry-point execs |
| `poststart` | host | after entry-point execs ✓ |
| `poststop` | host | after container exits ✓ |

Current remora: `createRuntime` ✓, `createContainer` runs in host ns (wrong), `startContainer` not implemented.

**`createContainer` fix:** After reading the ready pipe (container's namespaces are up, container
is blocking on `accept(exec.sock)`), the parent can `setns()` into the container's mount/net/uts
namespaces and exec the hook programs. This mirrors `exec_in_container` but does not require
entering the PID namespace (hooks are identified by host-side paths).

**`startContainer` fix:** Add a second sync socket (`hooks.sock`) that the container's shim
blocks on before calling `exec()`. After `remora start` sends the trigger byte to `exec.sock`,
a hook-runner process (forked by `cmd_start`) joins the container namespaces, runs the
startContainer hooks, then connects to `hooks.sock` to signal completion. Only then does
the container exec.

Implementation in `container.rs`: extend `oci_sync` from `Option<(ready_w, listen_fd)>` to
`Option<OciSync>` with an additional `hooks_listen_fd`.

---

## Implementation Order

```
Phase 1: CLI flags          — prerequisite; runtime-tools can invoke remora
Phase 2: Mount types        — most bundles use proc/sysfs/devpts; tests fail without it
Phase 3: Capability table   — mechanical; blocks several conformance tests
Phase 4: State + signals    — quick; needed for state and kill tests
Phase 5: Linux config       — rootfsPropagation, cgroupsPath
Phase 6: Hook namespaces    — complex; final compliance gate
```

All phases include integration tests and documentation updates.

---

## Testing Strategy

- **Unit:** `cargo test --lib` for parsing and dispatch logic
- **Conformance:** `opencontainers/runtime-tools` after each phase
  ```bash
  git clone https://github.com/opencontainers/runtime-tools
  cd runtime-tools
  make runtimetest
  sudo RUNTIME=remora ./runtimetest
  ```
- **Integration:** new tests in `tests/integration_tests.rs` for each behaviour
- **Regression:** existing 84 integration tests must continue to pass

---

## State Tracking

| Phase | Issue | Status |
|-------|-------|--------|
| Phase 1: CLI flags | #12 | pending |
| Phase 2: Mount types | #13 | pending |
| Phase 3: Capability table | #14 | pending |
| Phase 4: State + signals | #15 | pending |
| Phase 5: Linux config | #16 | pending |
| Phase 6: Hook namespaces | #17 | pending |
