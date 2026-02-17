# Remora vs Established Container Runtimes

**Date:** 2026-02-16
**Status:** Feature comparison analysis
**Compared Against:** runc (OCI reference), Docker, Podman, crun

---

## Executive Summary

**What Remora Is:**
- ✅ Low-level container runtime library (like runc)
- ✅ Focused on Linux namespaces and isolation
- ✅ API for building container tools
- ✅ Educational and lightweight

**What Remora Is NOT:**
- ❌ Not a complete container platform (like Docker)
- ❌ Not a CLI replacement for Docker (like Podman)
- ❌ Not OCI-compliant (yet)
- ❌ Not production-grade orchestration

**Comparison Level:**
- **Primary:** runc (low-level OCI runtime)
- **Secondary:** Docker/Podman (high-level platforms)

---

## Feature Matrix

### Legend
- ✅ **Implemented** - Feature works
- ⚠️ **Partial** - Basic implementation, missing advanced features
- 🔧 **Planned** - Dependency added, not yet implemented
- ❌ **Not Implemented** - Not available
- 🚫 **Out of Scope** - Not planned

| Feature Category | Remora | runc | Docker | Podman | Notes |
|------------------|--------|------|--------|--------|-------|
| **Core Isolation** |
| UTS namespace | ✅ | ✅ | ✅ | ✅ | Hostname isolation |
| Mount namespace | ✅ | ✅ | ✅ | ✅ | Filesystem isolation |
| PID namespace | ❌ | ✅ | ✅ | ✅ | Requires double-fork (see PID_NAMESPACE_ISSUE.md) |
| Network namespace | ⚠️ | ✅ | ✅ | ✅ | Can join, can't create isolated network |
| IPC namespace | ✅ | ✅ | ✅ | ✅ | IPC isolation |
| User namespace | ⚠️ | ✅ | ✅ | ✅ | API exists, rootless not fully implemented |
| Cgroup namespace | ✅ | ✅ | ✅ | ✅ | Cgroup view isolation |
| **Filesystem** |
| chroot | ✅ | ✅ | ✅ | ✅ | Basic root filesystem change |
| pivot_root | ✅ | ✅ | ✅ | ✅ | More secure than chroot |
| Auto /proc mount | ✅ | ✅ | ✅ | ✅ | Automatic proc filesystem |
| Auto /sys mount | ✅ | ✅ | ✅ | ✅ | Automatic sysfs |
| Auto /dev mount | ✅ | ✅ | ✅ | ✅ | Automatic device filesystem |
| Volumes | ❌ | ✅ | ✅ | ✅ | Persistent storage |
| Bind mounts | ❌ | ✅ | ✅ | ✅ | Mount host directories |
| tmpfs mounts | ❌ | ✅ | ✅ | ✅ | In-memory filesystems |
| Overlay filesystem | ❌ | ✅ | ✅ | ✅ | Layered filesystems |
| **Resource Limits** |
| rlimits | ✅ | ✅ | ✅ | ✅ | Basic per-process limits |
| Memory limit (rlimit) | ✅ | ✅ | ✅ | ✅ | RLIMIT_AS |
| CPU time (rlimit) | ✅ | ✅ | ✅ | ✅ | RLIMIT_CPU |
| FD limit (rlimit) | ✅ | ✅ | ✅ | ✅ | RLIMIT_NOFILE |
| Cgroup v1 | 🔧 | ✅ | ✅ | ✅ | cgroups-rs added, not implemented |
| Cgroup v2 | 🔧 | ✅ | ✅ | ✅ | cgroups-rs added, not implemented |
| CPU quota/shares | 🔧 | ✅ | ✅ | ✅ | Needs cgroup implementation |
| Memory cgroup limits | 🔧 | ✅ | ✅ | ✅ | Needs cgroup implementation |
| I/O bandwidth limits | 🔧 | ✅ | ✅ | ✅ | Needs cgroup implementation |
| Device access control | 🔧 | ✅ | ✅ | ✅ | Needs cgroup implementation |
| **Security** |
| Capabilities | ✅ | ✅ | ✅ | ✅ | 12 common capabilities |
| Drop all caps | ✅ | ✅ | ✅ | ✅ | Maximum security |
| Selective caps | ✅ | ✅ | ✅ | ✅ | Keep specific capabilities |
| Seccomp | ❌ | ✅ | ✅ | ✅ | Syscall filtering |
| AppArmor | ❌ | ✅ | ✅ | ✅ | MAC profiles |
| SELinux | ❌ | ✅ | ✅ | ✅ | MAC labels |
| Read-only rootfs | ❌ | ✅ | ✅ | ✅ | Immutable filesystem |
| No new privileges | ❌ | ✅ | ✅ | ✅ | Prevent privilege escalation |
| Masked paths | ❌ | ✅ | ✅ | ✅ | Hide sensitive paths |
| **Networking** |
| Join netns | ✅ | ✅ | ✅ | ✅ | Join existing network namespace |
| Create isolated network | ❌ | ✅ | ✅ | ✅ | veth pairs, bridges |
| Port mapping | ❌ | 🚫 | ✅ | ✅ | Host port to container |
| DNS configuration | ❌ | ✅ | ✅ | ✅ | Custom resolv.conf |
| Custom networks | ❌ | 🚫 | ✅ | ✅ | User-defined networks |
| Network plugins | ❌ | 🚫 | ✅ | ✅ | CNI plugins |
| **User Management** |
| Set UID/GID | ✅ | ✅ | ✅ | ✅ | Basic user setting |
| UID/GID mapping | ⚠️ | ✅ | ✅ | ✅ | API exists, needs testing |
| Rootless mode | ❌ | ✅ | ⚠️ | ✅ | Run without root |
| Subuid/subgid | ❌ | ✅ | ⚠️ | ✅ | User namespace mapping |
| **OCI Compliance** |
| OCI config.json | ❌ | ✅ | ✅ | ✅ | Standard config format |
| OCI image format | ❌ | ✅ | ✅ | ✅ | Standard image format |
| OCI runtime spec | ❌ | ✅ | ⚠️ | ⚠️ | Full spec compliance |
| **Process Management** |
| Spawn process | ✅ | ✅ | ✅ | ✅ | Basic execution |
| Wait for exit | ✅ | ✅ | ✅ | ✅ | Get exit status |
| Signal handling | ⚠️ | ✅ | ✅ | ✅ | Send signals to container |
| TTY/PTY | ❌ | ✅ | ✅ | ✅ | Interactive shells |
| Attach/detach | ❌ | ✅ | ✅ | ✅ | Background containers |
| Exec into container | ❌ | ✅ | ✅ | ✅ | Run commands in running container |
| **Container Lifecycle** |
| Create | ✅ | ✅ | ✅ | ✅ | Create container |
| Start | ✅ | ✅ | ✅ | ✅ | Start container |
| Stop | ⚠️ | ✅ | ✅ | ✅ | Basic only (via wait) |
| Kill | ❌ | ✅ | ✅ | ✅ | Send SIGKILL |
| Delete | ❌ | ✅ | ✅ | ✅ | Remove container |
| Pause/Resume | ❌ | ✅ | ✅ | ✅ | Freeze container |
| Checkpoint/Restore | ❌ | ✅ | ✅ | ✅ | CRIU integration |
| **Image Management** |
| Pull images | ❌ | 🚫 | ✅ | ✅ | Download from registry |
| Push images | ❌ | 🚫 | ✅ | ✅ | Upload to registry |
| Build images | ❌ | 🚫 | ✅ | ⚠️ | Requires buildah |
| Image layers | ❌ | 🚫 | ✅ | ✅ | Overlay filesystem |
| **Orchestration** |
| Docker Compose | ❌ | 🚫 | ✅ | ✅ | Multi-container apps |
| Kubernetes | ❌ | ⚠️ | ⚠️ | ✅ | Via CRI |
| Swarm mode | ❌ | 🚫 | ✅ | ❌ | Docker-specific |
| **Developer Experience** |
| Library API | ✅ | ❌ | ⚠️ | ⚠️ | Rust library |
| CLI tool | ⚠️ | ✅ | ✅ | ✅ | Basic binary |
| REST API | ❌ | 🚫 | ✅ | ✅ | Remote control |
| Event system | ❌ | ✅ | ✅ | ✅ | Container events |
| Logging | ⚠️ | ✅ | ✅ | ✅ | Basic stdout |
| Metrics | ❌ | ⚠️ | ✅ | ✅ | Resource usage |
| **Testing** |
| Integration tests | ✅ | ✅ | ✅ | ✅ | 12 tests |
| Unit tests | ⚠️ | ✅ | ✅ | ✅ | Some coverage |
| Fuzzing | ❌ | ✅ | ⚠️ | ⚠️ | Security testing |

