# Pelagos Security Model

This document describes Pelagos's security architecture, default hardening posture,
and structural properties that make it immune to certain classes of container escape
vulnerabilities.

---

## Default Security Posture

Every container spawned by pelagos — regardless of flags — receives the following
hardening automatically:

| Mechanism | Implementation | Effect |
|-----------|---------------|--------|
| Seccomp-BPF | Docker's default profile via `seccompiler` | ~380 syscalls allowed; dangerous calls blocked |
| Capability drop | All capabilities dropped in pre_exec | No ambient privilege; `:cap-add` restores specific ones |
| No-new-privileges | `PR_SET_NO_NEW_PRIVS` | Blocks setuid/setgid escalation |
| Masked paths | `/proc/kcore`, `/sys/firmware`, etc. bind-mounted to `/dev/null` | Hides sensitive kernel interfaces |
| Namespace isolation | UTS, Mount, IPC, PID, Network, User, Cgroup | Six independent isolation domains |

Optional hardening (opt-in per container):

| Mechanism | API | Effect |
|-----------|-----|--------|
| Read-only rootfs | `with_readonly_rootfs(true)` | `MS_RDONLY` remount; immutable filesystem |
| Landlock LSM | `with_landlock_ro(path)` / `with_landlock_rw(path)` | Kernel-enforced per-path filesystem rules |
| rlimits | `with_rlimit_*()` | Bound memory, CPU time, file descriptors |
| Cgroups v2 | `with_cgroup_memory()` etc. | Hard resource limits with kernel enforcement |

---

## Structural CVE Immunity — The 2025 runc TOCTOU Class

In November 2025 a cluster of three critical vulnerabilities was disclosed against runc:

| CVE | CVSS | Attack Vector |
|-----|------|--------------|
| CVE-2025-31133 | 8.6 | Race in `/proc` path resolution during container setup — host breakout |
| CVE-2025-52565 | 8.1 | Race in `/dev` node creation — symlink swap wins before isolation is complete |
| CVE-2025-52881 | 7.9 | `/proc/self/exe` reopen race during container exec |

All three exploit the same structural weakness in runc's architecture: runc writes
into `/proc` or `/dev` *from a privileged parent process* while the container
namespace is still being constructed. A malicious container can swap a path
component during this window to redirect the privileged write onto the host
filesystem.

### Why pelagos is immune by construction

Pelagos uses a fundamentally different process model:

1. **Single-threaded child setup.** After `fork()`, all namespace operations
   (`unshare`, `pivot_root`, mount setup, capability drop, seccomp application)
   run inside the child process via a `pre_exec` hook. There is no concurrent
   privileged parent thread writing into container-visible paths.

2. **No self re-exec.** Pelagos never re-opens `/proc/self/exe` or re-invokes
   itself to enter a namespace. The `/proc/self/exe` race window that drives
   CVE-2025-52881 does not exist.

3. **Atomically committed isolation.** The child calls `pivot_root` (or `chroot`)
   before any container-visible path is written. Once isolation is committed,
   the child no longer has access to host paths — there is no partial-setup window.

4. **Seccomp applied last, inside the isolated child.** The BPF filter is the final
   step in the pre_exec sequence, applied after the child is fully isolated and
   namespaced. All setup syscalls complete before any restriction is in place.
   No setup operation races against an active filter.

This immunity is not a mitigation or a patch — it is a consequence of the
architecture. A future runc-class vulnerability in the same TOCTOU family would
need to find an entirely different attack surface in pelagos.

For deeper analysis, see [RUNTIME_STRATEGY_2026.md](RUNTIME_STRATEGY_2026.md).

---

## Pre-exec Hook Ordering

The sequence inside the child process (after `fork()`, before `exec()`) is fixed
and deliberately ordered. Changing the order breaks security invariants.

```
1.  unshare(namespaces)
2.  make mounts private (if MOUNT namespace)
3.  write UID/GID mappings (if USER namespace)
4.  setuid / setgid
5.  chroot / pivot_root
6.  mount /proc, /sys, /dev
7.  apply bind mounts, tmpfs, overlays
8.  mask sensitive paths
9.  drop capabilities
10. set rlimits
11. apply Landlock rules       ← after chroot; before seccomp
12. PR_SET_NO_NEW_PRIVS        ← if requested
13. apply seccomp BPF filter   ← MUST be last
```

Seccomp is last because many of the preceding steps require syscalls (e.g. `mount`,
`setuid`, `unshare`) that the Docker default profile blocks. Landlock is applied
before seccomp because `landlock_restrict_self` (syscall 446) is not in the Docker
default allowlist.

---

## Reporting Security Issues

Please report security vulnerabilities via GitHub Issues with the `security` label,
or by opening a private security advisory at:
https://github.com/pelagos-containers/pelagos/security/advisories/new
