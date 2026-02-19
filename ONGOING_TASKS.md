# Ongoing Tasks

## Completed: Rootless Remora — Phase A + B (v0.2.0)

**Status: RELEASED** — v0.2.0 tagged and released. All code changes, documentation, and release artifacts done.

### What was delivered

**Phase A — Storage Path Abstraction:**
- New `src/paths.rs` — single source of truth for all filesystem paths
- Root: `/var/lib/remora/` + `/run/remora/`
- Rootless: `~/.local/share/remora/` + `$XDG_RUNTIME_DIR/remora/`
- All hardcoded paths across 8 files migrated to `crate::paths::*`

**Phase B — Rootless Whiteouts + Overlay Mount:**
- Rootless whiteouts via `user.overlay.whiteout` / `user.overlay.opaque` xattrs
- Native overlay with `userxattr` on kernel 5.11+
- Automatic `fuse-overlayfs` fallback for older kernels
- Overlay probe forks into user+mount namespace with uid/gid mappings
- PTY slave fd CLOEXEC management prevents relay loop hang with fuse-overlayfs

**Documentation:**
- README.md — rootless quick start, updated features/requirements/comparison
- USER_GUIDE.md — rootless sections for image pull, overlay, storage, troubleshooting

**Bugs fixed during implementation:**
- CLI `-i` flag consumed as positional arg (fixed with `trailing_var_arg`)
- Container hang on exit (PTY slave fd inherited by fuse-overlayfs daemon)
- Native overlay probe failed without uid/gid mappings after unshare

---

## Current: Rootless Remora — Phases C, D, E

### Context

Phases A+B (v0.2.0) gave us rootless image pull and container run with a single-UID
mapping (`container 0 → host UID`). Three gaps remain for Podman-level rootless parity:

1. Files inside images owned by UIDs other than 0 (e.g. `nobody:65534`) appear as
   `nobody` / get `EACCES` because we only map a single UID.
2. `/dev` is a recursive bind-mount of the host's `/dev` with error tolerance — this
   leaks host device nodes and fails for mknod in rootless mode.
3. Cgroup resource limits (`--memory`, `--cpus`, `--pids-limit`) are silently skipped
   in rootless mode.

---

### Phase C: Multi-UID Mapping

**Why:** Most OCI images have files owned by multiple UIDs (root:0, daemon:1, nobody:65534,
etc.). With only `0 → host_uid`, any access to files owned by other UIDs fails or maps to
`nobody`. Podman solves this with subordinate UID/GID ranges from `/etc/subuid` and
`/etc/subgid`, applied via the setuid helpers `newuidmap`/`newgidmap`.

**How it works on Linux:**
- `/etc/subuid` contains lines like `cb:100000:65536` (user `cb` may map container UIDs
  100000–165535 to host UIDs 100000–165535)
- Unprivileged processes can only write a single-line uid_map (`0 <host_uid> 1`) directly
- For multi-range mappings, the kernel requires the helper binaries `newuidmap`/`newgidmap`
  (setuid-root, from `shadow-utils`/`uidmap` package)
- These helpers validate the requested ranges against `/etc/subuid`/`/etc/subgid` and write
  the maps on behalf of the unprivileged process

**New file: `src/idmap.rs`** (~150 lines)

```rust
/// A subordinate ID range from /etc/subuid or /etc/subgid
pub struct SubIdRange {
    pub start: u32,   // first host UID/GID in the range
    pub count: u32,   // number of consecutive IDs
}

/// Parse /etc/subuid (or /etc/subgid) for the current user.
/// Returns all ranges assigned to the user (by name or numeric UID).
pub fn parse_subid_file(path: &Path, user: &str, uid: u32) -> io::Result<Vec<SubIdRange>>

/// Check whether newuidmap/newgidmap are available on PATH.
pub fn has_newuidmap() -> bool
pub fn has_newgidmap() -> bool

/// After child calls unshare(CLONE_NEWUSER), parent calls these to set up
/// the UID/GID maps via the helper binaries.
/// pid: the child's PID
/// uid_map: lines for /proc/{pid}/uid_map
/// gid_map: lines for /proc/{pid}/gid_map
pub fn apply_uid_map_via_helper(pid: u32, maps: &[UidMap]) -> io::Result<()>
pub fn apply_gid_map_via_helper(pid: u32, maps: &[GidMap]) -> io::Result<()>
```

