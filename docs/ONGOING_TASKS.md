# Remora: Active Development Tasks

**Last Updated:** 2026-02-17
**Current Status:** Phase 6 (Networking) — N1+N2 implementing named-netns approach

---

## Phase Progress

- ✅ **Phase 1:** Security Hardening (seccomp, no-new-privs, readonly rootfs, masked paths, capabilities, rlimits) — 22 tests
- ✅ **Phase 2:** Interactive PTY (`spawn_interactive`, SIGWINCH relay, TerminalGuard)
- ✅ **Phase 4:** Filesystem Flexibility (bind mounts, tmpfs, named volumes) — 26 tests
- ✅ **Phase 5:** Cgroups v2 Resource Management (memory, CPU, PIDs, resource stats) — 31 tests
- 🔄 **Phase 6:** Native Networking — N1+N2 in progress

---

## Phase 6: Native Networking Plan

Full plan: `docs/PHASE6_NETWORKING_PLAN.md`

### Architecture Decision
- **Native networking** (not CNI) — full control, no external plugins required
- **Named netns for N2** — `ip netns add/exec/del` configures bridge BEFORE fork; child joins
  via `setns()` in pre_exec. Eliminates all races (container sees configured network from exec).
- **ioctl** for loopback bring-up inside container (pre_exec for Loopback mode)
- Shell out to `nft` for NAT/port-mapping rules (N3/N4)

#### Why named netns (not fd-passing, not sync-pipe)?

| Approach | Problem |
|----------|---------|
| Sync pipe in pre_exec | Deadlock: Rust's spawn() blocks until child exec()s via fail-pipe; pre_exec blocking prevents exec |
| Open /proc/{pid}/ns/net after spawn | ENOENT if container exits before parent opens the file (`exit 0` containers) |
| SCM_RIGHTS fd passing | Complex + still doesn't fix container-visible race (polling still needed) |
| **Named netns (chosen)** | No race at all — setup is 100% complete before fork |

#### Named netns implementation:

```
ip netns add rem-{pid}-{n}                   # creates /run/netns/rem-{pid}-{n}
ip -n rem-{pid}-{n} link set lo up           # bring up lo inside the netns
ip link add vh-{hash} type veth peer vp-{hash}
ip link set vp-{hash} netns rem-{pid}-{n}    # move peer into named netns
ip -n rem-{pid}-{n} link set vp-{hash} name eth0
ip -n rem-{pid}-{n} addr add {ip}/24 dev eth0
ip -n rem-{pid}-{n} link set eth0 up
ip -n rem-{pid}-{n} route add default via 172.19.0.1
ip link set vh-{hash} master remora0
ip link set vh-{hash} up
# In pre_exec: open("/run/netns/rem-{pid}-{n}") + setns(fd, CLONE_NEWNET)
# In teardown: ip link del vh-{hash} && ip netns del rem-{pid}-{n}
```

### Sub-phases

| Phase | Feature | Tests | Status |
|-------|---------|-------|--------|
| N1 | Loopback bring-up in NET namespace | +1 → 32 | 🔄 In progress |
| N2 | veth + remora0 bridge + IPAM (named netns) | +3 → 35 | 🔄 In progress |
| N3 | NAT / internet access (nftables MASQUERADE) | +2 → 37 | ⏳ Pending |
| N4 | Port mapping (DNAT rules) | +2 → 39 | ⏳ Pending |
| N5 | DNS (write /etc/resolv.conf) | +2 → 41 | ⏳ Pending |

### Files to touch for N1+N2

| File | Change |
|------|--------|
| `src/network.rs` | **New**: `NetworkMode`, `NetworkConfig`, `NetworkSetup`, `bring_up_loopback()`, `setup_bridge_network()`, `teardown_network()` |
| `src/lib.rs` | Add `pub mod network;` |
| `src/container.rs` | `network_config` field, `with_network()` builder, loopback in pre_exec, parent-side bridge setup, `Child.network` teardown |
| `tests/integration_tests.rs` | 4 new tests: loopback, bridge IP, veth exists, cleanup |

### Key Implementation Notes

**N1 — Loopback (pre_exec, in child after unshare(NET)):**
```rust
// src/network.rs
pub fn bring_up_loopback() -> io::Result<()> {
    // Uses ioctl(SIOCGIFFLAGS) + ioctl(SIOCSIFFLAGS) on AF_INET socket
    // Sets IFF_UP flag on "lo" — kernel auto-assigns 127.0.0.1
}
```

**N2 — Bridge setup (parent-side after fork, via ip/nsenter):**
```
ip link add remora0 type bridge           (idempotent)
ip addr add 172.19.0.1/24 dev remora0    (idempotent)
ip link set remora0 up
ip link add veth-{pid} type veth peer name eth0
ip link set eth0 netns {pid}             (move into container netns)
nsenter --net=/proc/{pid}/ns/net -- ip addr add {ip}/24 dev eth0
nsenter --net=/proc/{pid}/ns/net -- ip link set eth0 up
nsenter --net=/proc/{pid}/ns/net -- ip route add default via 172.19.0.1
ip link set veth-{pid} master remora0
ip link set veth-{pid} up
```

**IPAM:** file-based at `/run/remora/next_ip` (flock protected), allocates 172.19.0.2+

**Teardown:** `ip link del veth-{pid}` — cascades to remove peer and bridge attachment

**NetworkMode enum:**
```rust
pub enum NetworkMode {
    None,       // share host net (default, no changes)
    Loopback,   // isolated NET ns, lo only
    Bridge,     // full connectivity via remora0
}
```

**`with_network(mode)` builder:** automatically adds `Namespace::NET` for Loopback/Bridge modes

### N3–N5 Notes (future)

- **N3 NAT:** `nft add table ip remora` + MASQUERADE rule, reference-counted teardown
- **N4 port mapping:** `with_port_mapping(host, container)`, DNAT via nft
- **N5 DNS:** write `/etc/resolv.conf` into rootfs pre-exec, default `1.1.1.1 8.8.8.8`

---

## Completed Phases Archive

### Phase 5 (Feb 16 2026) — cgroups v2
- `src/cgroup.rs`: `CgroupConfig`, `setup_cgroup`, `teardown_cgroup`, `ResourceStats`, `read_stats`
- `src/container.rs`: `cgroup_config` field, builder methods, `Child.cgroup` teardown in wait()
- 5 tests: memory limit, PIDs limit, CPU shares, resource stats, cleanup

### Phase 4 (Feb 16 2026) — filesystem flexibility
- `src/container.rs`: `BindMount`, `TmpfsMount`, `Volume`, builder methods
- Bind mounts happen before chroot (host paths); tmpfs after chroot (no host source)
- 4 tests: bind rw, bind ro, tmpfs, named volume

### Phase 1-2 (Feb 16 2026) — security + PTY
- Seccomp (Docker profile + minimal), no-new-privs, readonly rootfs, masked paths
- Capability drop, rlimits
- PTY relay, SIGWINCH forwarding, TerminalGuard RAII

---

## Running Tests

```bash
# Unit tests (no root required):
cargo test --lib

# Integration tests (requires root):
sudo -E cargo test --test integration_tests

# After N1+N2: verify no leftover state
ip link show remora0          # bridge should exist
ip link show | grep veth      # no veth- interfaces (all cleaned up)
```
