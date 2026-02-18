# Testing Guide

Remora's integration tests are organized into **11 categorized modules** in
`tests/integration_tests.rs`. Each module can be run independently using
Cargo's test name filter.

## Quick Reference

```bash
# Run ALL integration tests (requires root + alpine-rootfs)
sudo -E cargo test --test integration_tests

# Run a single category
sudo -E cargo test --test integration_tests <category>::

# Run a single test
sudo -E cargo test --test integration_tests <category>::<test_name>

# Unit tests (no root required)
cargo test --lib
```

## Test Categories

| Category | Tests | Root | Rootfs | Description |
|---|---|---|---|---|
| `api` | 5 | No | No | Builder pattern, bitflags, API surface verification |
| `core` | 4 | Yes | Yes | Namespace creation, proc mount, combined features, PID namespace fork |
| `capabilities` | 2 | Yes | Yes | Dropping all / selective capability sets |
| `resources` | 3 | Yes | Yes | rlimits for file descriptors, memory, CPU time |
| `security` | 9 | Yes | Yes | Seccomp profiles, no-new-privileges, read-only rootfs, masked paths |
| `filesystem` | 7 | Yes | Yes | Bind mounts (RW/RO), tmpfs, named volumes, overlay filesystem |
| `cgroups` | 5 | Yes | Yes | Memory/PID/CPU cgroup limits, resource stats, cleanup |
| `networking` | 15 | Yes | Yes | Loopback, bridge, NAT, port forwarding, DNS, concurrent spawn |
| `oci_lifecycle` | 11 | Yes | Yes | OCI create/start/state/kill/delete, mounts, caps, seccomp, hooks |
| `rootless` | 7 | **No** | Yes | Rootless containers, USER namespace, pasta networking |
| `linking` | 4 | Yes | Yes | Container-to-container `/etc/hosts` injection and connectivity |

**Total: 72 tests**

## Running by Category

### API tests (no root needed)

```bash
cargo test --test integration_tests api::
```

Verifies builder pattern chaining, namespace/capability bitflags, and seccomp
API surface. These tests don't spawn containers — they just confirm the API
compiles and methods are callable.

### Core tests

```bash
sudo -E cargo test --test integration_tests core::
```

Basic namespace creation (UTS + MOUNT), `/proc` mounting, combined feature
stacking, and the PID namespace repeated-fork regression test.

### Capabilities tests

```bash
sudo -E cargo test --test integration_tests capabilities::
```

Dropping all capabilities and keeping only a selective set.

### Resources tests

```bash
sudo -E cargo test --test integration_tests resources::
```

rlimits for max open file descriptors, memory, and CPU time.

### Security tests

```bash
sudo -E cargo test --test integration_tests security::
```

Seccomp BPF filtering (Docker default profile, minimal profile, without
seccomp), `PR_SET_NO_NEW_PRIVS`, read-only rootfs, masked paths (default and
custom sets), and the combined Phase 1 security stack.

### Filesystem tests

```bash
sudo -E cargo test --test integration_tests filesystem::
```

Bind mounts (read-write and read-only), tmpfs on read-only rootfs, named
volumes with persistence, and overlay filesystem (write-to-upper,
lower-unchanged, merged-dir cleanup).

### Cgroups tests

```bash
sudo -E cargo test --test integration_tests cgroups::
```

Cgroups v2 memory limit, PID limit, CPU shares/weight, `resource_stats()`
retrieval, and automatic cgroup cleanup after `wait()`.

### Networking tests

```bash
sudo -E cargo test --test integration_tests networking::
```

Covers N1 through N5:
- **Loopback**: `lo` interface with 127.0.0.1
- **Bridge**: IP assignment, veth creation, veth/netns cleanup, loopback in
  bridge mode, gateway reachability, concurrent IP allocation
- **NAT**: nftables rule creation, cleanup, refcount across multiple containers
- **Port forwarding**: DNAT rule creation, cleanup, independent teardown
- **DNS**: `/etc/resolv.conf` injection via bind mount

Some tests use `#[serial(nat)]` to avoid nftables race conditions.

### OCI lifecycle tests

```bash
sudo -E cargo test --test integration_tests oci_lifecycle::
```

Full OCI runtime spec lifecycle: `create` → `start` → `state` → `kill` →
`delete`. Also tests OCI config.json features: tmpfs mounts, capabilities,
masked/readonly paths, cgroup resources, rlimits, sysctl, hooks (prestart +
poststop), and seccomp policies.

### Rootless tests (no root — run WITHOUT sudo)

```bash
cargo test --test integration_tests rootless::
```

These tests **must be run as a non-root user** (without `sudo`). They verify:
- Rootless container with auto USER namespace and uid=0 mapping
- Loopback networking in rootless mode
- Bridge networking rejection with clear error message
- Explicit USER namespace with uid/gid maps
- Pasta networking: TAP interface creation, rootless mode, end-to-end
  internet connectivity

### Linking tests

```bash
sudo -E cargo test --test integration_tests linking::
```

Container-to-container networking via `/etc/hosts` injection:
- Hosts file contains linked container's bridge IP
- Alias and original name both resolve
- Actual ICMP connectivity (ping by name)
- Missing container produces a clear error

## Running Multiple Categories

Cargo's filter is a substring match, so you can combine:

```bash
# Run security + capabilities
sudo -E cargo test --test integration_tests security:: capabilities::

# Run all network-related tests (networking + linking)
sudo -E cargo test --test integration_tests networking:: linking::
```

## Prerequisites

- **Alpine rootfs**: Run `scripts/build-rootfs-docker.sh` or `scripts/build-rootfs-tarball.sh`
  to create `alpine-rootfs/` in the project root
- **Root privileges**: Most categories require `sudo -E` (the `-E` preserves
  environment variables like `PATH` and `CARGO_TARGET_DIR`)
- **pasta** (optional): Required for `rootless::test_pasta_*` tests — install
  via your package manager (`passt` package on most distros)
- **Internet access** (optional): Required for `rootless::test_pasta_connectivity`