**Changes to `src/container.rs`:**

Currently (rootless auto-config, ~line 1563):
```rust
if self.uid_maps.is_empty() {
    self.uid_maps.push(UidMap { inside: 0, outside: host_uid, count: 1 });
}
```

New behavior when rootless + subuid ranges available:
```rust
if self.uid_maps.is_empty() && is_rootless {
    match idmap::parse_subid_file("/etc/subuid", &username, host_uid) {
        Ok(ranges) if !ranges.is_empty() && idmap::has_newuidmap() => {
            // Map container 0 → host_uid (1 ID) + container 1 → subuid_start (subuid_count IDs)
            self.uid_maps.push(UidMap { inside: 0, outside: host_uid, count: 1 });
            self.uid_maps.push(UidMap { inside: 1, outside: ranges[0].start, count: ranges[0].count });
            self.use_id_helpers = true;
        }
        _ => {
            // Fallback: single UID map (current behavior)
            self.uid_maps.push(UidMap { inside: 0, outside: host_uid, count: 1 });
        }
    }
}
```

**UID map writing changes (pre_exec ~line 1857):**

Currently the child writes `/proc/self/uid_map` directly. With multi-range maps, the
child must signal the parent to run `newuidmap`/`newgidmap` instead. This requires a
sync pipe:

1. Before fork: create a pipe pair `(map_read_fd, map_write_fd)`
2. Child (pre_exec): after `unshare(CLONE_NEWUSER)`, write a ready byte to `map_write_fd`,
   then block reading `map_read_fd` waiting for parent's "done" byte
3. Parent (after fork): read ready byte from `map_read_fd`, run `newuidmap`/`newgidmap`
   as subprocesses, write done byte to `map_write_fd`
4. Child resumes pre_exec

When `use_id_helpers` is false, the current direct-write path is used (no pipe needed).

Both `spawn()` and `spawn_interactive()` need this change.

**Files changed:**

| File | What changes |
|------|-------------|
| `src/idmap.rs` | **New** — subuid/subgid parsing, helper detection, helper invocation |
| `src/lib.rs` | Add `pub mod idmap;` |
| `src/container.rs` | Auto-detect multi-UID ranges; sync pipe for parent-side map writing; `use_id_helpers` field on `Command` |

---

### Phase D: Minimal `/dev` Setup — COMPLETE ✅

**Status: IMPLEMENTED** — Replaced host `/dev` bind-mount with minimal tmpfs + safe device setup.

**What was delivered:**
- Replaced recursive bind-mount of host `/dev` with tmpfs + safe devices in both `spawn()` and `spawn_interactive()`
- Host device FDs opened before chroot, bind-mounted via `/proc/self/fd/<n>` after chroot
- Safe devices: null, zero, full, random, urandom, tty (bind-mounted from host)
- Subdirectories: /dev/pts (devpts), /dev/shm (tmpfs), /dev/mqueue (mqueue)
- Symlinks: /dev/fd, /dev/stdin, /dev/stdout, /dev/stderr, /dev/ptmx
- Tolerates failures gracefully in rootless mode
- Enabled by default via `with_image_layers()` and CLI `build_command()`
- 5 integration tests, E2E test script (`scripts/test-dev.sh`)

**Files changed:**

