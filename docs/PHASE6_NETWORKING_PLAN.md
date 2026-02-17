# Phase 6: Native Networking — Implementation Plan

**Date:** 2026-02-17
**Status:** Planning

---

## Decision: Native Networking (not CNI)

We implement networking ourselves using Linux netlink rather than delegating to CNI plugins.

**Why native:**
- Full control — no external plugin processes or JSON config schemas
- Aligns with Podman 4.0+'s direction (dropped CNI for native netavark in Rust)
- Better fit for Remora's sync, library-first architecture
- Docker itself uses native libnetwork, not CNI
- CNI only matters if integrating with Kubernetes/containerd — not our goal

**Sync netlink (not rtnetlink):**
- `rtnetlink` crate requires `tokio` — heavyweight, async-only
- `netlink-packet-route` + `netlink-sys` provide the same kernel interface synchronously
- Matches Remora's existing threading model

---

## Architecture Overview

All network setup is **parent-side**, after fork, targeting the child's network namespace via `/proc/{pid}/ns/net` — no changes to the pre_exec closure for veth/bridge work.

Loopback bring-up is the one exception: it happens in the child (pre_exec) because it only needs to touch the child's own netns, not create cross-namespace resources.

```
Parent:
  fork() ──────────────────────────────────────────────────► Child (pre_exec):
                                                               unshare(CLONE_NEWNET)
  ◄── gets child PID ─────────────────────────────────────── (loopback up, if N1)

  [N2] create veth pair (in host netns)
  [N2] move veth1 → child netns via RTM_NEWLINK + IFLA_NET_NS_FD
  [N2] configure veth1 IP in child netns (enter child netns via setns)
  [N2] attach veth0 to remora0 bridge
  [N3] ensure NAT/IP forwarding (once per bridge, idempotent)
  [N4] install DNAT rules for port mappings

  Child:
    child.wait() ──► teardown: remove DNAT rules, drop veth pair
```

---

## New Module: `src/network.rs`

```rust
pub enum NetworkMode {
    None,           // No network namespace (share host) — current default
    Loopback,       // Isolated netns, lo only (N1)
    Bridge,         // Full connectivity via remora0 bridge (N2+)
}

pub struct PortMapping {
    pub host_port:      u16,
    pub container_port: u16,
    pub protocol:       PortProtocol,   // Tcp | Udp
}

pub enum PortProtocol { Tcp, Udp }

pub struct NetworkConfig {
    pub mode:          NetworkMode,
    pub port_mappings: Vec<PortMapping>,
    pub dns_servers:   Vec<Ipv4Addr>,     // default: [1.1.1.1, 8.8.8.8]
}
```

---

## Dependencies to Add

```toml
[dependencies]
netlink-packet-route = "0.28"   # RTM_NEWLINK, RTM_NEWADDR, RTM_NEWROUTE
netlink-sys           = "0.8"   # Sync NetlinkSocket
ipnetwork             = "0.21"  # Ipv4Network for IPAM
```

No tokio. No async.

---

## Phase N1: Loopback — Isolated Network Namespace

**Goal:** When `Namespace::NET` is requested, bring up `lo` inside the container so `127.0.0.1` works. Today, `lo` is created by the kernel but left DOWN.

### What changes

- `container.rs` pre_exec: if NET namespace is being unshared, bring up `lo` via a netlink `RTM_NEWLINK` (or `ioctl SIOCSIFFLAGS`) call inside the child
- No new API — existing `.with_namespaces(Namespace::NET)` is sufficient
- `NetworkMode::None` stays the default; no new field needed yet

### Implementation sketch

```rust
// In pre_exec, after unshare(CLONE_NEWNET):
fn bring_up_loopback() -> io::Result<()> {
    // Open a NETLINK_ROUTE socket
    // Send RTM_NEWLINK with IFF_UP | IFF_LOOPBACK for ifindex 1
    // Or: use ioctl(SIOCSIFFLAGS) on a raw UDP socket — simpler
}
```

The ioctl approach is 15 lines of safe-ish code. The netlink approach is more principled. We use netlink for consistency with N2.

### Tests (N1, 1 new test → 32 total)

- **test_loopback_up** — container with `Namespace::NET`; run `ip addr show lo` or `ping -c1 127.0.0.1`; verify exit 0

---

## Phase N2: veth + Bridge + IPAM

**Goal:** Containers get a private IP (`172.19.0.x/24`) reachable from the host and from each other via the `remora0` bridge.

### New API

```rust
Command::new("/bin/sh")
    .with_network(NetworkMode::Bridge)
    .spawn()?;
```

### IPAM (simple, no daemon)

