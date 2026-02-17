# Remora

A modern, lightweight Linux container runtime library written in Rust.

Remora provides a safe, ergonomic API for creating containerized processes using
Linux namespaces, seccomp filtering, cgroups v2, and native networking — without
a daemon, without CNI plugins, and without image management.

## Features

### Isolation
- **Namespaces:** UTS, Mount, IPC, Network, User, PID, Cgroup
- **Filesystem:** chroot, pivot_root, automatic /proc /sys /dev mounts
- **Networking:** loopback-only or full bridge (veth + remora0, 172.19.0.x/24)

### Security
- **Seccomp-BPF:** Docker's default profile or a minimal profile, via pure-Rust `seccompiler`
- **No-new-privileges:** `PR_SET_NO_NEW_PRIVS` blocks setuid escalation
- **Read-only rootfs:** `MS_RDONLY` remount makes the filesystem immutable
- **Masked paths:** `/proc/kcore`, `/sys/firmware`, and others hidden with `/dev/null`
- **Capability management:** drop all caps or keep a specific set

### Resource Management
- **rlimits:** memory address space, CPU time, file descriptors, process count
- **Cgroups v2:** memory hard limit, CPU weight, CPU quota, PID limit
- **Resource stats:** `child.resource_stats()` reads live cgroup counters

### Filesystem Flexibility
- **Bind mounts:** `with_bind_mount()` (RW) and `with_bind_mount_ro()` (RO)
- **tmpfs:** `with_tmpfs()` — writable scratch space inside a read-only rootfs
- **Named volumes:** `Volume::create/open/delete`, `with_volume()` — persisted at
  `/var/lib/remora/volumes/<name>/`

### Networking
- **Loopback:** `NetworkMode::Loopback` — isolated NET namespace, `lo` only
- **Bridge:** `NetworkMode::Bridge` — veth pair + `remora0` bridge, IPAM via
  `/run/remora/next_ip` (flock-protected)
- **NAT:** `with_nat()` — nftables MASQUERADE, reference-counted across containers
- **Port mapping / DNS:** N4/N5, coming next

### Interactive Containers
- **PTY:** `spawn_interactive()` allocates a PTY pair via `openpty()`
- **SIGWINCH relay:** terminal resize forwarded to container via `TIOCSWINSZ`
- **Terminal restore:** `TerminalGuard` RAII ensures raw mode is always cleaned up

## Quick Start

```rust
use remora::container::{Command, Namespace, Stdio};

// Secure container with seccomp, read-only rootfs, and cgroups
let mut child = Command::new("/bin/sh")
    .args(&["-c", "echo hello from container"])
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
    .with_proc_mount()
    .with_seccomp_default()
    .with_no_new_privileges(true)
    .with_readonly_rootfs(true)
    .with_masked_paths_default()
    .drop_all_capabilities()
    .with_cgroup_memory(256 * 1024 * 1024)  // 256 MB
    .spawn()?;

child.wait()?;
```

```rust
use remora::network::NetworkMode;

// Bridge-mode container with internet access
let child = Command::new("/bin/sh")
    .args(&["-c", "ping -c 1 8.8.8.8"])
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_proc_mount()
    .with_network(NetworkMode::Bridge)
    .with_nat()
    .spawn()?;

child.wait_with_output()?;
```

```rust
// Interactive shell
let session = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_proc_mount()
    .spawn_interactive()?;

session.run()?;  // blocks; relays stdin/stdout, forwards SIGWINCH, restores terminal
```

## Building a Root Filesystem

Remora requires a rootfs directory. The test suite uses Alpine Linux.

```bash
# With Docker (recommended):
./build-rootfs-docker.sh

# Without Docker:
./build-rootfs-tarball.sh
```

See `docs/BUILD_ROOTFS.md` for details.

## Running

```bash
# Interactive shell (requires root):
sudo -E cargo run -- --rootfs alpine-rootfs --exe /bin/sh --uid 0 --gid 0

# Seccomp demo:
sudo -E cargo run --example seccomp_demo
```

## Testing

```bash
# Unit tests (no root required):
cargo test --lib

# Integration tests (42 tests, requires root + alpine-rootfs):
sudo -E cargo test --test integration_tests
```

## Architecture

### Pre-exec hook order (critical)

1. **Parent** — opens namespace files, compiles seccomp BPF, sets up bridge netns
2. **Fork**
3. **Child pre_exec** — unshare, UID/GID maps, setuid/setgid, chroot/pivot_root,
   mounts, capability drop, rlimits, setns, seccomp (must be last)
4. **exec** — replace child with target program

Seccomp is applied last because setup requires syscalls it would otherwise block.
Bridge networking is set up entirely in the parent before fork — the child joins
the pre-configured named netns via `setns()`, eliminating all races.

## Comparison

| Feature | Remora | runc | Docker |
|---------|--------|------|--------|
| Namespaces | ✅ 6/7 | ✅ All | ✅ All |
| Seccomp | ✅ Docker profile | ✅ | ✅ |
| Read-only rootfs | ✅ | ✅ | ✅ |
| Capabilities | ✅ | ✅ | ✅ |
| Cgroups v2 | ✅ | ✅ | ✅ |
| Bind / tmpfs / volumes | ✅ | ✅ | ✅ |
| Interactive PTY | ✅ | ✅ | ✅ |
| Loopback + bridge | ✅ | ✅ | ✅ |
| NAT (MASQUERADE) | ✅ | ✅ | ✅ |
| Port mapping | ❌ N4 next | — | ✅ |
| DNS | ❌ N5 next | ✅ | ✅ |
| OCI compliant | ❌ planned | ✅ | ✅ |
| Rootless | ❌ planned | ✅ | ✅ |
| Library API | ✅ | ❌ | ❌ |
| Daemon required | ❌ | ❌ | ✅ |

**Estimated runc parity: ~65%.** See `docs/RUNTIME_COMPARISON.md` for the full matrix
and `docs/ROADMAP.md` for what's next.

## Documentation

| File | Contents |
|------|----------|
| `docs/ROADMAP.md` | What's done and what's next |
| `docs/RUNTIME_COMPARISON.md` | Full feature matrix vs runc/Docker |
| `docs/INTEGRATION_TESTS.md` | Every integration test documented |
| `docs/SECCOMP_DEEP_DIVE.md` | Seccomp-BPF implementation details |
| `docs/PTY_DEEP_DIVE.md` | PTY/interactive session design |
| `docs/CGROUPS.md` | Cgroups v1 vs v2 analysis |
| `docs/BUILD_ROOTFS.md` | How to build the Alpine rootfs |

## Requirements

- Linux kernel 5.0+ (cgroups v2 unified hierarchy)
- Root / `CAP_SYS_ADMIN` for most features
- `nft` (nftables) for NAT and port mapping
- `ip` (iproute2) for bridge networking

## License

See LICENSE file for details.