| File | What changes |
|------|-------------|
| `src/container.rs` | Minimal /dev setup in `spawn()` + `spawn_interactive()`; host device FD pre-opening; `mount_dev = true` in `with_image_layers()` |
| `src/cli/run.rs` | Added `.with_dev_mount()` to `build_command()` |
| `tests/integration_tests.rs` | New `dev` module: 5 tests |
| `scripts/test-dev.sh` | **New** — E2E test script (root + rootless sections) |
| `docs/INTEGRATION_TESTS.md` | Documented all 5 new tests |

---

### Phase E: Rootless Cgroup v2 Delegation

**Why:** Resource limits (`--memory`, `--cpus`, `--pids-limit`) are silently skipped in
rootless mode. On systemd-based systems, cgroups v2 can be delegated to unprivileged
users, allowing container resource control without root.

**How cgroup delegation works:**
- systemd creates a per-user cgroup scope at `/sys/fs/cgroup/user.slice/user-$UID.slice/user@$UID.service/`
- If `Delegate=yes` is set (default in modern systemd), the user owns this subtree
- The user can create sub-cgroups and write to `memory.max`, `cpu.max`, `pids.max` etc.
- The `$XDG_RUNTIME_DIR` typically has the cgroup path at `$XDG_RUNTIME_DIR/../cgroup`
  or it can be read from `/proc/self/cgroup`

**How Remora will use it:**

Rather than `cgroups-rs` (which tries to create cgroups at the root of the hierarchy and
fails without root), in rootless mode we manage cgroups directly via filesystem writes:

**New file: `src/cgroup_rootless.rs`** (~120 lines)

```rust
/// Find the current process's cgroup path from /proc/self/cgroup.
/// Returns the path relative to the cgroup mount, e.g. "user.slice/user-1000.slice/..."
pub fn self_cgroup_path() -> io::Result<PathBuf>

/// Check if cgroup delegation is available:
/// 1. /proc/self/cgroup is readable and shows a cgroup2 path
/// 2. The cgroup directory is writable by the current user
/// 3. Required controllers (memory, cpu, pids) are available in cgroup.controllers
pub fn is_delegation_available() -> bool

/// Create a sub-cgroup under the user's delegated scope, apply limits, add child PID.
/// cgroup_name: "remora-{pid}"
pub fn setup_rootless_cgroup(cfg: &CgroupConfig, child_pid: u32) -> io::Result<RootlessCgroup>

/// Clean up: remove the sub-cgroup directory after all tasks have exited.
pub fn teardown_rootless_cgroup(cg: RootlessCgroup)

pub struct RootlessCgroup {
    path: PathBuf,  // full path to the cgroup directory
}
```

**`setup_rootless_cgroup()` implementation:**
1. Read `/proc/self/cgroup` → get current cgroup path (e.g. `user.slice/user-1000.slice/...`)
2. Construct full path: `/sys/fs/cgroup/{cgroup_path}/remora-{child_pid}/`
3. `mkdir` the sub-cgroup directory
4. Enable required controllers: write `+memory +cpu +pids` to `cgroup.subtree_control`
   in the parent directory (may already be enabled)
5. Write limits:
   - `memory.max` ← bytes (or "max" for unlimited)
   - `cpu.max` ← `quota_us period_us` (or "max period_us")
   - `cpu.weight` ← shares value
   - `pids.max` ← limit (or "max")
6. Write child PID to `cgroup.procs`

**Changes to `src/container.rs`:**

Replace the cgroup setup block (~line 2632 in `spawn()`, ~line 3700 in `spawn_interactive()`):

```rust
let cgroup = if let Some(ref cfg) = self.cgroup_config {
    if is_rootless {
        // Try rootless cgroup delegation
        match crate::cgroup_rootless::setup_rootless_cgroup(cfg, child_pid) {
            Ok(cg) => Some(CgroupHandle::Rootless(cg)),
            Err(e) => {
                log::warn!("rootless cgroup delegation not available, skipping: {}", e);
                None
            }
        }
    } else {
        match crate::cgroup::setup_cgroup(cfg, child_pid) {
            Ok(cg) => Some(CgroupHandle::Root(cg)),
            Err(e) => return Err(Error::Io(e)),
        }
    }
} else {
    None
};
```