---

## Detailed Feature Analysis

### 1. Core Isolation (Namespaces)

**What We Have:**
```rust
✅ UTS, MOUNT, IPC, NET, USER, CGROUP namespaces
✅ Can join existing namespaces
✅ Namespace::CGROUP for isolated view
```

**What We're Missing:**
```rust
❌ PID namespace (architectural limitation - requires double-fork)
⚠️ Network namespace creation (can join, can't create isolated network)
⚠️ Full rootless support (USER namespace needs more work)
```

**Comparison to runc:**
- runc: Full PID namespace support with double-fork
- runc: Creates isolated networks with veth pairs
- runc: Complete rootless mode implementation

**Impact:** Medium - PID isolation is nice but not critical for most use cases

---

### 2. Filesystem Features

**What We Have:**
```rust
✅ chroot (basic)
✅ pivot_root (secure alternative)
✅ Automatic /proc, /sys, /dev mounting
✅ Mount namespace isolation
✅ Mount propagation fix (MS_PRIVATE)
```

**What We're Missing:**
```rust
❌ Volume management
❌ Bind mounts
❌ tmpfs mounts
❌ Overlay filesystem (for image layers)
❌ Read-only rootfs
```

**Comparison to runc:**
- runc: Full OCI mount specification
- runc: Supports bind, tmpfs, overlay, devtmpfs, mqueue, etc.
- runc: Read-only root with writable layers

