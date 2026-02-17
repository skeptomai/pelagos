# Phase 5: Cgroups v2 Resource Management — Implementation Plan

**Date:** 2026-02-17
**Status:** Approved for implementation

---

## Context

Remora uses `rlimits` for resource control today — per-process kernel limits applied in `pre_exec`. They cover basic cases but have key gaps: no I/O bandwidth control, no CPU shares (proportional scheduling), no process-count limits, no resource statistics. `cgroups-rs = "0.5.0"` is already in `Cargo.toml` but unused. Phase 5 activates it.

cgroups v2 (unified hierarchy) fills all these gaps and is what Docker/runc use. The dev system runs pure cgroups v2. `cgroups-rs` auto-detects v1 vs v2 via `hierarchies::auto()`.

### rlimits vs cgroups — what each buys us

| Feature | rlimits (current) | cgroups v2 (new) |
|---------|------------------|------------------|
| Memory limit | ✅ RLIMIT_AS | ✅ memory.max |
| CPU time | ✅ RLIMIT_CPU (seconds) | ✅ cpu.max (quota/period) |
| CPU shares | ❌ | ✅ cpu.weight |
| File descriptors | ✅ RLIMIT_NOFILE | ❌ |
| Process count | ✅ RLIMIT_NPROC | ✅ pids.max |
| I/O bandwidth | ❌ | ✅ io.max (follow-on) |
| Resource stats | ❌ | ✅ memory.current, cpu.stat, pids.current |
| Hierarchical limits | ❌ | ✅ |

Both coexist — no breaking changes.

---

## Key Architecture: Parent-Side Management

Unlike rlimits (applied in child via `pre_exec`), cgroups are managed entirely from the **parent process** — no changes to the `pre_exec` closure.

**Flow:**
1. Parent builds `CgroupConfig` (limits only, no cgroup created yet)
2. Parent calls `self.inner.spawn()` → gets child PID
3. Parent creates cgroup, applies limits, adds child PID — immediately after step 2
4. `Child` struct holds the `Cgroup` handle
5. After `child.wait()` → parent calls `cgroup.delete()`

Race condition is negligible: cgroups enforce limits on running processes; adding a PID milliseconds after exec is fine for all practical workloads.

---

## New Module: `src/cgroup.rs`

Keeps cgroup logic out of the already-large `container.rs`.

```rust
use cgroups_rs::{Cgroup, CgroupPid, cgroup_builder::CgroupBuilder, hierarchies};

pub struct CgroupConfig {
    pub memory_limit: Option<i64>,         // bytes hard limit → memory.max
    pub cpu_shares:   Option<u64>,         // weight 1–10000  → cpu.weight
    pub cpu_quota:    Option<(i64, u64)>,  // (quota_us, period_us) → cpu.max
    pub pids_limit:   Option<u64>,         // max processes   → pids.max
}

/// Create cgroup, apply limits, add child PID. Returns handle for cleanup.
pub fn setup_cgroup(cfg: &CgroupConfig, child_pid: u32) -> io::Result<Cgroup>

/// Delete cgroup after child exits. Logs but does not propagate errors.
pub fn teardown_cgroup(cg: Cgroup)

pub struct ResourceStats {
    pub memory_current_bytes: u64,  // memory.current
    pub cpu_usage_ns:         u64,  // cpu.stat usage_usec * 1000
    pub pids_current:         u64,  // pids.current
}

pub fn read_stats(cg: &Cgroup) -> io::Result<ResourceStats>
```

`setup_cgroup` logic:
```rust
let hier = hierarchies::auto();
let cg = CgroupBuilder::new(&format!("remora-{}", child_pid))
    .memory().memory_hard_limit(bytes).done()    // if memory_limit set
    .cpu().shares(shares).done()                  // if cpu_shares set
    .cpu().quota(quota).period(period).done()     // if cpu_quota set
    .pid().maximum_number_of_processes(max).done()// if pids_limit set
    .build(hier)?;
cg.add_task(CgroupPid::from(child_pid as u64))?;
```

---

## `src/container.rs` Changes

### Command struct — one new field (after `rlimits`):
```rust
cgroup_config: Option<crate::cgroup::CgroupConfig>,
```
Initialize to `None` in `Command::new()`.

### Builder methods:
```rust
pub fn with_cgroup_memory(mut self, bytes: i64) -> Self
pub fn with_cgroup_cpu_shares(mut self, shares: u64) -> Self
pub fn with_cgroup_cpu_quota(mut self, quota_us: i64, period_us: u64) -> Self
pub fn with_cgroup_pids_limit(mut self, max: u64) -> Self
```
Each lazily initializes `cgroup_config` (creates `CgroupConfig` with all fields `None` if not yet present).

### Child struct — add cgroup handle:
```rust
pub struct Child {
    inner: process::Child,
    cgroup: Option<cgroups_rs::Cgroup>,
}
```

`wait()` and `wait_with_output()` call `teardown_cgroup` after the child exits.

New method on `Child`:
```rust
pub fn resource_stats(&self) -> Result<ResourceStats, Error>
```

### In `spawn()` — after `self.inner.spawn()`:
```rust
let child_inner = self.inner.spawn().map_err(Error::Spawn)?;
let cgroup = if let Some(ref cfg) = self.cgroup_config {
    Some(cgroup::setup_cgroup(cfg, child_inner.id()).map_err(Error::Io)?)
} else {
    None
};
drop(join_ns_files);
Ok(Child { inner: child_inner, cgroup })
```

Same change in `spawn_interactive()`.

---

## `src/lib.rs` Change

Add `pub mod cgroup;`

---

## New Integration Tests (5 new → 31 total)

All follow existing pattern (`is_root()`, `get_test_rootfs()`, `ALPINE_PATH`).

1. **test_cgroup_memory_limit** — set a small memory limit; run a process that tries to allocate beyond it; verify killed or exits non-zero
2. **test_cgroup_pids_limit** — set `pids_limit(4)`; run a shell that tries to fork many subprocesses; verify fails
3. **test_cgroup_cpu_shares** — set `cpu_shares(512)`; run a quick compute task; verify exits successfully (smoke test)
4. **test_resource_stats** — run a container, call `resource_stats()` after wait; verify values are plausible
5. **test_cgroup_cleanup** — after `child.wait()`, verify `/sys/fs/cgroup/remora-{pid}` no longer exists

---

## Files to Modify

| File | Change |
|------|--------|
| `src/cgroup.rs` | **New**: `CgroupConfig`, `setup_cgroup`, `teardown_cgroup`, `ResourceStats`, `read_stats` |
| `src/lib.rs` | Add `pub mod cgroup;` |
| `src/container.rs` | `cgroup_config` field, builder methods, `Child` extension, spawn integration |
| `tests/integration_tests.rs` | Import `cgroup::ResourceStats`; 5 new tests |
| `CLAUDE.md` | Phase 5 complete, file structure, comparison table |

---

## Verification

1. `cargo build` — clean build
2. `cargo test --lib` — all unit tests pass
3. User runs: `sudo -E cargo test --test integration_tests` — all 31 tests pass
4. Manual check: `ls /sys/fs/cgroup/ | grep remora` shows no leftover cgroups after tests

---

## Notes / Risks

- `cgroups_rs::Cgroup` is not `Send` — fine since `Child` is used on the spawning thread only
- `Cgroup::delete()` can fail if tasks remain; once the child exits the kernel removes it from cgroups automatically, so teardown after `wait()` should succeed
- IO weight (`io.weight`) deferred — requires block device major:minor numbers, harder to unit-test; can be a follow-on
- rlimits coexist unchanged — no breaking changes