New enum to hold either type:
```rust
enum CgroupHandle {
    Root(cgroups_rs::fs::Cgroup),
    Rootless(crate::cgroup_rootless::RootlessCgroup),
}
```

`Child.cgroup` field type changes from `Option<Cgroup>` to `Option<CgroupHandle>`.
`resource_stats()` and teardown in `wait()`/`wait_with_output()` dispatch on the enum.

**Files changed:**

| File | What changes |
|------|-------------|
| `src/cgroup_rootless.rs` | **New** — rootless cgroup delegation via direct fs writes |
| `src/lib.rs` | Add `pub mod cgroup_rootless;` |
| `src/container.rs` | `CgroupHandle` enum, dispatch in setup/teardown/stats (2 spawn locations + wait + wait_with_output + resource_stats) |

---

### Implementation Order

1. **Phase D first** (minimal `/dev`) — smallest scope, no new sync mechanisms, no
   external dependencies. Self-contained change in pre_exec block.
2. **Phase E second** (rootless cgroups) — new file but straightforward filesystem writes.
   No changes to fork/exec flow.
3. **Phase C last** (multi-UID mapping) — most complex. Requires sync pipe between parent
   and child, external helper invocation, `/etc/subuid` parsing. Higher risk of breakage.

### Verification

1. `cargo build` — compiles
2. `cargo test --lib` — unit tests pass (new tests for subuid parsing, cgroup path detection)
3. `cargo clippy -- -D warnings` — clean
4. `cargo fmt --check` — clean
5. Manual rootless tests:
   - Phase D: `remora run alpine /bin/ls -la /dev/` — shows minimal device set, not full host /dev
   - Phase D: `remora run alpine /bin/sh -c 'echo test > /dev/null'` — works
   - Phase E: `remora run --memory 128m alpine /bin/sh -c 'cat /sys/fs/cgroup/memory.max'` — shows limit (on systemd with delegation)
   - Phase C: `remora run alpine /bin/ls -la /etc/` — files show correct ownership (not all nobody)
   - Phase C: `cat /etc/subuid` confirms subordinate ranges exist for user
6. Root mode unchanged: `sudo remora run alpine /bin/echo hello` still works

### Risks / Notes

- **Phase C — newuidmap availability:** Not all systems have `newuidmap` installed (package
  `uidmap` on Debian, `shadow` on Arch). Falls back to single-UID map gracefully.
- **Phase C — subuid not configured:** Fresh installs may not have `/etc/subuid` entries for
  the user. `usermod --add-subuids` or manual edit required. Clear error message.
- **Phase C — sync pipe complexity:** Adding parent↔child synchronization to the already-
  complex pre_exec closures is the riskiest part. Must be applied to both `spawn()` and
  `spawn_interactive()`.
- **Phase D — devpts newinstance:** May not work on all kernels in user namespaces. Fall back
  to skipping `/dev/pts` mount.
- **Phase E — delegation not universal:** Older systemd or non-systemd systems won't have
  cgroup delegation. Falls back to current behavior (skip gracefully, warn).
- **Phase E — controller availability:** Some controllers may not be delegated. Check
  `cgroup.controllers` before attempting writes.
- **Pre_exec duplication:** All changes must be applied to both `spawn()` and
  `spawn_interactive()`. This is ongoing tech debt.

---

## Previous Releases

### v0.1.0 — Initial Release

GitHub Actions CI + release workflows, CHANGELOG, install script. Full feature set:
namespaces, seccomp, capabilities, cgroups v2, overlay, networking (loopback/bridge/NAT/
port mapping/DNS/pasta), OCI image pull, container exec, OCI runtime compliance,
interactive PTY.

---

## Planned (Deferred)

### AppArmor / SELinux — MAC Profile Support

Deferred: the seccomp + capabilities + masked paths stack is already solid, and MAC requires
system-side setup (profile loading) that most users won't have. Revisit if there's demand.