**Impact:** High - Volumes and bind mounts are very common use cases

---

### 3. Resource Limits

**What We Have:**
```rust
✅ rlimits (NOFILE, AS, CPU, NPROC, FSIZE)
✅ API: with_max_fds(), with_memory_limit(), with_cpu_time_limit()
🔧 cgroups-rs dependency added
```

**What We're Missing:**
```rust
❌ cgroup CPU quota/shares (needs implementation)
❌ cgroup memory limits (needs implementation)
❌ cgroup I/O limits (needs implementation)
❌ cgroup device controller (needs implementation)
```

**Comparison to runc:**
- runc: Full cgroup v1 and v2 support
- runc: CPU quota, memory, swap, I/O, devices, PIDs
- runc: Hierarchical limits

**Impact:** Medium - rlimits work well for basic limits, cgroups needed for advanced control

---

### 4. Security Features

**What We Have:**
```rust
✅ Capability management (12 common capabilities)
✅ Drop all capabilities
✅ Selective capability retention
✅ Namespace isolation
```

**What We're Missing:**
```rust
❌ Seccomp (syscall filtering)
❌ AppArmor profiles
❌ SELinux labels
❌ No-new-privileges flag
❌ Masked paths
```

**Comparison to runc:**
- runc: Default seccomp profile (blocks ~300 dangerous syscalls)
- runc: AppArmor/SELinux integration
- runc: Masked paths (/proc/kcore, /sys/firmware, etc.)
- Docker: Default 14 capabilities (vs our 12)
- Podman: Default 11 capabilities (more secure)

**Impact:** High - Seccomp is critical for production security

---

### 5. Networking

**What We Have:**
```rust
✅ Join existing network namespace
✅ Network namespace isolation
```

**What We're Missing:**
```rust
❌ Create isolated networks
❌ veth pair creation
❌ Bridge configuration
❌ Port mapping
❌ DNS configuration
❌ CNI plugins
```

**Comparison to runc:**
- runc: Can create network namespaces (but delegates to CNI)
- Docker: Full networking stack (bridge, host, overlay)
- Podman: CNI plugin support

**Impact:** High - Most containers need network isolation

**Example of what's missing:**
```bash
# Docker can do:
docker run -p 8080:80 nginx  # Port mapping
docker network create mynet  # Custom network

# We can't do this yet
```

---

### 6. User Management & Rootless

**What We Have:**
```rust
✅ Set UID/GID
⚠️ UID/GID mapping API (not fully tested)
✅ USER namespace support
```

**What We're Missing:**
```rust
❌ Rootless mode (run as non-root user)
❌ Subuid/subgid mapping
❌ Unprivileged user namespaces
```

**Comparison:**
- **runc:** Rootless mode available
- **Docker:** Rootless mode added (newer feature)
- **Podman:** Rootless from the start (major selling point)

**Podman's advantage:**
```bash
# Podman as regular user
$ podman run --rm alpine whoami
alpine

# No sudo needed!
```

**Impact:** Medium-High - Rootless is increasingly important for security

---

### 7. OCI Compliance

**What We Have:**
```rust
✅ Rust library API
✅ Basic container execution
```

