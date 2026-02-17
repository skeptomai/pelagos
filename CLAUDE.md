# Remora - Linux Container Runtime

## ⚠️ CRITICAL RULES FOR CLAUDE ⚠️

### ❌ NEVER RUN SUDO COMMANDS
**YOU CANNOT RUN SUDO** - The user MUST run sudo commands themselves.

**What NOT to do:**
- ❌ `sudo cargo test`
- ❌ `sudo -E cargo run`
- ❌ `sudo ./script.sh`
- ❌ ANY command starting with `sudo`

**What TO do instead:**
- ✅ Tell the user: "Please run: sudo -E cargo test --test integration_tests"
- ✅ Explain what the command will do
- ✅ Wait for user to run it and report results

### Document Every Integration Test
**When writing a new integration test, you MUST also add its entry to `docs/INTEGRATION_TESTS.md` in the same change.**

The entry must include:
- The function name as a heading
- Whether it requires root and/or rootfs
- What it actually asserts and why — not just what the code does, but what failure would indicate

This is a hard requirement, not optional cleanup.

### Ask Before Major Decisions
- API design choices
- Adding new features not explicitly requested
- Architectural changes
- When uncertain about the right approach

### No Time Estimates
**NEVER include time estimates** in any documentation or planning:
- ❌ "~3 weeks", "1-2 weeks", "3 days"
- ✅ Use: "Quick", "Moderate Effort", "Significant Work"

---

## Project Overview

Remora is a modern, lightweight Linux container runtime written in Rust. It provides a safe, ergonomic API for creating containerized processes using Linux namespaces, seccomp filtering, capabilities, and resource limits.

## Current State (Updated Feb 17, 2026)

### ✅ Completed Features

**Core Isolation:**
- Linux namespaces: UTS, Mount, IPC, User, Net, Cgroup (6/7)
- PID namespace (works in library, architectural limitation in CLI)
- Filesystem isolation: chroot and pivot_root
- Automatic mounts: /proc, /sys, /dev

**Security (Phase 1 COMPLETE ✅):**
- **Seccomp filtering**: Docker's default profile + minimal profile
- **No-new-privileges**: Prevent setuid/setgid escalation
- **Read-only rootfs**: Immutable filesystem
- **Masked paths**: Hide sensitive kernel info
- **Capability management**: Drop/keep specific capabilities
- **Resource limits**: rlimits for memory, CPU, file descriptors

**Interactive Containers (Phase 2 COMPLETE ✅):**
- **PTY support**: `spawn_interactive()` allocates a PTY pair via `openpty()`
- **Session isolation**: `setsid()` + `TIOCSCTTY` gives container its own session
- **Raw-mode relay**: `InteractiveSession::run()` polls stdin↔master, 100ms timeout
- **Window resize**: `SIGWINCH` handler syncs terminal size to PTY via `TIOCSWINSZ`
- **Terminal restore**: `TerminalGuard` RAII ensures raw mode is always cleaned up
- **`src/pty.rs`**: relay loop, `TerminalGuard`, `InteractiveSession`

**Advanced Resource Management (Phase 5 COMPLETE ✅):**
- **Cgroups v2**: `with_cgroup_memory()`, `with_cgroup_cpu_shares()`, `with_cgroup_cpu_quota()`, `with_cgroup_pids_limit()`
- **Auto-detection**: `cgroups-rs` auto-detects v1 vs v2 via `hierarchies::auto()`
- **Resource stats**: `child.resource_stats()` returns memory, CPU, and PID stats
- **Automatic cleanup**: cgroup deleted in `wait()` / `wait_with_output()`
- **Coexists with rlimits**: both mechanisms work independently

**Filesystem Flexibility (Phase 4 COMPLETE ✅):**
- **Bind mounts**: `with_bind_mount()` (RW) and `with_bind_mount_ro()` (RO) — map host dirs into container
- **tmpfs mounts**: `with_tmpfs()` — in-memory writable scratch space (works with read-only rootfs)
- **Named volumes**: `Volume::create/open/delete` backed by `/var/lib/remora/volumes/<name>/`; `with_volume()` builder method

