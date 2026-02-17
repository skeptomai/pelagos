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

## Current State (Updated Feb 16, 2026)

### ✅ Completed Features

**Core Isolation:**
- Linux namespaces: UTS, Mount, IPC, User, Net, Cgroup (6/7)
- PID namespace (works in library, architectural limitation in CLI)
- Filesystem isolation: chroot and pivot_root
- Automatic mounts: /proc, /sys, /dev

**Security:**
- **Seccomp filtering**: Docker's default profile + minimal profile
- **Capability management**: Drop/keep specific capabilities
- **Resource limits**: rlimits for memory, CPU, file descriptors

**Advanced:**
- UID/GID mapping for user namespaces
- Namespace joining (attach to existing namespaces)
- Ergonomic builder API

### 📁 File Structure

```
src/
  lib.rs                  # Library entry point
  main.rs                 # CLI binary
  container.rs            # Main API (~1200 lines)
  seccomp.rs              # Seccomp-BPF filtering (~400 lines)

tests/
  integration_tests.rs    # 17 integration tests (require root)

examples/
  seccomp_demo.rs         # Seccomp demonstration

Documentation:
  README.md                      # Project overview
  CLAUDE.md                      # This file
  ROADMAP.md                     # Development plan (NO time estimates!)
  SECCOMP_DEEP_DIVE.md          # Seccomp implementation details
  SECCOMP_IMPLEMENTATION.md      # What was implemented
  CGROUPS.md                     # Cgroups v1 vs v2 analysis
  RUNTIME_COMPARISON.md          # vs Docker/runc/Podman
```

## Dependencies

### Current Dependencies (Cargo.toml)

```toml
log = "*"
env_logger = "*"
nix = { version = "0.31.1", features = ["process", "sched", "mount", "fs"] }
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

### Running Examples (User Must Run)
```bash
# User runs:
sudo -E cargo run --example seccomp_demo
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

**Phase 1 - Security Hardening:**
- ✅ Seccomp filtering - COMPLETE
- ⏳ Read-only rootfs (MS_RDONLY)
- ⏳ Masked paths (/proc/kcore, /sys/firmware)
- ⏳ No new privileges (PR_SET_NO_NEW_PRIVS)

**Phase 2 - Interactive Containers:**
- TTY/PTY support
- Signal handling

**Phase 3 - Networking:**
- CNI integration (delegate to external tools)

See ROADMAP.md for full plan (no time estimates!)

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
| Resource limits | ✅ rlimits | ✅ cgroups |
| TTY/PTY | ❌ Planned | ✅ |
| Networking | ⚠️ Join only | ✅ CNI |
| OCI Compatible | ❌ | ✅ |

**Current parity: ~35% of runc features**
