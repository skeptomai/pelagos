# Remora

[![CI](https://github.com/skeptomai/remora/actions/workflows/ci.yml/badge.svg)](https://github.com/skeptomai/remora/actions/workflows/ci.yml)

A modern, lightweight Linux container runtime library written in Rust.

Remora provides a safe, ergonomic API for creating containerized processes using
Linux namespaces, seccomp filtering, cgroups v2, and native networking — without
a daemon and without CNI plugins.

**[User Guide](docs/USER_GUIDE.md)** — full CLI reference, networking, storage, security, and more.

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
- **Named volumes:** `Volume::create/open/delete`, `with_volume()` — persisted in
  the data directory (root: `/var/lib/remora/volumes/`, rootless: `~/.local/share/remora/volumes/`)
- **Overlay filesystem:** `with_overlay(upper, work)` — copy-on-write view of a
  shared lower rootfs; writes land in `upper_dir`, lower layer is never modified

### Networking
- **Loopback:** `NetworkMode::Loopback` — isolated NET namespace, `lo` only
- **Bridge:** `NetworkMode::Bridge` — veth pair + `remora0` bridge, IPAM via
  `/run/remora/next_ip` (flock-protected)
- **NAT:** `with_nat()` — nftables MASQUERADE, reference-counted across containers
- **Port mapping:** `with_port_forward(host, container)` — TCP DNAT via nftables
- **DNS:** `with_dns(&["1.1.1.1", "8.8.8.8"])` — bind-mounts a per-container resolv.conf; shared rootfs is never modified

### OCI Runtime Compliance
- **OCI bundles:** parse `config.json` — `ociVersion`, `root`, `process`, `linux.namespaces`, `mounts`
- **Lifecycle:** `remora create <id> <bundle>` / `start` / `state` / `kill` / `delete`
- **State machine:** creating → created → running → stopped
- **Sync:** Unix socket at `/run/remora/<id>/exec.sock` suspends exec until `start`
- **Phase 2 (complete):** `process.capabilities`, `linux.maskedPaths`, `linux.readonlyPaths`,
  `linux.resources`, `process.rlimits`, `linux.sysctl`, `linux.devices`, hooks, `linux.seccomp`

### Image Build
- **`remora build`:** build custom images from Remfiles (simplified Dockerfiles)
- **Instructions:** FROM, RUN, COPY, CMD, ENV, WORKDIR, EXPOSE
- **Daemonless:** Buildah-style — each RUN step snapshots the overlay upper dir as a layer
- **Path safety:** COPY rejects sources outside the build context

### Rootless Containers
- **Auto-detection:** `getuid() != 0` triggers rootless mode automatically — no flag needed
- **Image pull/run:** `remora image pull alpine && remora run alpine /bin/sh` works without root
- **Rootless overlay:** kernel 5.11+ native overlay with `userxattr`, or automatic `fuse-overlayfs` fallback
- **Rootless storage:** `~/.local/share/remora/` (images/layers) + `$XDG_RUNTIME_DIR/remora/` (runtime)
- **User namespace:** auto-adds `Namespace::USER` and a default uid/gid map (`container 0 → host UID`)
- **Pasta networking:** `NetworkMode::Pasta` — full internet access without root via [pasta](https://passt.top/passt/about/)
- **Cgroups:** skipped gracefully in rootless (no `CAP_SYS_ADMIN` needed)
- **Bridge rejected:** clear error if `NetworkMode::Bridge` is attempted without root

### Interactive Containers
- **PTY:** `spawn_interactive()` allocates a PTY pair via `openpty()`
- **SIGWINCH relay:** terminal resize forwarded to container via `TIOCSWINSZ`
- **Terminal restore:** `TerminalGuard` RAII ensures raw mode is always cleaned up

## Installation

```bash
# Install to /usr/local/bin (recommended):
scripts/install.sh

# Or install to ~/.cargo/bin:
cargo install --path .

# Or install to /usr/local/bin via cargo:
sudo cargo install --path . --root /usr/local
```

You can also download a pre-built binary from the
[Releases](https://github.com/skeptomai/remora/releases) page.

For a statically linked binary (e.g. for minimal containers or distroless hosts):

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools   # or equivalent for your distro
cargo build --release --target x86_64-unknown-linux-musl
```

## Quick Start

### Rootless (no sudo)

```bash
# Pull an image and run a command — no root required
remora image pull alpine
remora run alpine /bin/echo hello

# Interactive shell with internet (Ctrl-D to exit)
remora run -i --network pasta alpine /bin/sh
```

### Root (full feature set)

```bash
# Pull an image and run a command
sudo remora image pull alpine
sudo remora run alpine /bin/echo hello

# Interactive shell (Ctrl-D to exit)
sudo remora run -i alpine /bin/sh

# Detached container with bridge networking
sudo remora run -d --name mybox --network bridge --nat alpine \
  /bin/sh -c 'while true; do echo tick; sleep 1; done'

# Check on it
remora ps
remora logs -f mybox

# Stop and clean up
sudo remora stop mybox
remora rm mybox
```

See the **[User Guide](docs/USER_GUIDE.md)** for networking, storage, security,
resource limits, rootless mode, exec, and the full flag reference.

## Rust Library API

Remora is also a library. Use it to embed container isolation in your own programs.

```rust
use remora::container::{Command, Namespace, Stdio};

let mut child = Command::new("/bin/sh")
    .args(&["-c", "echo hello from container"])
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
    .with_proc_mount()
    .with_seccomp_default()
    .drop_all_capabilities()
    .with_cgroup_memory(256 * 1024 * 1024)
    .spawn()?;

child.wait()?;
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

See the [CLI-to-API translation table](docs/USER_GUIDE.md#cli-to-api-translation) in
the user guide.

## Testing

```bash
# Unit tests + lint (no root required):
cargo test --lib
cargo clippy -- -D warnings
cargo fmt -- --check

# Integration tests (require root):
sudo -E cargo test --test integration_tests

# E2E, build, and stress test suites (require root):
sudo -E ./scripts/test-e2e.sh
sudo -E ./scripts/test-build.sh
sudo -E ./scripts/test-stress.sh

# Web stack example (require root + release build):
cargo build --release
sudo PATH=$PWD/target/release:$PATH ./examples/web-stack/run.sh
```

See the [User Guide testing section](docs/USER_GUIDE.md#testing) for details on each suite.

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
| Overlay filesystem | ✅ | ✅ | ✅ |
| Interactive PTY | ✅ | ✅ | ✅ |
| Loopback + bridge | ✅ | ✅ | ✅ |
| NAT (MASQUERADE) | ✅ | ✅ | ✅ |
| Port mapping | ✅ TCP | — | ✅ |
| DNS | ✅ resolv.conf | ✅ | ✅ |
| Image build | ✅ Remfile | — | ✅ Dockerfile |
| OCI compliant | ✅ Phase 1 | ✅ | ✅ |
| Rootless | ✅ (pull, build, run, overlay, pasta) | ✅ | ✅ |
| Library API | ✅ | ❌ | ❌ |
| Daemon required | ❌ | ❌ | ✅ |

**Estimated runc parity: ~80%.** See `docs/RUNTIME_COMPARISON.md` for the full matrix
and `docs/ROADMAP.md` for what's next.

## Documentation

| File | Contents |
|------|----------|
| `docs/USER_GUIDE.md` | CLI and API user guide |
| `docs/ROADMAP.md` | What's done and what's next |
| `docs/RUNTIME_COMPARISON.md` | Full feature matrix vs runc/Docker |
| `docs/INTEGRATION_TESTS.md` | Every integration test documented |
| `docs/SECCOMP_DEEP_DIVE.md` | Seccomp-BPF implementation details |
| `docs/PTY_DEEP_DIVE.md` | PTY/interactive session design |
| `docs/CGROUPS.md` | Cgroups v1 vs v2 analysis |
| `docs/BUILD_ROOTFS.md` | How to build the Alpine rootfs |
| `CHANGELOG.md` | Version history and release notes |

## Requirements

- Linux kernel 5.11+ recommended (rootless overlay with `userxattr`)
- Kernel 5.0+ works with root, or rootless with `fuse-overlayfs` installed
- `pasta` ([passt](https://passt.top)) for rootless networking
- `nft` (nftables) for NAT and port mapping (root only)
- `ip` (iproute2) for bridge networking (root only)

## License

See LICENSE file for details.
