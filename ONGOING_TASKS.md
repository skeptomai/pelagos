# Ongoing Tasks

## Current Task: None — E2E suite passing

**Status:** IDLE

All E2E tests pass (81 passed, 0 failed, 1 skipped). Last run: 2026-02-20.

---

## Up Next: Developer Experience

- Example scripts / tutorials (e.g., multi-container demo with networking + volumes)
- CI integration (GitHub Actions running the rootless E2E suite)
- Will scope after E2E testing work is complete.

---

## Deferred: Feature Work

- **AppArmor / SELinux** — MAC profile support. seccomp + capabilities + masked paths
  stack is solid. Revisit if there's demand.
- **OCI Compliance Phase 3** — fine-grained device ACLs, seccomp arg conditions,
  remaining hooks (`createRuntime`, `startContainer`, `annotations`).
- **Authenticated registry pulls** — currently anonymous only.

---

## Completed Phases

### E2E Bug Fixes (v0.2.1)
**COMPLETE** — Fixed 4 bugs found by E2E suite:
1. `--workdir`: proc mount used relative path, broke after chdir. Fixed to absolute `/proc`.
2. `seccomp=minimal`: ~70 missing syscall numbers in hand-maintained lookup table.
3. `exec --user`: setuid dropped capabilities before setns(CLONE_NEWNS). Reordered
   setuid/setgid to run after user callback and namespace joins.
4. `exec --workdir`: workdir captured into pre_exec callback instead of hardcoded chdir("/").

### Phase A+B: Storage Path Abstraction + Rootless Overlay (v0.2.0)
**RELEASED** — rootless image pull and container run with single-UID mapping.

### Phase D: Minimal `/dev` Setup
**COMPLETE** — tmpfs + safe devices replacing host /dev bind-mount.

### Phase C: Multi-UID Mapping via Subordinate Ranges
**COMPLETE** — `newuidmap`/`newgidmap` helpers with pipe+thread sync; auto-detects
subordinate ranges from `/etc/subuid` and `/etc/subgid`; falls back to single-UID
mapping when helpers unavailable.

### Phase E: Rootless Cgroup v2 Delegation
**COMPLETE** — direct cgroupfs writes under user's delegated cgroup scope.

### Rootless E2E Test Script
**COMPLETE** — `scripts/test-rootless.sh` covering all rootless phases via CLI binary.
8 sections, skip-on-missing-prereqs, follows `test-dev.sh` pattern.

---

## Previous Releases

### v0.1.0 — Initial Release
Full feature set: namespaces, seccomp, capabilities, cgroups v2, overlay, networking,
OCI image pull, container exec, OCI runtime compliance, interactive PTY.