- Bridge subnet: `172.19.0.0/24`, gateway `172.19.0.1`
- Each container gets the next available `/24` host address
- Allocation: atomic file lock + scan `/sys/class/net/remora0/` ARP table
- Keep it simple: sequential allocation from `.2`; release on teardown

### Implementation (src/network.rs)

```rust
pub struct NetworkSetup {
    pub container_ip:   Ipv4Addr,
    pub veth_host:      String,     // e.g. "veth-a3f2"
    pub veth_container: String,     // always "eth0" inside container
}

/// Called in parent after fork, before returning Child.
pub fn setup_bridge_network(
    child_pid: u32,
    config: &NetworkConfig,
) -> io::Result<NetworkSetup>

/// Called after child.wait() — removes veth pair (kernel removes from bridge automatically).
pub fn teardown_network(setup: &NetworkSetup) -> io::Result<()>
```

**Steps inside `setup_bridge_network`:**

1. Ensure `remora0` bridge exists and is UP (`RTM_NEWLINK` with `IFLA_INFO_KIND=bridge`)
2. Assign `172.19.0.1/24` to `remora0` if not already set
3. Generate random `veth_host` name (`veth-{4 hex chars}`)
4. Create veth pair: `RTM_NEWLINK` with `IFLA_INFO_KIND=veth`, peer name = `veth_host`
5. Move `eth0` end into child netns: `RTM_NEWLINK` with `IFLA_NET_NS_FD = /proc/{pid}/ns/net`
6. Assign IP to `eth0` inside child netns:
   - `setns()` into child netns via `open(/proc/{pid}/ns/net)`
   - `RTM_NEWADDR` for container IP
   - `RTM_NEWROUTE` for default route via `172.19.0.1`
   - `RTM_NEWLINK` to bring up `eth0`
   - `setns()` back to host netns
7. Attach `veth_host` to `remora0` bridge: `RTM_NEWLINK` with `IFLA_MASTER`
8. Bring up `veth_host`

### Child struct extension

```rust
pub struct Child {
    inner:    process::Child,
    cgroup:   Option<cgroups_rs::fs::Cgroup>,
    network:  Option<NetworkSetup>,           // NEW
}
```

`wait()` / `wait_with_output()` call `teardown_network` after `teardown_cgroup`.

### Tests (N2, 3 new tests → 35 total)

- **test_bridge_network_ip** — container gets `172.19.0.x`; run `ip addr`; grep for `172.19`
- **test_bridge_host_reachability** — from parent process, ping container IP after spawn; verify reachable
- **test_two_containers_communicate** — spawn two containers; ping each other's IP; verify reachable

---

## Phase N3: NAT / Internet Access

**Goal:** Containers can reach the internet via MASQUERADE on the host's default route interface.

### What changes

- After `remora0` is set up (first container using Bridge mode), enable:
  - `/proc/sys/net/ipv4/ip_forward = 1`
  - `nftables` MASQUERADE rule: `ip saddr 172.19.0.0/24 masquerade`

- We call `nft` via `std::process::Command` (shell out) — writing raw nftables netlink is complex; shelling out is idiomatic (Docker does the same with `iptables`)

```rust
fn ensure_nat(bridge_subnet: &str) -> io::Result<()> {
    // Idempotent: check if rule exists first
    // echo 1 > /proc/sys/net/ipv4/ip_forward
    // nft add table ip remora
    // nft add chain ip remora postrouting { type nat hook postrouting priority 100; }
    // nft add rule ip remora postrouting ip saddr <subnet> masquerade
}
```

- Teardown: remove `remora` nft table on last container exit (reference count in `/run/remora/refcount`)

### Tests (N3, 2 new tests → 37 total)

- **test_internet_access** — container runs `wget -q -O- http://1.1.1.1` (IP, not DNS); verify exit 0
- **test_nat_cleanup** — after all containers exit, verify `nft list table ip remora` fails or is empty

---

## Phase N4: Port Mapping

**Goal:** `with_port_mapping(8080, 80)` makes host port 8080 DNAT to container port 80.

### New API

```rust
Command::new("/bin/sh")
    .with_network(NetworkMode::Bridge)
    .with_port_mapping(8080, 80)          // host:container, TCP
    .with_port_mapping_udp(5353, 53)      // UDP
    .spawn()?;
```

### Implementation

```rust
fn install_dnat_rules(container_ip: Ipv4Addr, mappings: &[PortMapping]) -> io::Result<()> {
    // nft add rule ip remora prerouting \
    //   tcp dport <host_port> dnat to <container_ip>:<container_port>
}

fn remove_dnat_rules(container_ip: Ipv4Addr, mappings: &[PortMapping]) -> io::Result<()>
```