**Networking (Phase 6 IN PROGRESS 🔄):**
- **N1 Loopback**: `with_network(NetworkMode::Loopback)` — isolated NET namespace, lo brought up via ioctl (127.0.0.1 active)
- **N2 Bridge**: `with_network(NetworkMode::Bridge)` — veth pair + `remora0` bridge (172.19.0.x/24), IPAM via `/run/remora/next_ip`
- **Automatic cleanup**: veth pair deleted in `wait()` / `wait_with_output()`
- **`src/network.rs`**: `NetworkMode`, `bring_up_loopback()`, `setup_bridge_network()`, `teardown_network()`
- N3 (NAT), N4 (port mapping), N5 (DNS) — pending

**Advanced:**
- UID/GID mapping for user namespaces
- Namespace joining (attach to existing namespaces)
- Ergonomic builder API

### 📁 File Structure

```
src/
  lib.rs                  # Library entry point
  main.rs                 # CLI binary
  container.rs            # Main API (~1950 lines)
  cgroup.rs               # Cgroups v2 resource management
  network.rs              # Native networking (N1 loopback, N2 bridge)
  seccomp.rs              # Seccomp-BPF filtering (~400 lines)
  pty.rs                  # PTY relay, TerminalGuard, InteractiveSession

tests/
  integration_tests.rs    # 35 integration tests (require root)

examples/
  seccomp_demo.rs         # Seccomp demonstration

Documentation:
  README.md                             # Project overview
  CLAUDE.md                             # This file
  docs/ROADMAP.md                       # Development plan (NO time estimates!)
  docs/INTEGRATION_TESTS.md            # Every integration test documented
  docs/RUNTIME_COMPARISON.md            # vs Docker/runc/Podman
  docs/SECCOMP_DEEP_DIVE.md            # Seccomp implementation details
  docs/CGROUPS.md                       # Cgroups v1 vs v2 analysis
  docs/PTY_DEEP_DIVE.md                # PTY/interactive session design
  docs/BUILD_ROOTFS.md                  # How to build the Alpine rootfs
```

## Dependencies

### Current Dependencies (Cargo.toml)

```toml
log = "*"
env_logger = "*"
nix = { version = "0.31.1", features = ["process", "sched", "mount", "fs", "term", "poll", "signal", "ioctl"] }
libc = "*"
clap = { version = "3.1.6", features = ["derive"] }
thiserror = "2.0"
bitflags = "2.6"
cgroups-rs = "0.5.0"      # For future cgroup management
seccompiler = "0.5.0"     # Pure Rust seccomp-BPF (Firecracker)
```

**Removed dependencies:**
- ~~unshare~~ - Replaced with custom implementation using nix
- ~~subprocess~~ - Never used
- ~~cgroups-fs~~ - Replaced with cgroups-rs
- ~~palaver~~ - Never used

## Root Filesystem

Remora requires an Alpine Linux rootfs to run containers.

**Two build options:**

1. **With Docker** (recommended):
   ```bash
   ./build-rootfs-docker.sh
   ```

2. **Without Docker** (tarball):
   ```bash
   ./build-rootfs-tarball.sh
   ```

See `BUILD_ROOTFS.md` for detailed instructions.

## Usage Examples

### Basic Container
```rust
use remora::container::{Command, Namespace, Stdio};

let mut child = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
    .with_proc_mount()
    .with_seccomp_default()      // Docker's seccomp profile
    .drop_all_capabilities()     // Least privilege
    .spawn()?;

child.wait()?;
```

### Interactive Container (PTY)
```rust
use remora::container::{Command, Namespace};

let session = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_proc_mount()
    .spawn_interactive()?;

// Blocks: relays stdin/stdout, forwards SIGWINCH, restores terminal on exit
let status = session.run()?;
```

### Running Examples (User Must Run)
```bash
# User runs:
sudo -E cargo run --example seccomp_demo
# Interactive shell:
sudo -E cargo run -- --rootfs alpine-rootfs --exe /bin/sh --uid 0 --gid 0
```

## Testing

### Unit Tests (No Root Required)
```bash
cargo test --lib
```

### Integration Tests (Require Root)
Tell user to run:
```bash
sudo -E cargo test --test integration_tests
```

## Architecture