**What We're Missing:**
```rust
❌ OCI config.json support
❌ OCI image format
❌ OCI runtime hooks
❌ OCI bundle format
```

**Comparison to runc:**
- runc: Full OCI Runtime Spec v1.2.0 compliance
- runc: Reference implementation

**Example OCI config:**
```json
{
  "ociVersion": "1.2.0",
  "process": {
    "args": ["/bin/sh"],
    "capabilities": { "bounding": ["CAP_NET_BIND_SERVICE"] }
  },
  "root": { "path": "rootfs" },
  "linux": {
    "namespaces": [{"type": "pid"}, {"type": "network"}],
    "resources": {
      "memory": { "limit": 536870912 },
      "cpu": { "quota": 50000, "period": 100000 }
    }
  }
}
```

**Impact:** High - OCI compliance needed for interoperability

---

### 8. Process Management

**What We Have:**
```rust
✅ Spawn process
✅ Wait for exit
✅ Get exit status
⚠️ Basic stdio (inherit, null, piped)
```

**What We're Missing:**
```rust
❌ TTY/PTY support (interactive shells)
❌ Attach/detach
❌ Exec into running container
❌ Signal forwarding
❌ Process tree tracking
```

**Comparison:**
```bash
# Docker can do:
docker exec -it mycontainer /bin/sh   # Exec into running container
docker attach mycontainer              # Attach to running container
docker kill -s SIGTERM mycontainer    # Send signals

# We can't do this yet
```

**Impact:** High - Interactive shells are very common

---

### 9. Container Lifecycle

**What We Have:**
```rust
✅ Create (spawn)
✅ Start (spawn)
⚠️ Stop (via wait, no graceful shutdown)
```

**What We're Missing:**
```rust
❌ Pause/Resume (CRIU)
❌ Checkpoint/Restore
❌ Proper kill (SIGTERM then SIGKILL)
❌ Delete/cleanup
❌ State management
```

**runc lifecycle:**
```bash
runc create <id>      # Create container
runc start <id>       # Start init process
runc pause <id>       # Freeze container
runc resume <id>      # Unfreeze
runc kill <id> TERM   # Send signal
runc delete <id>      # Remove container
```

**Impact:** Medium - Basic lifecycle is sufficient for simple use cases

---

### 10. Image Management

**What We Have:**
```bash
❌ Nothing - we work with rootfs directories
```

**What Docker/Podman Have:**
```bash
docker pull nginx                    # Download image
docker build -t myapp .             # Build image
docker push myregistry.com/myapp    # Upload image
```

**Impact:** High - Images are fundamental to container workflows

**Our approach:**
```bash
# We require pre-built rootfs
./remora --rootfs ./alpine-rootfs --exe /bin/sh
```

**Scope:** 🚫 Out of scope - We're a runtime, not an image manager

---

## Summary Comparison

### vs runc (OCI Reference Runtime)

**We match runc in:**
- ✅ Basic namespace isolation (except PID)
- ✅ chroot/pivot_root
- ✅ Basic resource limits (rlimits)
- ✅ Capability management
- ✅ Mount helpers

**runc is significantly ahead in:**
- ❌ PID namespace support
- ❌ Full cgroup integration
- ❌ Seccomp/AppArmor/SELinux
- ❌ OCI spec compliance
- ❌ TTY/PTY support
- ❌ Rootless mode
- ❌ Process lifecycle management

**Percentage complete:** ~35% of runc's features

---

### vs Docker (Full Platform)

**We match Docker in:**
- ✅ Basic container execution
- ✅ Namespace isolation (partial)

**Docker is massively ahead in:**
- ❌ Image management (pull, push, build)
- ❌ Networking (port mapping, custom networks)
- ❌ Volumes
- ❌ Orchestration (Compose, Swarm)
- ❌ Developer experience (CLI, API)
- ❌ Production features (logging, metrics, events)

**Percentage complete:** ~10% of Docker's features

**Fair comparison:** Docker is a full platform, we're a low-level library

---

### vs Podman (Docker Alternative)

