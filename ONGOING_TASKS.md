# Ongoing Tasks

## Current: Networking End-to-End Test Suite

### Motivation

Rule/config existence tests pass while actual traffic is broken — proven by
the NAT debugging session where nftables MASQUERADE was present but TCP/UDP
was silently dropped by iptables FORWARD policy DROP. We need tests that
send real packets through every networking feature.

### Plan

#### Test 1: `test_port_forward_end_to_end` (integration test)

**Module:** `networking`
**Requires:** root, rootfs
**Serial key:** `nat`

Container A runs `echo HELLO | nc -l -p 80` with
`with_port_forward(19090, 80)` + `with_nat()`. A temporary external network
namespace (`pf-test-client`, 10.99.0.0/24 veth pair) connects to host on
forwarded port. Traffic goes through PREROUTING → DNAT → FORWARD → bridge → A.

Note: DNAT prerouting doesn't apply to locally-originated traffic (OUTPUT chain)
or bridge-internal traffic (hairpin issues). Must test from external netns.

**What it proves:** The full DNAT path works — external traffic → nftables
prerouting → FORWARD → container. Current tests only check rule strings exist.

#### Test 2: `test_bridge_cleanup_after_sigkill` (integration test)

**Module:** `networking`
**Requires:** root, rootfs
**Serial key:** `nat`

Spawn a bridge+NAT container running `sleep 60`. Record the veth name
(`child.veth_name()`), netns name (`child.netns_name()`), and confirm
iptables FORWARD rules exist. Then SIGKILL the container and call `wait()`.

Assert after wait:
- veth is gone (`ip link show {veth}` fails)
- netns is gone (`/run/netns/{ns}` absent)
- nftables table is gone (`nft list table ip remora` fails)
- iptables FORWARD rules are gone (`iptables -C ...` fails)

**What it proves:** `wait()` runs full teardown even after ungraceful death.
All existing cleanup tests use normal exit.

#### Test 3: `test_nat_end_to_end_tcp` (integration test)

**Module:** `networking`
**Requires:** root, rootfs, outbound internet
**Serial key:** `nat`

Spawn a bridge+NAT+DNS container that runs
`wget -q -T 5 --spider http://1.1.1.1/`. Assert exit code 0.

Skip gracefully if no internet (check with host-side ping first).
Follows the same pattern as `test_pasta_connectivity`.

**What it proves:** TCP actually flows through NAT to the internet. Our
existing NAT tests only verify nftables/iptables rules exist.

#### Test 4: `examples/full_stack_smoke/main.rs` (example script)

Compose every major feature in one container setup:
- Overlay filesystem (upper + work dirs)
- Bridge networking + NAT + DNS
- Container linking (2 containers)
- tmpfs mount

Container A: bridge+NAT+DNS+overlay, runs `wget -qO- http://1.1.1.1/` to
prove internet works, then runs `nc -l -p 8080` serving a message.
Container B: bridge+overlay+link to A, connects to A by name, prints result.

**What it proves:** Features compose correctly. Each works alone but
interactions are untested. This is a smoke test, not a unit assertion.

### Files to change

| Action | File |
|--------|------|
| Edit | `tests/integration_tests.rs` — add tests 1-3 to `networking` module |
| Edit | `docs/INTEGRATION_TESTS.md` — document all 3 new tests |
| Create | `examples/full_stack_smoke/main.rs` — composition smoke test |
| Edit | `ONGOING_TASKS.md` — mark complete when done |

### Order of implementation

1. `test_port_forward_end_to_end` — self-contained, no internet needed
2. `test_bridge_cleanup_after_sigkill` — self-contained, no internet needed
3. `test_nat_end_to_end_tcp` — needs internet, skip guard
4. `examples/full_stack_smoke/main.rs` — last, depends on all above passing

---

## Planned Feature 1: OCI Image Layers

**Priority:** High — enables `remora pull alpine` instead of manual rootfs setup
**Effort:** Significant Work

Pull OCI/Docker images from registries, unpack their layers, and run containers
from them using overlayfs. See git history for full design notes.

---

## Planned Feature 2: `remora exec` — Attach to Running Container

**Priority:** Medium — quality-of-life for debugging running containers
**Effort:** Moderate

Run a new process inside an already-running container's namespaces, similar to
`docker exec`. See git history for full design notes.

---

## Previous Tasks — COMPLETE

- `4abfa6d` — Integration tests for cross-container TCP and NAT iptables rules
- `7ecbc40` — Fix NAT forwarding for UFW/Docker hosts, upgrade web pipeline to httpd
- `ce4a8cf` — Multi-container web pipeline and net debug examples
- `22ec972` — Container linking + test reorganization (76 tests, 11 modules)
- `bff6327` — Fix OCI create PID resolution and kill test for PID namespaces
- `41b78ce` — Full-featured CLI and PID namespace double-fork bug

---

## Planned (Deferred)

### AppArmor / SELinux — MAC Profile Support

Deferred: the seccomp + capabilities + masked paths stack is already solid, and MAC requires
system-side setup (profile loading) that most users won't have. Revisit if there's demand.