### Pre-exec Hook Order (Critical!)
The spawn process has a carefully orchestrated setup:

1. **Parent process** (before fork):
   - Open namespace files (can't do in pre_exec)
   - Compile seccomp BPF filter (requires allocation)

2. **Fork**: Create child process

3. **Pre-exec hook** (in child, before exec):
   1. Unshare namespaces
   2. Make mounts private (if MOUNT namespace)
   3. Set up UID/GID mappings (if USER namespace)
   4. Set UID/GID
   5. Change root (chroot or pivot_root)
   6. Mount filesystems (/proc, /sys, /dev)
   7. Drop capabilities
   8. Set resource limits
   9. Run user pre_exec callback
   10. Join existing namespaces (setns)
   11. **Apply seccomp filter (MUST BE LAST!)**

4. **Exec**: Replace with target program

**Why seccomp is last:** Many syscalls needed for setup (mount, setuid) would be blocked if applied earlier.

## Development Workflow

### Making Changes
1. Write code
2. Run unit tests: `cargo test --lib`
3. Build: `cargo build`
4. Tell user to run integration tests if relevant

### Adding Features
1. Ask user if uncertain about approach
2. Implement in src/
3. Add tests
4. Update README.md
5. Add example if appropriate

### Documentation
- Keep concise and practical
- Focus on "how to use" over theory
- Provide working examples
- Update README when adding major features

## Next Steps (from ROADMAP.md)

**Phase 1 - Security Hardening: COMPLETE ✅**
- ✅ Seccomp filtering
- ✅ Read-only rootfs (MS_RDONLY via bind-mount + remount)
- ✅ Masked paths (/proc/kcore, /sys/firmware, etc.)
- ✅ No new privileges (PR_SET_NO_NEW_PRIVS)
- ✅ Capability management
- ✅ Resource limits (rlimits)

**Phase 2 - Interactive Containers: COMPLETE ✅**
- ✅ PTY support (`spawn_interactive()`, `InteractiveSession::run()`)
- ✅ SIGWINCH forwarding (window resize)
- ✅ Session isolation (setsid + TIOCSCTTY)

**Phase 5 - Advanced Resource Management: COMPLETE ✅**
- ✅ Cgroups v2 memory limit — `with_cgroup_memory(bytes)`
- ✅ Cgroups v2 CPU shares/weight — `with_cgroup_cpu_shares(weight)`
- ✅ Cgroups v2 CPU quota — `with_cgroup_cpu_quota(quota_us, period_us)`
- ✅ Cgroups v2 PID limit — `with_cgroup_pids_limit(max)`
- ✅ Resource stats — `child.resource_stats()`
- ✅ Automatic cgroup cleanup on `wait()`

**Phase 4 - Filesystem Flexibility: COMPLETE ✅**
- ✅ Bind mounts (RW and RO) — `with_bind_mount()`, `with_bind_mount_ro()`
- ✅ tmpfs mounts — `with_tmpfs()`
- ✅ Named volumes — `Volume::create/open/delete`, `with_volume()`

**Phase 3 - Networking:**
- CNI integration (delegate to external tools)

See docs/ROADMAP.md for full plan (no time estimates!)

## Common Issues

### "alpine-rootfs not found"
Run: `./fix-rootfs.sh` (requires Docker + sudo)

### Integration tests fail
User must run with: `sudo -E cargo test --test integration_tests`

### Permission denied
Many features require root or CAP_SYS_ADMIN

## Comparison to Docker/runc

| Feature | Remora | Docker |
|---------|--------|--------|
| Namespaces | ✅ 6/7 | ✅ All |
| Seccomp | ✅ Docker profile | ✅ |
| Capabilities | ✅ | ✅ |
| Resource limits | ✅ rlimits + cgroups v2 | ✅ cgroups |
| TTY/PTY | ✅ PTY relay | ✅ |
| Bind mounts | ✅ RW + RO | ✅ |
| tmpfs mounts | ✅ | ✅ |
| Named volumes | ✅ | ✅ |
| Networking | 🔄 Loopback + Bridge (N1/N2) | ✅ Native libnetwork |
| OCI Compatible | ❌ | ✅ |

**Current parity: ~60% of runc features**