**Similar comparison to Docker, but Podman has:**
- ✅ Better rootless support (we don't have this)
- ✅ Daemonless architecture (we have this - we're a library!)
- ✅ More secure defaults (11 caps vs our 12)

**Percentage complete:** ~10% of Podman's features

---

## What Remora Is Good At

### ✅ Strengths

1. **Rust Library API**
   - Type-safe container creation
   - Zero-cost abstractions
   - No daemon required
   - Embeddable in Rust applications

2. **Educational Value**
   - Clear, readable code
   - Well-documented
   - Demonstrates Linux containerization concepts

3. **Lightweight**
   - Minimal dependencies
   - Small binary size
   - Fast compilation

4. **Modern Rust Patterns**
   - Builder pattern
   - Bitflags for namespaces/capabilities
   - thiserror for error handling
   - Comprehensive documentation

5. **Foundational Features**
   - Namespace isolation works well
   - Capability management is solid
   - rlimits cover basic use cases
   - Mount helpers simplify common tasks

### 🎯 Ideal Use Cases

**Good for:**
- 📚 Learning how containers work
- 🔧 Building custom container tools
- 🧪 Experimentation and prototyping
- 🏗️ Foundation for specialized runtimes
- 📦 Embedding containers in Rust applications

**Not good for:**
- ❌ Production workloads (yet)
- ❌ Docker replacement
- ❌ Running existing container images
- ❌ Complex orchestration
- ❌ Enterprise deployments

---

## Roadmap to Feature Parity

### Priority 1: Security ⭐⭐⭐

**Critical for production:**
1. Seccomp filtering
2. AppArmor/SELinux support
3. No-new-privileges flag
4. Read-only rootfs
5. Masked paths

**Why:** Security features are non-negotiable for production

### Priority 2: Networking ⭐⭐

**Essential for real containers:**
1. Create network namespaces
2. veth pair setup
3. Basic bridge networking
4. DNS configuration

**Why:** Isolated containers need networking

### Priority 3: Process Management ⭐⭐

**Quality of life:**
1. TTY/PTY support (interactive shells)
2. Signal handling
3. Exec into containers
4. Proper lifecycle (pause, resume)

**Why:** Users expect interactive shells

### Priority 4: Filesystems ⭐

**Flexibility:**
1. Bind mounts
2. tmpfs
3. Volume management
4. Overlay filesystem

**Why:** Common use cases need these

### Priority 5: Cgroups ⭐

**Resource control:**
1. Implement cgroups v2 API
2. CPU quota/shares
3. Memory limits (cgroup)
4. I/O limits

**Why:** Better resource control than rlimits

### Priority 6: OCI Compliance

**Interoperability:**
1. Parse OCI config.json
2. Support OCI bundles
3. Implement OCI hooks

**Why:** Work with OCI ecosystem

### Priority 7: Rootless Mode

**Security:**
1. Unprivileged user namespaces
2. Subuid/subgid mapping
3. Rootless cgroup delegation

**Why:** Run without root privileges

---

## Conclusion

### Current State

**Remora is:**
- ✅ A functional low-level container library
- ✅ ~35% feature-complete vs runc
- ✅ ~10% feature-complete vs Docker/Podman
- ✅ Suitable for learning and experimentation
- ❌ Not production-ready for general use

### Missing Critical Features

**For production parity with runc, we need:**
1. Seccomp (syscall filtering)
2. PID namespace (requires architecture change)
3. Full networking support
4. TTY/PTY support
5. OCI compliance

**Estimated effort:** 6-12 months of focused development

### Recommendations

**If you want to use Remora for:**

📚 **Learning:** ✅ Perfect as-is
- Demonstrates core concepts clearly
- Well-documented
- Easy to understand

🔧 **Custom Tools:** ✅ Good foundation
- Solid API for building on
- Embeddable in Rust apps
- Flexible architecture

🏭 **Production:** ❌ Not ready
- Missing critical security (seccomp)
- Missing networking features
- Missing process management
- Missing OCI compliance

**Next Steps:**
1. Focus on security (Priority 1)
2. Add networking (Priority 2)
3. Improve process management (Priority 3)
4. Consider OCI compliance for interoperability

---

## References

- [runc GitHub](https://github.com/opencontainers/runc)
- [OCI Runtime Specification](https://github.com/opencontainers/runtime-spec)
- [Docker Security](https://docs.docker.com/engine/security/)
- [Podman vs Docker Comparison](https://last9.io/blog/podman-vs-docker/)
- [Docker Container Run Reference](https://docs.docker.com/reference/cli/docker/container/run/)
- [OCI Runtime Spec Deep Dive](https://mkdev.me/posts/the-tool-that-really-runs-your-containers-deep-dive-into-runc-and-oci-specifications)

---

**Last Updated:** 2026-02-16
**Remora Version:** 0.1.0
**Comparison Baseline:** runc v1.2.0, Docker Engine 27.x, Podman 5.x
