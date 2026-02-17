# Cgroups in Remora: Analysis and Strategy

**Date:** 2026-02-16
**Status:** Research complete, dependency added for future expansion
**System:** cgroups v2 (pure unified hierarchy)

---

## Executive Summary

This document explains our approach to Linux control groups (cgroups) in Remora, compares cgroups v1 vs v2, analyzes available Rust libraries, and outlines our implementation strategy.

**Current Status:**
- ✅ Using `rlimits` for basic resource limits (FDs, memory, CPU time)
- ✅ `cgroups-rs` dependency added for future expansion
- ✅ `Namespace::CGROUP` available for cgroup namespace isolation
- ⏳ Full cgroup management API to be implemented when needed

**Philosophy:** Match Docker/runc - provide the tools but don't set defaults. Let users configure what they need.

---

## Table of Contents

1. [Cgroups v1 vs v2](#cgroups-v1-vs-v2)
2. [Current Implementation: rlimits](#current-implementation-rlimits)
3. [Why Not Full Cgroups Yet?](#why-not-full-cgroups-yet)
4. [Rust Library Analysis](#rust-library-analysis)
5. [What Container Runtimes Do](#what-container-runtimes-do)
6. [Future Implementation Plan](#future-implementation-plan)
7. [Migration Path](#migration-path)
8. [References](#references)

---

## Cgroups v1 vs v2

### Cgroups v1 (Legacy)

**Architecture:**
- **Multiple hierarchies** - Each controller (CPU, memory, I/O, devices) has its own separate mount point
- **Separate trees** - Different resource types managed independently
- **Complex paths** - `/sys/fs/cgroup/memory/`, `/sys/fs/cgroup/cpu/`, etc.

**Problems:**
- Inconsistent controller behavior
- Thread vs process confusion
- Complex delegation model
- Difficult to manage

**Example v1 structure:**
```
/sys/fs/cgroup/
├── memory/
│   └── docker/
│       └── <container-id>/
│           └── memory.limit_in_bytes
├── cpu/
│   └── docker/
│       └── <container-id>/
│           └── cpu.shares
└── blkio/
    └── docker/
        └── <container-id>/
            └── blkio.weight
```

### Cgroups v2 (Modern - Unified Hierarchy)

**Architecture:**
- **Single unified hierarchy** - One tree for all controllers
- **Consistent behavior** - All controllers work the same way
- **Simple path** - Everything under `/sys/fs/cgroup/`

**Improvements:**
- ✅ Simplified management (single tree)
- ✅ Consistent controller APIs
- ✅ Better delegation model (unprivileged users can manage cgroups safely)
- ✅ Process-only control (no thread confusion)
- ✅ New features:
  - **PSI (Pressure Stall Information)** - Advanced monitoring
  - **eBPF integration** - Programmable control
  - **Better security** - Safer delegation

**Example v2 structure:**
```
/sys/fs/cgroup/
└── user.slice/
    └── user-1000.slice/
        └── session-3.scope/
            └── docker-<container-id>.scope/
                ├── memory.max
                ├── memory.current
                ├── cpu.max
                ├── cpu.weight
                ├── io.max
                └── pids.max
```

### Distribution Timeline

| Distribution | v1 Support | v2 Support | Default |
|-------------|-----------|-----------|---------|
| RHEL 6-7 | ✅ | ❌ | v1 |
| RHEL 8-9 | ✅ | ✅ | v1 |
| RHEL 10+ | ❌ | ✅ | v2 |
| Fedora 31+ | ✅ | ✅ | v2 |
| Ubuntu 21.10+ | ✅ | ✅ | v2 |
| Arch Linux (current) | ✅ | ✅ | v2 |

**Your system:** ✅ cgroups v2 (pure unified hierarchy)

---

## Current Implementation: rlimits

### What We Use Now

Remora currently uses **rlimits** (resource limits) for basic resource control:

```rust
Command::new("/bin/sh")
    .with_max_fds(1024)                  // RLIMIT_NOFILE
    .with_memory_limit(512 * 1024 * 1024)  // RLIMIT_AS
    .with_cpu_time_limit(300)            // RLIMIT_CPU
    .spawn()?;
```

### rlimits vs Cgroups

| Feature | rlimits (current) | cgroups v2 |
|---------|------------------|------------|
| **Memory limit** | ✅ RLIMIT_AS | ✅ memory.max |
| **CPU time** | ✅ RLIMIT_CPU (seconds) | ✅ cpu.max (quota/period) |
| **File descriptors** | ✅ RLIMIT_NOFILE | ❌ |
| **Process count** | ✅ RLIMIT_NPROC | ✅ pids.max |
| **File size** | ✅ RLIMIT_FSIZE | ❌ |
| **I/O bandwidth** | ❌ | ✅ io.max, io.weight |
| **Network priority** | ❌ | ✅ via eBPF |
| **Device access** | ❌ | ✅ devices.allow/deny |
| **CPU shares** | ❌ | ✅ cpu.weight |
| **Accounting** | ❌ | ✅ Full statistics |
| **Hierarchical limits** | ❌ | ✅ Tree-based |
| **Scope** | Per-process | Per-cgroup (process group) |
| **Persistence** | No | Yes (survives process) |

### Why rlimits Work Well for Now

**Advantages:**
- ✅ Simple API
- ✅ No extra dependencies
- ✅ Covers basic use cases
- ✅ Matches Unix philosophy
- ✅ Per-process granularity

**Limitations:**
- ❌ No I/O bandwidth control
- ❌ No hierarchical limits
- ❌ No accounting/statistics
- ❌ No device access control
- ❌ Limited CPU control (time, not shares)

---

## Why Not Full Cgroups Yet?

### Design Philosophy

**We follow the Docker/runc approach:**

1. **Don't set defaults** - Container runtimes shouldn't impose arbitrary limits
2. **Let users decide** - Users know their workload requirements
3. **Start simple** - Add complexity only when needed
4. **Be composable** - Work well with external tools

### What Docker/runc Actually Do

**By default:** Docker and runc do **NOT** set cgroup limits!

```bash
# This container has NO resource limits
docker run alpine sh

# Users explicitly configure what they need
docker run --memory 256m --cpus 0.5 --pids-limit 100 alpine sh
```

**OCI Runtime Specification says:**

> "Do not specify resources unless limits have to be updated."
>
> "If the value is not specified, the runtime MAY define the default cgroups path."

**Translation:** The spec explicitly says NOT to set defaults.

### When You Need Full Cgroups

Add cgroup management when you need:

1. **I/O bandwidth limits** - Prevent disk/network hogging
2. **CPU shares** - Weighted CPU scheduling across containers
3. **Device control** - Whitelist/blacklist devices
4. **Advanced monitoring** - PSI (Pressure Stall Information)
5. **Hierarchical limits** - Parent groups controlling child groups
6. **Better accounting** - Track actual resource usage

**Current status:** None of these are critical for basic containerization.

---

## Rust Library Analysis

### Libraries Evaluated

| Library | Version | v1 Support | v2 Support | Maintained | Recommendation |
|---------|---------|-----------|-----------|-----------|---------------|
| `cgroups-rs` | 0.5.0 | ✅ | ✅ | ✅ Active (Kata) | ⭐ **CHOSEN** |
| `cgroups-fs` | 1.2.0 | ✅ | ❌ | ⚠️ Limited | ❌ No v2 |
| `controlgroup` | 0.3.0 | ✅ | ✅ | ⚠️ Less active | ❌ Less mature |
| `libflux` | 0.1.0 | ✅ | ✅ | ⚠️ New | ❌ Full runtime (too heavy) |

### Why cgroups-rs?

**Repository:** https://github.com/kata-containers/cgroups-rs
**Crate:** https://crates.io/crates/cgroups-rs

**Advantages:**
- ✅ **Dual support** - Works with both v1 and v2
- ✅ **Auto-detection** - Automatically detects which version is available
- ✅ **Production-ready** - Used by Kata Containers (Intel/Cloud Native Foundation)
- ✅ **Native Rust** - No FFI overhead
- ✅ **Builder pattern** - Matches our API style
- ✅ **Active maintenance** - Regular updates
- ✅ **Comprehensive** - Supports all major controllers

**API Example:**
```rust
use cgroups_rs::Cgroup;
use cgroups_rs::memory::MemController;
use cgroups_rs::cpu::CpuController;

// Create cgroup
let cg = Cgroup::new("mycontainer")?;

// Set memory limit
let mem: &MemController = cg.controller_of().unwrap();
mem.set_limit(512 * 1024 * 1024)?;  // 512 MB

// Set CPU quota
let cpu: &CpuController = cg.controller_of().unwrap();
cpu.set_shares(512)?;  // Relative weight

// Add process to cgroup
cg.add_task_by_tgid(pid)?;

// Cleanup when done
cg.delete()?;
```

**Important Note:** The `Cgroup` struct does **not** implement `Drop`, so you must explicitly call `delete()` to clean up the cgroup. This is intentional - cgroups often need to persist beyond the process lifetime.

---

## What Container Runtimes Do

### Docker Default Behavior

**Without limits:**
```bash
docker run alpine sh
# No cgroups limits set - container uses host resources
```

**With limits:**
```bash
docker run \
    --memory 256m \
    --memory-swap 512m \
    --cpus 0.5 \
    --pids-limit 100 \
    --device-read-bps /dev/sda:1mb \
    alpine sh
```

**runc translates this to (cgroups v2):**
```
/sys/fs/cgroup/docker-<id>/
├── memory.max = 268435456          # 256 MB
├── memory.swap.max = 268435456     # Additional 256 MB swap
├── cpu.max = "50000 100000"        # 50% of one core (50ms per 100ms)
├── pids.max = 100
└── io.max = "8:0 rbps=1048576"     # 1 MB/s read from /dev/sda
```

### Cgroup Namespace Mode

**Docker defaults:**
- **cgroups v2:** `--cgroupns=private` (isolated cgroup view)
- **cgroups v1:** `--cgroupns=host` (see host cgroups)

**What this means:**
- `private`: Container sees only its own cgroup subtree (more isolated)
- `host`: Container sees all host cgroups (less isolated, more debugging)

**Remora:** We have `Namespace::CGROUP` available for private mode.

### What runc Does

**Process:**
1. Read OCI config.json
2. **If** resources specified, create cgroup hierarchy
3. Configure requested limits
4. Add container PID to cgroup
5. Start container process

**Key point:** runc only creates/configures cgroups **if explicitly requested**.

---

## Future Implementation Plan

### Phase 1: Basic Cgroup Support (When Needed)

**Goal:** Create cgroup, add process, basic limits

**API Design:**
```rust
use cgroups_rs::Cgroup;

let cgroup = Cgroup::new("remora_container_12345")?;

let child = Command::new("/bin/sh")
    .with_namespaces(Namespace::MOUNT | Namespace::CGROUP)
    .with_chroot("/path/to/rootfs")
    .with_cgroup(cgroup)  // New method
    .spawn()?;

// Cgroup is automatically configured with child PID
// and deleted when child exits
```

**Implementation:**
```rust
pub struct Command {
    // ... existing fields
    cgroup: Option<Cgroup>,
}

impl Command {
    pub fn with_cgroup(mut self, cgroup: Cgroup) -> Self {
        self.cgroup = Some(cgroup);
        self
    }
}

// In spawn():
// 1. Create child process
// 2. Add child PID to cgroup
// 3. Continue with exec
```

### Phase 2: Resource Limit API (Future)

**Goal:** Ergonomic API for common limits

```rust
let child = Command::new("/bin/sh")
    .with_namespaces(Namespace::MOUNT | Namespace::CGROUP)
    .with_chroot("/path/to/rootfs")
    // New cgroup-based methods
    .with_cgroup_memory_limit(512 * 1024 * 1024)  // 512 MB
    .with_cgroup_cpu_quota(50, 100)  // 50% of one core
    .with_cgroup_cpu_shares(512)     // Relative weight
    .with_cgroup_pids_limit(100)
    .with_cgroup_io_weight(500)
    .spawn()?;
```

**This would coexist with rlimits:**
- rlimits: per-process limits
- cgroups: per-container limits (process group)

### Phase 3: Advanced Features (Far Future)

**Possible additions:**
- Device whitelisting
- Network priority
- PSI monitoring
- eBPF program attachment
- Hierarchical cgroup management

---

## Migration Path

### Current State
```rust
// Users can do basic limits today
let child = Command::new("/bin/sh")
    .with_max_fds(1024)           // rlimit
    .with_memory_limit(512_000_000)  // rlimit
    .with_cpu_time_limit(300)     // rlimit
    .spawn()?;
```

### Future State (Backward Compatible)
```rust
// rlimits still work
let child = Command::new("/bin/sh")
    .with_max_fds(1024)  // Still valid!

    // NEW: cgroup-based limits (optional)
    .with_cgroup_memory_limit(512_000_000)
    .with_cgroup_cpu_quota(50, 100)
    .spawn()?;

// Can use both:
// - rlimits for per-process limits
// - cgroups for container-wide limits
```

**No breaking changes** - All existing code continues to work.

---

## Implementation Checklist

When we add full cgroup support:

- [ ] Add `with_cgroup()` method
- [ ] Implement automatic PID addition
- [ ] Handle cgroup cleanup (explicit `delete()`)
- [ ] Add convenience methods for common limits
- [ ] Update documentation
- [ ] Add integration tests
- [ ] Handle cgroups v1 vs v2 differences
- [ ] Implement proper error handling
- [ ] Consider rootless containers (unprivileged cgroups)
- [ ] Add examples to README

---

## Current Status

**Dependency:** ✅ Added `cgroups-rs = "0.5.0"`
**Reason:** Future expansion - when we need advanced resource management
**Implementation:** ⏳ To be added when needed
**Current approach:** ✅ rlimits working well for basic use cases

**When to implement:**
- User requests I/O limits
- Need better resource accounting
- Want CPU shares (not just time limits)
- Need device access control
- Building toward production-grade orchestration

---

## References

### Documentation
- [Kubernetes cgroups v2 docs](https://kubernetes.io/docs/concepts/architecture/cgroups/)
- [cgroups(7) Linux man page](https://man7.org/linux/man-pages/man7/cgroups.7.html)
- [OCI Runtime Spec v1.1](https://opencontainers.org/posts/blog/2023-07-21-oci-runtime-spec-v1-1/)
- [OCI Runtime Spec - Linux Config](https://github.com/opencontainers/runtime-spec/blob/main/config-linux.md)

### Articles & Analysis
- [Cgroups v1 vs v2: Critical Evolution](https://thamizhelango.medium.com/cgroups-v1-vs-v2-the-critical-evolution-for-modern-containerization-472224dc97c9)
- [RHEL cgroups v1 to v2 Migration](https://access.redhat.com/articles/3735611)
- [Datadog: Container Security Fundamentals - Cgroups](https://securitylabs.datadoghq.com/articles/container-security-fundamentals-part-4/)
- [Using runc to explore OCI Runtime Spec](https://frasertweedale.github.io/blog-redhat/posts/2021-05-27-oci-runtime-spec-runc.html)

### Docker & Runtime Docs
- [Docker Resource Constraints](https://docs.docker.com/engine/containers/resource_constraints/)
- [Docker Runtime Metrics](https://docs.docker.com/engine/containers/runmetrics/)
- [Docker Daemon Configuration](https://docs.docker.com/reference/cli/dockerd/)
- [Rootless Containers - cgroup v2](https://rootlesscontaine.rs/getting-started/common/cgroup2/)

### Rust Libraries
- [cgroups-rs on crates.io](https://crates.io/crates/cgroups-rs)
- [cgroups-rs GitHub repo](https://github.com/kata-containers/cgroups-rs)
- [cgroups-rs documentation](https://docs.rs/cgroups-rs/latest/cgroups_rs/)

### Related Projects
- [runc (Go)](https://github.com/opencontainers/runc)
- [Kata Containers](https://github.com/kata-containers/kata-containers)
- [Building a Container from Scratch in Rust](https://brianshih1.github.io/mini-container/resource_restrictions.html)

---

## Conclusion

**Current Approach:** ✅ **Correct and Sufficient**

We're following industry best practices:
1. ✅ Using rlimits for basic resource limits
2. ✅ Not setting arbitrary defaults
3. ✅ Providing `Namespace::CGROUP` for isolation
4. ✅ Added `cgroups-rs` dependency for future expansion
5. ✅ System running modern cgroups v2

**Next Steps:**
- Continue with current rlimits approach
- Implement full cgroup API when users request advanced features
- Focus on other features (networking, volumes, etc.)

**Philosophy:** Ship working software, add complexity when needed, not before.

---

**Last Updated:** 2026-02-16
**Author:** Research and analysis for remora container runtime
**Status:** Dependency added, implementation deferred until needed