`PortMapping` stored in `NetworkSetup` for teardown.

### Tests (N4, 2 new tests → 39 total)

- **test_port_mapping_tcp** — spawn container running `nc -l -p 80`; connect from host to mapped port 8080; verify data flows
- **test_port_mapping_cleanup** — after wait, verify DNAT rule is gone (`nft list table ip remora`)

---

## Phase N5: DNS

**Goal:** Containers can resolve hostnames.

### Implementation

Simple approach: write `/etc/resolv.conf` into the container's rootfs before exec.

```rust
pub fn write_resolv_conf(rootfs: &Path, servers: &[Ipv4Addr]) -> io::Result<()> {
    let path = rootfs.join("etc/resolv.conf");
    let content = servers.iter()
        .map(|ip| format!("nameserver {}\n", ip))
        .collect::<String>();
    fs::write(path, content)
}
```

Called in pre_exec if `dns_servers` is non-empty (requires rootfs, not just netns).

Default servers: `[1.1.1.1, 8.8.8.8]` when `NetworkMode::Bridge`.

**No embedded DNS server** — that's complexity we don't need. Containers resolve via the host's upstream.

### New API

```rust
Command::new("/bin/sh")
    .with_network(NetworkMode::Bridge)
    .with_dns(&["1.1.1.1", "8.8.8.8"])   // override defaults
    .spawn()?;
```

### Tests (N5, 2 new tests → 41 total)

- **test_dns_resolution** — container runs `nslookup one.one.one.one`; verify exit 0 (requires N3 internet access)
- **test_custom_dns** — set `with_dns(&["8.8.8.8"])`; inspect `/etc/resolv.conf` inside container

---

## Files to Modify

| File | Change |
|------|--------|
| `src/network.rs` | **New**: `NetworkMode`, `NetworkConfig`, `PortMapping`, `NetworkSetup`, `setup_bridge_network`, `teardown_network`, `write_resolv_conf`, `ensure_nat`, `install_dnat_rules` |
| `src/lib.rs` | Add `pub mod network;` |
| `src/container.rs` | `network_config` field on `Command`; `network: Option<NetworkSetup>` on `Child`; builder methods; loopback in pre_exec (N1); parent-side setup in `spawn()`/`spawn_interactive()`; teardown in `wait()`/`wait_with_output()` |
| `tests/integration_tests.rs` | 10 new tests (N1-N5) |
| `Cargo.toml` | Add `netlink-packet-route`, `netlink-sys`, `ipnetwork` |
| `CLAUDE.md` | Update phase status, file structure, comparison table |

---

## Verification Per Phase

**N1:**
```bash
sudo -E cargo test --test integration_tests test_loopback
```

**N2:**
```bash
sudo -E cargo test --test integration_tests test_bridge
# Manual: ping $(container_ip) from host while container sleeps
```

**N3:**
```bash
sudo -E cargo test --test integration_tests test_internet
# Manual: nft list table ip remora
```

**N4:**
```bash
sudo -E cargo test --test integration_tests test_port_mapping
# Manual: curl http://localhost:8080
```

**N5:**
```bash
sudo -E cargo test --test integration_tests test_dns
```

**Full suite after all phases:**
```bash
sudo -E cargo test --test integration_tests   # 41 tests
ls /sys/class/net/ | grep remora              # only remora0 bridge
nft list table ip remora 2>/dev/null          # empty if no containers running
```

---

## Notes / Risks

- **setns in parent** — entering child netns from parent to configure addresses requires `setns()`. This is safe but means the calling thread temporarily lives in a different netns. Use a dedicated OS thread (`std::thread::spawn`) to avoid polluting the spawning thread's netns state.
- **Bridge persistence** — `remora0` bridge persists across container runs (like `docker0`). Explicit cleanup would require a separate CLI subcommand; not implemented by default.
- **nft vs iptables** — we use `nft` (nftables). Systems without nft will need iptables fallback; defer that concern.
- **IPAM race** — concurrent container starts could race on IP allocation. Use an advisory file lock at `/run/remora/ipam.lock`.
- **Rootless networking** — `slirp4netns`-style userspace networking for rootless containers is a follow-on; too complex for this phase.
- **No breaking changes** — `NetworkMode::None` (default) leaves existing behavior identical.

---

## Test Count Summary

| After Phase | Tests |
|-------------|-------|
| Current     | 31    |
| N1          | 32    |
| N2          | 35    |
| N3          | 37    |
| N4          | 39    |
| N5          | 41    |
