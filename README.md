# Remora

A modern, lightweight container runtime library for Linux written in Rust.

Remora provides a safe, ergonomic API for creating containerized processes using Linux namespaces, seccomp filtering, capabilities, and resource limits.

## Features

### Core Isolation
- **Linux Namespaces**: UTS, Mount, IPC, User, PID, Network, Cgroup
- **Filesystem Isolation**: chroot and pivot_root support
- **Automatic Mounts**: /proc, /sys, /dev helpers

### Security
- **Seccomp Filtering**: Syscall filtering using BPF (Docker's default profile included)
- **Capability Management**: Drop unnecessary capabilities for least-privilege containers
- **Resource Limits**: rlimits for memory, CPU time, file descriptors, etc.

### Advanced Features
- **UID/GID Mapping**: User namespace support for unprivileged containers
- **Namespace Joining**: Attach to existing namespaces (useful for networking)
- **Ergonomic Builder API**: Fluent interface inspired by `std::process::Command`

## Quick Start

### Installation

Add remora to your `Cargo.toml`:

```toml
[dependencies]
remora = { path = "." }
```

### Basic Example

```rust
use remora::container::{Command, Namespace, Stdio};

fn main() {
    // Create a containerized process with security features
    let mut child = Command::new("/bin/sh")
        .args(&["-c", "echo Hello from container!"])
        .stdin(Stdio::Inherit)
        .stdout(Stdio::Inherit)
        .stderr(Stdio::Inherit)
        .with_chroot("/path/to/rootfs")
        .with_namespaces(Namespace::UTS | Namespace::PID | Namespace::MOUNT)
        .with_proc_mount()              // Auto-mount /proc
        .with_seccomp_default()         // Apply Docker's seccomp profile
        .drop_all_capabilities()        // Run with minimal capabilities
        .with_max_fds(1024)             // Limit file descriptors
        .spawn()
        .expect("Failed to spawn container");

    let status = child.wait().expect("Failed to wait");
    println!("Container exited: {:?}", status);
}
```

## Security Features

### Seccomp Filtering

Remora includes seccomp-BPF support for syscall filtering. The Docker default profile blocks ~44 dangerous syscalls including:

- Container escapes: `ptrace`, `unshare`, `mount`, `setns`
- System manipulation: `reboot`, `kexec_load`, `init_module`
- Time manipulation: `clock_settime`, `settimeofday`
- And many more...

**Usage:**

```rust
use remora::container::{Command, SeccompProfile};

// Use Docker's default profile (recommended)
Command::new("/bin/sh")
    .with_seccomp_default()
    .spawn()?;

// Or use minimal profile (only ~40 essential syscalls)
Command::new("/bin/sh")
    .with_seccomp_minimal()
    .spawn()?;

// Or specify a profile programmatically
Command::new("/bin/sh")
    .with_seccomp_profile(SeccompProfile::Docker)
    .spawn()?;
```

See `examples/seccomp_demo.rs` for a complete demonstration.

### Capability Management

Drop Linux capabilities to enforce least-privilege:

```rust
use remora::container::Capability;

// Drop all capabilities (most secure)
Command::new("/bin/sh")
    .drop_all_capabilities()
    .spawn()?;

// Or keep only specific capabilities
Command::new("/bin/sh")
    .with_capabilities(Capability::NET_BIND_SERVICE | Capability::CHOWN)
    .spawn()?;
```

### Resource Limits

Prevent resource exhaustion with rlimits:

```rust
// Limit file descriptors
Command::new("/bin/sh")
    .with_max_fds(1024)
    .spawn()?;

// Limit memory (address space)
Command::new("/bin/sh")
    .with_memory_limit(512 * 1024 * 1024)  // 512 MB
    .spawn()?;

// Limit CPU time
Command::new("/bin/sh")
    .with_cpu_time_limit(60)  // 60 seconds
    .spawn()?;

// Or use the generic rlimit API
Command::new("/bin/sh")
    .with_rlimit(libc::RLIMIT_NOFILE, 1024, 1024)
    .spawn()?;
```

## Building a Root Filesystem

Remora requires a root filesystem (rootfs) to run containers.

**Option 1: Use existing rootfs** (if `alpine-rootfs/` already exists):
```bash
# Verify it exists
ls alpine-rootfs/bin/busybox
```

**Option 2: Build with Docker** (recommended):
```bash
./build-rootfs-docker.sh
```

**Option 3: Build without Docker** (download tarball directly):
```bash
./build-rootfs-tarball.sh
```

Both scripts create an `alpine-rootfs/` directory with a minimal Alpine Linux environment (~5-10 MB). See `BUILD_ROOTFS.md` for details.

## Running Examples

```bash
# Build the alpine rootfs first (if needed)
./build-rootfs-docker.sh    # or ./build-rootfs-tarball.sh

# Run the seccomp demo (requires root)
sudo -E cargo run --example seccomp_demo
```

## Testing

Run the integration tests (requires root privileges):

```bash
# Build rootfs if needed
./build-rootfs-docker.sh    # or ./build-rootfs-tarball.sh

# Run tests
sudo -E cargo test --test integration_tests
```

## Architecture

Remora uses Linux namespaces for isolation and a carefully orchestrated pre-exec hook to set up the container environment:

1. **Parent process**: Opens files, compiles seccomp filters (requires allocation)
2. **Fork**: Creates child process
3. **Pre-exec hook** (in child, before exec):
   - Unshare namespaces
   - Set up UID/GID mappings
   - Change root (chroot or pivot_root)
   - Mount filesystems (/proc, /sys, /dev)
   - Drop capabilities
   - Set resource limits
   - Join existing namespaces
   - Apply seccomp filter (MUST be last!)
4. **Exec**: Replace child process with target program

The pre-exec hook must be signal-safe and cannot allocate memory, so all preparation (opening files, compiling filters) happens in the parent process.

## Documentation

For detailed API documentation:

```bash
cargo doc --open
```

For implementation details, see:

- `SECCOMP_DEEP_DIVE.md` - Comprehensive seccomp implementation guide
- `CGROUPS.md` - Cgroups v1 vs v2 analysis
- `RUNTIME_COMPARISON.md` - Feature comparison with Docker/runc/Podman
- `ROADMAP.md` - Future development plans

## Comparison to Other Runtimes

| Feature | Remora | runc | Docker | Podman |
|---------|--------|------|--------|--------|
| Namespaces | ✅ 6/7 | ✅ All | ✅ All | ✅ All |
| Seccomp | ✅ Docker profile | ✅ Custom | ✅ Default | ✅ Default |
| Capabilities | ✅ Drop/keep | ✅ Full | ✅ Full | ✅ Full |
| Resource limits | ✅ rlimits | ✅ Cgroups | ✅ Cgroups | ✅ Cgroups |
| TTY/PTY | ❌ Planned | ✅ | ✅ | ✅ |
| Networking | ⚠️ Join only | ✅ CNI | ✅ CNI | ✅ CNI |
| OCI Compatible | ❌ | ✅ | ✅ | ✅ |

Remora is currently at ~35% feature parity with runc. See `ROADMAP.md` for planned features.

## Requirements

- **OS**: Linux (kernel 3.8+ for namespaces, 3.5+ for seccomp)
- **Architecture**: x86_64 or aarch64
- **Privileges**: Most features require root or `CAP_SYS_ADMIN`

## License

See LICENSE file for details.

## Contributing

This is an educational project. Contributions, bug reports, and suggestions are welcome!

## Status

**Phase 1 Complete**: Core isolation, seccomp filtering, capabilities, resource limits

**In Progress**: See `ROADMAP.md` for upcoming features including TTY/PTY support, networking, and OCI compatibility.
