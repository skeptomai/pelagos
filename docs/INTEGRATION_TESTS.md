# Remora Integration Test Reference

Every integration test in `tests/integration_tests.rs` is documented here.
**When adding a new integration test, add its entry to this file in the same commit.**

Run with:
```bash
sudo -E cargo test --test integration_tests
```

Tests that do not require root skip themselves with `eprintln!("Skipping ...")` and return.
Tests that require `alpine-rootfs` skip themselves if it is absent.

---

## Conventions

- **Requires root**: must be run via `sudo -E cargo test ...`
- **Requires rootfs**: skips if `alpine-rootfs/bin/busybox` is not found
- **API-only**: compiles/runs without root or rootfs — just checks builder API shape

---

## No-Root / API Tests

These exercise the type system and builder API. They never spawn a process.

### `test_uid_gid_api`
**Type:** API-only

Verifies that `with_uid()`, `with_gid()`, `with_uid_maps()`, and `with_gid_maps()` exist,
accept the right types, and chain correctly. No process is spawned.

### `test_namespace_bitflags`
**Type:** API-only

Confirms that `Namespace` bitflags compose correctly via `|` and that `contains()` and
`!contains()` return expected results for `UTS`, `MOUNT`, and `PID`.

### `test_capability_bitflags`
**Type:** API-only

Same as above for `Capability` flags: `CHOWN`, `NET_BIND_SERVICE`, and `SYS_ADMIN`.

### `test_command_builder_pattern`
**Type:** API-only

Chains several builder methods (`args`, `stdin`, `stdout`, `stderr`, `with_namespaces`,
`with_chroot`, `env`, `with_proc_mount`, `with_max_fds`) and verifies it compiles.

### `test_seccomp_profile_api`
**Type:** API-only

Verifies all four seccomp builder methods compile and chain:
`with_seccomp_default()`, `with_seccomp_minimal()`, `with_seccomp_profile(Docker)`,
`without_seccomp()`.

---

## Core Container Tests

### `test_basic_namespace_creation`
**Requires:** root, rootfs

Spawns `ash -c "exit 0"` inside `UTS | MOUNT` namespaces with `chroot`. Verifies
that `spawn()` and `wait()` succeed — the baseline test that unshare + chroot works.

### `test_proc_mount`
**Requires:** root, rootfs

Runs `test -f /proc/self/status` inside a container with `with_proc_mount()`. Verifies
that procfs is mounted correctly so the container can see its own kernel metadata.

### `test_capability_dropping`
**Requires:** root, rootfs

Calls `drop_all_capabilities()` and verifies `ash -c "exit 0"` still exits cleanly.
Proves the capability drop itself doesn't prevent a minimal shell from running.

### `test_selective_capabilities`
**Requires:** root, rootfs

Calls `with_capabilities(NET_BIND_SERVICE | CHOWN)` — keeps only two capabilities,
drops all others — and verifies the container exits cleanly.

### `test_resource_limits_fds`
**Requires:** root, rootfs

Sets `with_max_fds(100)` (RLIMIT_NOFILE) and runs `test "$(ulimit -n)" = 100` inside
the container. Verifies the rlimit is visible to the process as expected.

### `test_resource_limits_memory`
**Requires:** root, rootfs

Sets `with_memory_limit(512MB)` (RLIMIT_AS) and runs `exit 0`. Smoke-tests that the
rlimit can be applied without preventing the process from starting.

### `test_resource_limits_cpu`
**Requires:** root, rootfs

Sets `with_cpu_time_limit(60)` (RLIMIT_CPU) and runs `exit 0`. Smoke-tests that a
60-second CPU time limit can be applied without breaking a trivial process.

### `test_combined_features`
**Requires:** root, rootfs

Combines `MOUNT | UTS | CGROUP` namespaces, `with_proc_mount()`,
`with_capabilities(NET_BIND_SERVICE)`, `with_max_fds(500)`, and `with_memory_limit(256MB)`.
Verifies that multiple features coexist without conflict.

---

## Seccomp Filter Tests

### `test_seccomp_docker_blocks_reboot`
**Requires:** root, rootfs

Applies the Docker seccomp profile and runs `reboot` inside the container. Verifies
the process exits (with code 0 or 1) without actually rebooting — proving the `reboot`
syscall is blocked by the BPF filter.

### `test_seccomp_docker_allows_normal_syscalls`
**Requires:** root, rootfs

Applies the Docker seccomp profile and runs `echo`. Verifies that read, write, brk,
and other everyday syscalls are not blocked — the filter should only restrict dangerous ones.

### `test_seccomp_minimal_is_restrictive`
**Requires:** root, rootfs

Applies the minimal seccomp profile and attempts `exit 0`. Does not assert success or
failure — the minimal profile may be too strict for even `ash` to start. Verifies
only that the filter compiles and can be applied without a Rust error.

### `test_seccomp_profile_api`
**Type:** API-only

Verifies the four seccomp builder methods exist and compile (no process spawned). See
the API-only section.

### `test_seccomp_without_flag_works`
**Requires:** root, rootfs

Runs `echo` with no seccomp configuration at all. Confirms baseline operation is
unaffected when seccomp is not used.

---

## Phase 1 Security Tests

### `test_no_new_privileges`
**Requires:** root, rootfs

Calls `with_no_new_privileges(true)` and reads `/proc/self/status` inside the container.
Greps for `NoNewPrivs:\s*1` — the kernel sets this field when `PR_SET_NO_NEW_PRIVS` has
been applied, preventing privilege escalation via setuid binaries.

### `test_readonly_rootfs`
**Requires:** root, rootfs

Calls `with_readonly_rootfs(true)` and runs `touch /test_file`. Verifies the container
process runs cleanly (ash exits 0) even though the `touch` fails — the rootfs is
immutable via a bind-remount with `MS_RDONLY`.

### `test_masked_paths_default`
**Requires:** root, rootfs

Calls `with_masked_paths_default()` (which masks `/proc/kcore`, `/sys/firmware`, etc.)
and attempts `cat /proc/kcore`. Verifies the container completes without error — the
masked path is replaced with a bind mount of `/dev/null`, so reads return nothing
or an error that the shell handles gracefully.

### `test_masked_paths_custom`
**Requires:** root, rootfs

Calls `with_masked_paths(&["/proc/kcore", "/sys/firmware"])` with a custom list and
runs `echo`. Verifies that specifying masked paths manually doesn't prevent the
container from executing.

### `test_combined_phase1_security`
**Requires:** root, rootfs

Stacks all Phase 1 security features: `with_seccomp_default()`,
`with_no_new_privileges(true)`, `with_readonly_rootfs(true)`,
`with_masked_paths_default()`, `drop_all_capabilities()`. Verifies they coexist
and the container can still run `echo`.

---

## Phase 4: Filesystem Flexibility Tests

### `test_bind_mount_rw`
**Requires:** root, rootfs

Creates a temporary host directory, writes `hello.txt` into it, and mounts it
read-write at `/mnt/hostdir` via `with_bind_mount()`. Runs `cat /mnt/hostdir/hello.txt`
inside the container. Verifies that host files are accessible to the container.

### `test_bind_mount_ro`
**Requires:** root, rootfs

Mounts a temporary host directory read-only at `/mnt/ro` via `with_bind_mount_ro()`.
Runs `touch /mnt/ro/newfile` and captures the exit code. Verifies `exit=1` — the
write is rejected because the mount is read-only. The `MS_BIND | MS_RDONLY` remount
is required by the Linux kernel (two calls: bind, then remount-ro).

### `test_cli_volume_flag_ro`
**Requires:** root, rootfs

Verifies that the CLI `-v host:container:ro` and `-v host:container:rw` suffixes are
parsed correctly and produce the expected mount behaviour. Runs `remora run -v ...:ro`
and asserts that a write inside the container fails (`exit=1`); then runs with `:rw`
and asserts the write succeeds (`exit=0`).

This tests the `run.rs` parser path (distinct from `test_bind_mount_ro` which calls
`with_bind_mount_ro()` directly). Failure means the `rsplit_once(':')` fix that strips
`:ro`/`:rw` from the mount-target path has regressed, causing the suffix to be treated
as part of the filesystem path instead of a mount option.

### `test_tmpfs_mount`
**Requires:** root, rootfs

Configures a readonly rootfs via `with_readonly_rootfs(true)` and mounts a tmpfs at
`/tmp` via `with_tmpfs("/tmp", "size=10m,mode=1777")`. Runs `touch /tmp/testfile`.
Verifies that tmpfs can provide a writable island inside an otherwise immutable
container filesystem.

### `test_named_volume`
**Requires:** root, rootfs

Creates a named volume (`Volume::create("testvol")`), mounts it at `/data`, and runs
`echo persistent > /data/file.txt`. After `wait()`, reads `vol.path()/file.txt` on
the host and verifies the content persists. Confirms that named volumes survive
container exit. Cleans up with `Volume::delete("testvol")`.

---

## Phase 5: Cgroups v2 Resource Management Tests

### `test_cgroup_memory_limit`
**Requires:** root, rootfs

Creates a cgroup with `with_cgroup_memory(32MB)` and runs `dd if=/dev/urandom of=/dev/null bs=1M count=64`.
Because `dd` streams data without accumulating RSS, it typically won't OOM, but the
important thing is that the cgroup is created and the container runs under it without
error. Verifies the cgroup setup path works end-to-end.

### `test_cgroup_pids_limit`
**Requires:** root, rootfs

Sets `with_cgroup_pids_limit(4)` and forks 10 background `sleep 0` jobs in a shell
loop. Some forks will be denied by `pids.max`. Verifies that the container completes
(the shell handles fork failures gracefully) — tests that pids cgroup setup doesn't
crash the container.

### `test_cgroup_cpu_shares`
**Requires:** root, rootfs

Sets `with_cgroup_cpu_shares(512)` (writes `cpu.weight`) and runs `echo ok`.
Smoke-tests that CPU weight configuration doesn't interfere with container execution.
Does not verify proportional scheduling behaviour (would need a concurrent reference
process).

### `test_resource_stats`
**Requires:** root, rootfs

Spawns a container with `with_cgroup_memory(128MB)` and `with_cgroup_pids_limit(64)`,
then calls `child.resource_stats()` while the container may still be running.
Verifies the call returns a valid `ResourceStats` struct with `memory_current_bytes`,
`cpu_usage_ns`, and `pids_current` fields (all `u64`, so always ≥ 0).

### `test_cgroup_cleanup`
**Requires:** root, rootfs

Spawns with `with_cgroup_memory(64MB)`, records the child PID, calls `wait()`, then
checks that `/sys/fs/cgroup/remora-{pid}` no longer exists. Verifies that
`teardown_cgroup()` deletes the cgroup directory after the container exits.

---

## Phase 6: Native Networking Tests

### `test_loopback_network` — N1
**Requires:** root, rootfs

Calls `with_network(NetworkMode::Loopback)`. Inside `pre_exec`, after
`unshare(CLONE_NEWNET)`, `bring_up_loopback()` uses `ioctl(SIOCSIFFLAGS)` to set
`IFF_UP` on `lo`. Runs `ip addr show lo | grep -q '127.0.0.1'` inside the container.
Verifies that loopback is up with its standard address in an isolated net namespace.

### `test_bridge_network_ip` — N2
**Requires:** root, rootfs

Calls `with_network(NetworkMode::Bridge)`. `setup_bridge_network()` runs before fork,
creating a named netns (`rem-{pid}-{n}`), a veth pair, assigning `172.19.0.x/24` to
`eth0`, and attaching the host-side veth to `remora0`. The child joins the netns via
`setns()` in `pre_exec`. Runs `ip addr show eth0 | grep -q '172.19.0'` and verifies
`BRIDGE_IP_OK` — confirming the container sees its assigned IP from the first
instruction (no polling needed because setup is pre-fork).

### `test_bridge_network_veth_exists` — N2
**Requires:** root, rootfs

Spawns a bridge container running `sleep 2`. While it sleeps, queries
`ip link show {veth_name}` on the host (using `child.veth_name()` to get the
`vh-{hash}` interface name). Verifies the host-side veth exists while the container
is alive.

### `test_bridge_network_cleanup` — N2
**Requires:** root, rootfs

Spawns a bridge container with `ash -c "exit 0"` (exits immediately). Captures the
veth name before `wait()`, then calls `wait()`, then runs `ip link show {veth_name}`.
Verifies the veth is gone — `teardown_network()` calls `ip link del` in `Child::wait()`.
The immediate exit is safe because `setup_bridge_network()` runs before fork, so
there is no race between container startup and network setup.

### `test_bridge_netns_cleanup` — N2
**Requires:** root, rootfs

Spawns a bridge container with `exit 0`. Captures the named netns name from
`child.netns_name()` and verifies `/run/netns/{ns_name}` exists before `wait()`.
After `wait()`, verifies the path is gone. Closes a gap left by
`test_bridge_network_cleanup`, which only checks the veth — this test confirms
`ip netns del` in `teardown_network()` also runs successfully.

### `test_bridge_loopback_up` — N2
**Requires:** root, rootfs

Runs `ip addr show lo | grep -q '127.0.0.1'` inside a bridge-mode container.
Verifies that `lo` is up with `127.0.0.1` in addition to `eth0`. Loopback in bridge
mode is configured by `setup_bridge_network()` via
`ip -n {ns_name} link set lo up` before fork — different from Loopback mode which
uses an in-process `ioctl`.

### `test_bridge_gateway_reachable` — N2
**Requires:** root, rootfs

Runs `ping -c 1 -W 2 172.19.0.1` inside a bridge-mode container. Verifies actual
layer-3 connectivity: ICMP echo traverses `eth0` → veth pair → `remora0` bridge →
host, which replies with `172.19.0.1`. This is the only test that exercises a real
packet flowing through the full network stack, catching problems like missing ARP,
misconfigured routes, or a veth not attached to the bridge.

### `test_bridge_concurrent_spawn` — N2
**Requires:** root, rootfs

Spawns two bridge containers from separate threads simultaneously. Each thread builds
a `Command`, calls `spawn()`, and collects output entirely within the thread (no
non-`Send` types cross thread boundaries). Each container runs
`ip addr show eth0 | grep -m1 'inet ' | awk '{print $2}'` and emits its assigned IP.

Asserts:
- Both IPs are non-empty and in the `172.19.0.x/24` range
- The two IPs differ (`assert_ne!`)

Exercises the `flock(LOCK_EX)` IPAM lock (concurrent writes to `/run/remora/next_ip`)
and the `AtomicU32` namespace-name counter under real concurrency.

---

## Phase 6 N3 — NAT / MASQUERADE Tests

These three tests share a global `NAT_TEST_LOCK` mutex so they run serially.
All three check the nftables refcount state via `nft list table ip remora`,
which is global per-host state. Running them concurrently would cause spurious
failures when one test's container exits and sees a non-zero refcount left by a
sibling's still-running container.

### `test_nat_rule_added` — N3
**Requires:** root, rootfs

Spawns a bridge+NAT container running `sleep 2`. While it sleeps, runs
`nft list table ip remora` on the host and asserts exit 0. Failure would
indicate that `enable_nat()` did not install the MASQUERADE rule set, or that
`nft` is not available on the host.

### `test_nat_cleanup` — N3
**Requires:** root, rootfs

Spawns a bridge+NAT container with `ash -c "exit 0"` (exits immediately). After
`wait()`, runs `nft list table ip remora` and asserts non-zero exit. Failure
would indicate that `disable_nat()` did not remove the nftables table (refcount
not decremented to zero, or `nft delete table` failed silently).

### `test_nat_refcount` — N3
**Requires:** root, rootfs

Spawns two bridge+NAT containers: A (`sleep 2`) and B (`sleep 4`). Waits for A,
then asserts `nft list table ip remora` exits 0 (B still running — refcount ≥ 1).
Waits for B, then asserts it exits non-zero (refcount hits 0, table removed).
Failure would indicate the reference-counting logic in `enable_nat` /
`disable_nat` is incorrect — either decrementing too eagerly (table gone while B
runs) or not decrementing at all (table present after both exit).

### `test_nat_iptables_forward_rules` — N3
**Requires:** root, rootfs

Spawns a bridge+NAT container running `sleep 3`. While it sleeps, runs
`iptables -C FORWARD -s 172.19.0.0/24 -j ACCEPT` and
`iptables -C FORWARD -d 172.19.0.0/24 -j ACCEPT` on the host, asserting both
exit 0. After `wait()`, asserts the source rule is gone (exit non-zero).

These iptables rules are critical on hosts with UFW or Docker, which set
`iptables FORWARD policy DROP`. Without them, nftables MASQUERADE works for
ICMP but TCP/UDP is silently dropped — DNS resolution, HTTP requests, and
`apk add` all fail while ping succeeds. This was a real production bug.

Failure indicates `enable_nat()` is not adding the iptables FORWARD rules,
or `disable_nat()` is not cleaning them up.

---

## Phase 6 N4 — Port Mapping Tests

These three tests share the `#[serial(nat)]` key with the N3 tests (port-forward
rules live in the same `table ip remora`). All three use dedicated port numbers
(18080–18083) to avoid collision with real services on the host.

### `test_port_forward_rule_added` — N4
**Requires:** root, rootfs

Spawns a bridge+NAT container with `with_port_forward(18080, 80)` running `sleep 2`.
While it sleeps, runs `nft list chain ip remora prerouting` and asserts exit 0 and
that the output contains `dport 18080`. Failure would indicate that
`enable_port_forwards()` did not install the DNAT rule, or that the prerouting chain
was not created.

### `test_port_forward_cleanup` — N4
**Requires:** root, rootfs

Spawns a bridge+NAT container with `with_port_forward(18081, 80)` that exits
immediately (`ash -c "exit 0"`). After `wait()`, runs `nft list table ip remora`
and asserts non-zero exit (table gone). Failure would indicate that
`disable_port_forwards()` did not clean up nftables state, or that the port-forwards
state file was not cleared.

### `test_port_forward_independent_teardown` — N4
**Requires:** root, rootfs

Spawns A (`sleep 2`, port 18082→80) and B (`sleep 4`, port 18083→80), both with NAT.
Waits for A, then checks: prerouting chain still exists, A's rule (`dport 18082`)
is gone, B's rule (`dport 18083`) is still present. Waits for B, then asserts the
table is fully removed. Failure would indicate that `disable_port_forwards()` either
removed the wrong entries, failed to rebuild the prerouting chain from survivors, or
deleted the table prematurely while B was still running.

---

## Phase 6 N5 — DNS Tests

### `test_dns_resolv_conf` — N5
**Requires:** root, rootfs

Spawns a bridge+NAT container with `with_dns(&["1.1.1.1", "8.8.8.8"])` that runs
`cat /etc/resolv.conf` and captures stdout. Asserts the output contains both
`nameserver 1.1.1.1` and `nameserver 8.8.8.8`. Failure would indicate that the
per-container temp resolv.conf was not created, the bind mount over
`effective_root/etc/resolv.conf` failed, or the content was incorrect.
This test does not perform a live DNS lookup — it only verifies the file is visible
and correct inside the container. The shared Alpine rootfs is never modified.

---

## End-to-End Traffic Tests

These tests go beyond rule/config existence checks and verify that real packets
flow through the networking stack. They were added after discovering that nftables
rules can exist while iptables FORWARD policy DROP silently blocks TCP/UDP.

### `test_port_forward_end_to_end` — N4
**Requires:** root, rootfs, `nc` on host

Container A runs `echo HELLO_FROM_CONTAINER | nc -l -p 80` with
`with_port_forward(19090, 80)`. A temporary external network namespace
(`pf-test-client`) is created with its own veth pair to the host on
10.99.0.0/24, simulating a real external client. From that namespace,
`nc -w 2 10.99.0.1 19090` connects to the host on the forwarded port.
The traffic arrives on the `pf-test-h` veth, goes through nftables PREROUTING
(DNAT → container IP:80), then gets forwarded through the bridge to A.

Note: DNAT prerouting rules only apply to traffic arriving from external
interfaces, not locally-originated host packets (which go through OUTPUT) and
not bridge-internal traffic (hairpin routing issues). So this test creates a
separate network namespace as the client rather than connecting from the host
or from another bridge container.

Unlike `test_port_forward_rule_added` (which only checks the nftables rule string),
this proves the full DNAT path works: external traffic → nftables prerouting → DNAT →
FORWARD → bridge → container netns → container process → response back via conntrack.

### `test_udp_port_forward_rule_added` — N4-UDP
**Requires:** root, rootfs

Spawns a bridge+NAT container with `with_port_forward_udp(19095, 5000)`.
After 200 ms, queries nftables (`nft list chain ip remora-remora0 prerouting`)
and asserts the chain contains `udp dport 19095 dnat to <IP>:5000` and does NOT
contain `tcp dport 19095` (UDP-only mappings must not generate TCP rules).

Failure indicates UDP port mappings are silently ignored or the wrong nft protocol
token is emitted.  Container is SIGKILLed after the nftables check.

### `test_both_port_forward_rule_added` — N4-UDP
**Requires:** root, rootfs

Spawns a bridge+NAT container with `with_port_forward_both(19096, 53)`.
After 200 ms, queries nftables and asserts the prerouting chain contains BOTH
`tcp dport 19096 dnat to <IP>:53` AND `udp dport 19096 dnat to <IP>:53`.

Failure indicates the `Both` variant does not generate the two required rules,
which would break dual-protocol services (e.g. DNS, QUIC/HTTP3).

### `test_udp_proxy_threads_joined_on_teardown` — N4-UDP
**Requires:** root, rootfs

Starts a container with `with_port_forward_udp(19097, 5000)` and verifies:
1. While running: `UdpSocket::bind(127.0.0.1:19097)` fails (proxy holds the port).
2. After `SIGKILL` + `child.wait()`: the same bind succeeds (proxy thread was joined,
   inbound socket is closed, port is released).

This directly tests that `teardown_network` joins the per-port UDP proxy threads
(via `proxy_udp_threads.drain(..)` + `handle.join()`). Without the join, the
thread keeps the socket open and the port remains unavailable for a short window,
causing the test to fail.

### `test_bridge_cleanup_after_sigkill` — N2+N3
**Requires:** root, rootfs

Spawns a bridge+NAT container (`sleep 60`), records veth name, netns name, and
verifies iptables FORWARD rules exist. Then SIGKILLs the container and calls
`wait()`. Asserts all four resource types are cleaned up: veth pair, named netns,
nftables table, and iptables FORWARD rules.

All other cleanup tests use normal container exit. This catches teardown bugs that
only manifest when the container process dies unexpectedly — e.g. if `wait()` skips
`teardown_network()` or `disable_nat()` when the child was killed.

### `test_nat_end_to_end_tcp` — N3
**Requires:** root, rootfs, outbound internet

Spawns a bridge+NAT+DNS container that runs `wget --spider http://1.1.1.1/` and
asserts exit 0. Skips gracefully if the host has no outbound internet (checked via
host-side `ping -c1 -W2 1.1.1.1`).

This is the true end-to-end NAT test — TCP packets flow from the container through
MASQUERADE to the public internet and back. Existing NAT tests only verify that
nftables/iptables rules exist. Follows the same skip-if-no-internet pattern as
`test_pasta_connectivity`.

---

## Overlay Filesystem Tests

### `test_overlay_writes_to_upper`
**Requires:** root, rootfs

Creates temporary `upper` and `work` directories. Spawns a container with
`with_overlay(upper, work)` that writes `echo hello > /newfile`. After `wait()`:
asserts that `lower_dir/newfile` does **not** exist (lower layer is untouched),
and that `upper_dir/newfile` contains `"hello\n"`. Failure would indicate that
writes inside an overlay container are reaching the lower layer instead of the
upper layer — overlayfs copy-on-write is broken or the overlay was not mounted.

### `test_overlay_with_volume`
**Requires:** root, rootfs

Spawns a container with both `with_overlay(upper, work)` and
`with_volume(&vol, "/data")`. The container writes to the volume (`/data/vol_file.txt`)
and to a regular path (`/overlay_file.txt`). After `wait()`: asserts that the volume
file persists on the host, the regular write lands in the overlay upper dir (not the
rootfs), and the volume write does **not** appear in the overlay upper dir. Failure
would indicate that volume bind mounts are not correctly layered on top of the overlay
merged view, or that volume writes are leaking into the overlay upper directory.

### `test_overlay_lower_unchanged`
**Requires:** root, rootfs

Creates temporary `upper` and `work` directories. Records the original content of
`lower_dir/etc/hostname`, then spawns a container that runs
`echo modified > /etc/hostname`. After `wait()`: asserts that `lower_dir/etc/hostname`
is unchanged (same content as before), and that `upper_dir/etc/hostname` contains
`"modified\n"`. Failure would indicate that modifying an existing lower-layer file
writes through to the lower directory instead of producing a copy-on-write in the
upper layer.

### `test_overlay_merged_cleanup`
**Requires:** root, rootfs

Spawns a container with `with_overlay(upper, work)` that runs `true` (exits
immediately). Records the specific merged dir path via `child.overlay_merged_dir()`
before calling `wait()`. After `wait()`: asserts that neither the merged dir nor its
parent (`/run/remora/overlay-{pid}-{n}/`) exist. Failure would indicate that `wait()`
failed to call `remove_dir` on the merged directory and its parent, leaving stale
directories on the host. The test checks the specific path rather than scanning the
whole directory to avoid false failures from other overlay tests running in parallel.

---

## OCI Lifecycle Tests

These tests exercise the five OCI Runtime Spec v1.0.2 subcommands (`create`, `start`,
`state`, `kill`, `delete`) via the `remora` binary. They use minimal OCI bundles with
`rootfs/` symlinked to the Alpine rootfs and inline `config.json`.

### `test_oci_create_start_state`
**Requires:** root, rootfs

Writes a minimal `config.json` running `sleep 2`. Runs `remora create`, asserts
`remora state` returns `"created"`. Runs `remora start`, asserts `"running"`. Polls
until the process exits, asserts `"stopped"`. Runs `remora delete`, asserts the state
dir is gone. Failure indicates broken create/start synchronization, incorrect
state.json transitions, or wrong liveness detection via `kill(pid, 0)`.

### `test_oci_kill`
**Requires:** root, rootfs

Spawns a long-running container (`sleep 60`), starts it, then sends `SIGKILL` via
`remora kill` and polls until `remora state` reports `"stopped"`. Uses SIGKILL because
the container is PID 1 in a PID namespace — the kernel drops unhandled signals (like
SIGTERM) for namespace-init processes. Failure indicates that `cmd_kill` is not finding
the correct host-visible PID, or that liveness detection does not detect the exit.

### `test_oci_delete_cleanup`
**Requires:** root, rootfs

Runs `/bin/true` through the full create→start→wait-for-stopped lifecycle, records
the state dir path, runs `remora delete`, and asserts the directory is removed. Failure
indicates `cmd_delete` is not calling `remove_dir_all`, or is checking liveness
incorrectly and refusing to delete a stopped container.

### `test_oci_bundle_mounts`
**Requires:** root, rootfs

Creates a `config.json` with a `tmpfs` mount at `/scratch` and a process that writes
to `/scratch/test.txt`. Runs the full create→start→stopped lifecycle and asserts that
`remora delete` succeeds. Failure indicates that OCI `mounts` entries are not being
applied from `config.json`, or that tmpfs mount handling in `build_command()` is broken.

### `test_oci_capabilities`
**Requires:** root, rootfs

Creates a `config.json` with `process.capabilities` specifying only `CAP_CHOWN` in
the bounding and effective sets. The container runs `/usr/bin/id` and must exit
successfully. Asserts the full create→start→stopped lifecycle completes cleanly.
Failure indicates that OCI `process.capabilities` parsing or the
`with_capabilities()` wiring in `build_command()` is broken.

### `test_oci_masked_readonly_paths`
**Requires:** root, rootfs

Creates a `config.json` with `linux.maskedPaths: ["/proc/kcore"]` and
`linux.readonlyPaths: ["/sys/kernel"]`. The container verifies:
- `/proc/kcore` is masked (bind-mounted `/dev/null` → zero bytes readable)
- `/sys/kernel` is read-only (`touch /sys/kernel/test` is denied)

The shell command exits 0 only if both checks pass. Asserts the full lifecycle
completes cleanly. Failure indicates that `linux.maskedPaths` or
`linux.readonlyPaths` from OCI config are not being applied, or the wiring
into `with_masked_paths()` / `with_readonly_paths()` in `build_command()` is broken.

### `test_oci_resources`
**Requires:** root, rootfs

Creates a `config.json` with `linux.resources` setting a 64 MiB memory limit and a PID
limit of 50. The container reads `/sys/fs/cgroup/memory.max` and `/sys/fs/cgroup/pids.max`.
Failure indicates that `linux.resources` parsing from OCI config or the wiring into
`with_cgroup_memory()` / `with_cgroup_pids_limit()` is broken.

### `test_oci_rlimits`
**Requires:** root, rootfs

Creates a `config.json` with `process.rlimits` capping `RLIMIT_NOFILE` to 128. The container
runs `ulimit -n` (exits 0 if the limit is accepted). Failure indicates that `process.rlimits`
parsing or the wiring into `with_rlimit()` in `build_command()` is broken.

### `test_oci_sysctl`
**Requires:** root, rootfs

Creates a `config.json` with `linux.sysctl: {"kernel.domainname": "testdomain.local"}`. The
container greps for that value in `/proc/sys/kernel/domainname`. The sysctl is set in the
private UTS namespace so it doesn't affect the host. Failure indicates that `linux.sysctl`
parsing or the `with_sysctl()` / pre_exec write to `/proc/sys/` is broken.

### `test_oci_hooks`
**Requires:** root, rootfs

Creates a `config.json` with a `prestart` hook that touches a sentinel file, and a `poststop`
hook that touches a different sentinel file. Asserts the prestart sentinel exists after
`remora create` and the poststop sentinel exists after `remora delete`. Failure indicates
that OCI `hooks` parsing or the `run_hooks()` placement in `cmd_create()` / `cmd_delete()`
is broken.

### `test_oci_seccomp`
**Requires:** root, rootfs

Creates a `config.json` with `linux.seccomp` using a default-allow policy that denies only
`ptrace`, `personality`, and `bpf`. The container runs `/bin/echo hello` which must succeed.
Failure indicates that `linux.seccomp` parsing from OCI config, the `filter_from_oci()`
function in `src/seccomp.rs`, or the `with_seccomp_program()` wiring is broken.

### `test_oci_cap_all_known_names_round_trip` (unit)
**Requires:** nothing (unit test in `src/oci.rs`)

Asserts that all 41 Linux capability names (with `CAP_` prefix) map to a non-None value
via `oci_cap_to_flag`. Failure means an OCI bundle specifying that capability will silently
drop it rather than applying it to the container's capability set.

### `test_oci_cap_without_prefix` (unit)
**Requires:** nothing (unit test in `src/oci.rs`)

Verifies that `oci_cap_to_flag` accepts names both with and without the `CAP_` prefix,
and returns `None` for genuinely unknown names.

### `test_oci_signal_names` (unit)
**Requires:** nothing (unit test in `src/oci.rs`)

Verifies the signal name→number table covers all signal names sent by `opencontainers/runtime-tools`
including `SIGWINCH`, `SIGCHLD`, `SIGCONT`, `SIGSTOP`, `SIGQUIT`, `SIGSYS`, and numeric forms.

### `test_oci_kernel_mounts`
**Requires:** root, rootfs

Creates an OCI bundle with proc, sysfs, devpts, mqueue mounts (matching standard runc/containerd
output) and runs `ls /proc/self` inside. Failure indicates the OCI mount-type dispatch
(`oci.rs`) or the `KernelMount` pre_exec loop (`container.rs`) is broken. Primary gate for
`opencontainers/runtime-tools` conformance since nearly every test bundle uses these mounts.

### `test_oci_create_bundle_flag`
**Requires:** root, rootfs

Invokes `remora create --bundle <path> <id>` (named flag, OCI-standard form) and verifies the
container reaches "created" state. Failure indicates the `--bundle` CLI flag is not accepted,
which would prevent the `opencontainers/runtime-tools` conformance harness from invoking remora.

### `test_oci_create_pid_file`
**Requires:** root, rootfs

Invokes `remora create --bundle <path> --pid-file <path> <id>` and verifies the pid file is
written with a positive integer that matches the PID reported in `state.json`. Failure indicates
`--pid-file` is not written or contains the wrong PID, which breaks containerd / CRI-O integration.

---

### `test_oci_rootfs_propagation`
**Requires:** root, rootfs

Creates an OCI bundle with `linux.rootfsPropagation: "private"` and runs `echo ok` inside it.
Verifies the container starts and completes successfully. Failure indicates the `rootfsPropagation`
field is not parsed, the mapping to `MS_PRIVATE|MS_REC` is wrong, or the `mount(2)` call fails,
which would cause the container to refuse to start whenever a runtime-tools bundle specifies
mount propagation.

---

### `test_oci_cgroups_path`
**Requires:** root, rootfs

Creates an OCI bundle with `linux.cgroupsPath` set to a unique name and runs `echo ok` inside it.
Verifies the container starts and completes successfully. Failure indicates the `cgroupsPath` field
is not wired from OCI config through to `CgroupConfig.path`, which would break runtimes that
rely on predictable cgroup hierarchy placement (e.g. systemd-managed slices).

---

### `test_oci_create_container_hook_in_ns`
**Requires:** root, rootfs

Creates an OCI bundle with a `createContainer` hook script that writes the inode of
`/proc/self/ns/mnt` to a temp file. After `remora create`, reads the recorded inode and compares
it to the host's mount namespace inode (`/proc/1/ns/mnt`). Asserts they differ, confirming the
hook executed inside the container's mount namespace. Failure means `createContainer` hooks run
in the host namespace, violating the OCI spec and breaking runtimes that use these hooks to
inject config (e.g. seccomp, apparmor profiles) into the container environment.

---

### `test_oci_start_container_hook_in_ns`
**Requires:** root, rootfs

Creates an OCI bundle with a `startContainer` hook script that writes the inode of
`/proc/self/ns/mnt` to a temp file. After `remora start`, reads the recorded inode and compares
it to the host's mount namespace inode. Asserts they differ, confirming the hook executed inside
the container's mount namespace before the user process was exec'd. Failure means `startContainer`
hooks either do not run at all or run in the host namespace, violating the OCI spec.

---

## Rootless Mode Tests

The following tests only execute when the test binary is run **without root** (no `sudo`).
When run as root (as in the standard CI invocation), they print a skip message and exit.
To run these tests:

```bash
cargo test --test integration_tests test_rootless
cargo test --test integration_tests test_user_namespace_explicit
```

### `test_rootless_basic`
**Requires:** non-root user, rootfs

Spawns a container that runs `/bin/id` without any explicit namespace configuration beyond
`MOUNT | UTS`. The rootless auto-configuration adds `Namespace::USER` and a uid_map that
maps `{container 0 → host UID}`. Asserts that the output contains `uid=0`, confirming
that the process appears as root inside the container's user namespace. Failure indicates
that rootless auto-configuration (auto-add USER namespace + uid_map) is not working.

### `test_rootless_loopback`
**Requires:** non-root user, rootfs

Spawns a container with `NetworkMode::Loopback` without root. Verifies that `ping 127.0.0.1`
succeeds inside the container. Rootless auto-config adds USER namespace; combined with
the private NET namespace the process gains the capability to bring up `lo`. Failure
indicates that rootless + loopback networking is broken.

### `test_rootless_bridge_rejected`
**Requires:** non-root user, rootfs

Calls `spawn()` with `NetworkMode::Bridge` as a non-root user. Asserts that `spawn()`
returns an `Err` whose message mentions `root` or `rootless`. Failure indicates that the
rootless bridge-mode guard is not in place.

### `test_user_namespace_explicit`
**Requires:** root

Runs `/usr/bin/id` as root with an explicit `Namespace::USER` and an identity uid/gid map
(`{inside: 0, outside: 0, count: 1}`). No chroot or MOUNT namespace is used — the rootfs
lives under `/home/cb/` which is not traversable from inside a USER namespace with a
single-uid map (DAC_OVERRIDE only applies for inodes whose uid is in the map). Asserts the
container process outputs `uid=0`. Failure indicates a regression in the uid_map writing
path or the MS_PRIVATE MNT_LOCKED skip logic.

---

## Pasta Networking Tests

The following tests verify `NetworkMode::Pasta` (user-mode networking via the `pasta`
binary from the passt project). All tests skip gracefully when `pasta` is not installed.
All require a non-root user — pasta's privilege-dropping (root→nobody via an internal
user namespace) makes it unable to access container namespace file descriptors when run
as root. pasta is designed for rootless mode.

To run these tests:

```bash
# All pasta tests — run without sudo:
cargo test --test integration_tests test_pasta
```

### `test_pasta_interface_exists`
**Requires:** non-root user, rootfs, pasta installed

Spawns a container with `NetworkMode::Pasta`, sleeps 1 second to let pasta attach, then
runs `ip addr show`. Makes two assertions:
1. A non-loopback interface exists — pasta attached its TAP to the container's netns.
2. That interface has an `inet` address that is not 127.x — pasta's `--config-net` flag
   configured the IP inside the netns (without this, the TAP would exist but have no IP).

Failure on (1) means `setup_pasta_network()` is not being called or pasta cannot attach.
Failure on (2) means `--config-net` is not being passed, so the container has a TAP
with no address — no connectivity is possible.

### `test_pasta_rootless`
**Requires:** non-root user, rootfs, pasta installed

Same assertions as `test_pasta_interface_exists` but specifically exercises the rootless
auto-detection path: `Namespace::USER` is not set explicitly — remora adds it automatically
when `getuid() != 0`. Confirms that the USER+NET two-phase unshare and pasta still coexist
correctly when rootless mode is triggered implicitly.

### `test_pasta_connectivity`
**Requires:** non-root user, rootfs, pasta installed, outbound internet access

Spawns a container with `NetworkMode::Pasta`, sleeps 2 seconds (TAP attach + `--config-net`
routing setup), then runs `wget -q -T 5 --spider http://1.1.1.1/` (HEAD request — no body
to write, avoiding `/dev/null` which doesn't exist as a device node in the chroot). Asserts
the command exits 0 and prints `CONNECTED`. This is the end-to-end connectivity check — it verifies
that packets actually flow through pasta's relay to the internet, not just that the
interface exists and has an IP. Failure indicates pasta's packet relay is broken or outbound
internet is unavailable in the test environment.

---

## PID Namespace Tests

### `test_pid_namespace_repeated_fork`
**Requires:** root, rootfs

Regression test for a bug where `unshare(CLONE_NEWPID)` left the container process outside
the new PID namespace. Only the container's children entered it, so the first forked child
became PID 1. When that child exited, the kernel marked the namespace defunct and every
subsequent `fork()` failed with ENOMEM — even with abundant system memory.

Runs a shell loop that forks an external command (`sleep 0`) five times. All five forks must
succeed and the container must print `FORKS_OK`. Failure indicates the double-fork mechanism
in `pre_exec` (which makes the container process PID 1 in the new namespace) is broken.

---

## Container Linking Tests

### `test_container_link_hosts`
**Requires:** root, rootfs

Starts container A on bridge networking, writes its state (including bridge IP) to
`/run/remora/containers/link-test-a/state.json`, then starts container B with
`with_link("link-test-a")`. Reads B's `/etc/hosts` and verifies it contains A's bridge
IP and hostname. Failure indicates that link resolution, hosts file generation, or the
`/etc/hosts` bind-mount injection is broken.

### `test_container_link_alias`
**Requires:** root, rootfs

Same setup as `test_container_link_hosts`, but uses `with_link_alias("link-alias-a", "db")`.
Verifies B's `/etc/hosts` contains both the alias "db" and the original container name
"link-alias-a" on the same line. Failure indicates alias handling in the hosts file
generation is broken.

### `test_container_link_ping`
**Requires:** root, rootfs

Starts container A on bridge (running `sleep`), then starts container B linked to A and
runs `ping -c1 -W2 link-ping-a`. Verifies the ping succeeds, proving both `/etc/hosts`
name resolution and bridge network connectivity work end-to-end. Failure indicates that
the hosts entry is incorrect, the bridge is misconfigured, or containers can't reach each
other.

### `test_container_link_tcp`
**Requires:** root, rootfs

Starts container A on bridge running `echo HELLO_FROM_A | nc -l -p 8080` (a one-shot TCP
server). Registers A's state, then starts container B linked to A. B runs
`nc -w 2 link-tcp-a 8080` to connect by name and capture the response.

Unlike `test_container_link_ping` (ICMP only), this proves TCP connections work across
linked containers — the same protocol used by real services. This test was motivated by
a real bug where iptables `FORWARD policy DROP` (from UFW/Docker) blocked TCP/UDP while
allowing ICMP, making ping succeed but all real traffic fail.

Failure indicates TCP traffic cannot traverse the bridge between containers, possibly
due to missing iptables FORWARD rules in `enable_nat()` or bridge forwarding issues.

### `test_container_link_missing`
**Requires:** root, rootfs

Attempts to spawn a container with `with_link("nonexistent-container-xyz")`. Verifies
that spawn fails with an error message that mentions the missing container name. Failure
indicates that link resolution doesn't properly validate the target container exists before
proceeding with the spawn.

---

## Module: `images`

### `test_layer_extraction`
**Requires:** root

Creates a synthetic tar.gz layer containing two files (one in a subdirectory), extracts
it via `image::extract_layer()`, and verifies the files exist with correct content in
the content-addressable layer store. Failure indicates the tar+gzip extraction pipeline
or layer store layout is broken.

### `test_multi_layer_overlay_merge`
**Requires:** root, rootfs

Creates two temporary layers: bottom (rootfs + `/layer-bottom`) and top (`/layer-top`).
Uses `with_image_layers()` to mount them via overlayfs. Runs `cat` inside the container
to verify both files are visible. Failure indicates multi-layer overlayfs mount construction
or `lowerdir` ordering is broken.

### `test_multi_layer_overlay_shadow`
**Requires:** root, rootfs

Creates bottom layer with `/shadow-file` containing "bottom-value" and top layer with
`/shadow-file` containing "top-value". Uses `with_image_layers()` to verify the top
layer's file shadows the bottom. Failure indicates overlayfs layer ordering (top-first
lowerdir) is incorrect.

### `test_image_layers_cleanup`
**Requires:** root, rootfs

Spawns a container with `with_image_layers()`, captures the overlay merged-dir path,
waits for exit, then verifies the ephemeral overlay directory (merged + upper + work)
was cleaned up by `wait()`. Failure indicates the cleanup logic for image-layer overlay
dirs is broken.

### `test_pull_and_run_real_image`
**Requires:** root, network access
**Ignored by default** — run with `--ignored`

End-to-end test of the full OCI image pipeline. Pulls `alpine:latest` from Docker Hub
using the `remora` binary, loads the manifest, mounts layers via `with_image_layers()`,
and runs `cat /etc/alpine-release` inside the container. Verifies the output is a valid
Alpine version string. Failure indicates a regression anywhere in the chain: registry
pull, layer extraction, manifest persistence, multi-layer overlay mount, or container exec.

---

## Module: `exec`

Tests for `remora exec` — running commands inside running containers via
namespace join + `/proc/{pid}/root` chroot.

### `test_exec_basic`
**Requires:** root, rootfs

Starts a `sleep 30` container with UTS+MOUNT namespaces (no PID namespace —
see note below), then spawns an exec'd process (`/bin/cat /etc/os-release`) by
joining the container's mount namespace via `setns()` + `fchdir()` +
`chroot(".")` in a pre_exec callback. Verifies exit code 0 and non-empty output.

Failure indicates that the setns + fchdir + chroot pattern used by `remora exec`
is broken — either `setns()` fails, fchdir to the container root fd doesn't
work, or the exec'd process can't see the container's filesystem.

**Note:** PID namespace is omitted because `Namespace::PID` triggers a
double-fork where `container.pid()` returns the intermediate process (which
never execs and stays in the host namespaces), not the actual container. The
real `remora exec` CLI gets the correct PID from state.json.

### `test_exec_sees_container_filesystem`
**Requires:** root, rootfs

Starts a container that writes `EXEC_MARKER_12345` to `/tmp/exec-marker` (on
a tmpfs), then exec's `/bin/cat /tmp/exec-marker` via mount namespace join.
Verifies the output matches the marker value.

Failure indicates the exec'd process is not correctly entering the container's
mount namespace — it would see the host's `/tmp` instead of the container's
tmpfs, and the marker file would not exist. The `fchdir(root_fd) + chroot(".")`
technique (same as `nsenter(1)`) is critical here: a plain `chroot("/")` after
`setns(MOUNT)` would chroot to the host root, not the container's.

### `test_exec_environment`
**Requires:** root, rootfs

Starts a container with `FOO=bar_from_container` in its environment, reads
`/proc/{pid}/environ` to discover the env vars, applies them to the exec'd
command (`/bin/sh -c 'echo $FOO'`), and verifies the output is
`bar_from_container`.

Failure indicates that `/proc/{pid}/environ` reading or env propagation to
the exec'd process is broken.

### `test_exec_nonrunning_container_fails`
**Requires:** root

Verifies that `kill(999999, 0)` returns false (PID not alive) and
`/proc/999999/root` does not exist. This is the guard logic `remora exec`
uses to reject exec into stopped containers.

Failure indicates a kernel or procfs anomaly where dead PIDs still appear alive.

### `test_exec_joins_pid_namespace`
**Requires:** root, rootfs

Starts a detached container with `remora run -d --rootfs alpine /bin/sleep 30`.
The `--rootfs` path always enables `Namespace::PID`, so `state.pid` is the
intermediate process P whose `/proc/P/ns/pid` is the host PID namespace, but
`/proc/P/ns/pid_for_children` points to the container's PID namespace.

Runs `remora exec <name> readlink /proc/self/ns/pid` and asserts the output
matches `readlink /proc/{intermediate_pid}/ns/pid_for_children` read from the host.

Failure indicates `discover_namespaces` is not using the `pid_for_children` fallback
or the double-fork in `container.rs` step 1.65 is not putting the exec'd process in
the target PID namespace. A failing test means `ps` inside exec'd shells shows host
PIDs instead of container-scoped PIDs.

---

## Watcher Process Tests (`watcher` module)

### `test_watcher_kill_propagates_to_container`
**Requires:** root, rootfs

Starts a detached container with `remora run -d --rootfs alpine /bin/sleep 300`.
Reads `state.pid` (the intermediate process P), then reads P's `PPid` from
`/proc/<P>/status` to find the watcher PID.  Sends `SIGKILL` to the watcher
and polls for up to 3 seconds to verify the container process P also dies.

This tests that `PR_SET_CHILD_SUBREAPER` is effective: when the watcher is
killed, P (and therefore C inside the PID namespace) is re-parented to the
watcher rather than to host init, so the watcher's death triggers P's
`PR_SET_PDEATHSIG` in one hop.

Failure means the container process survives after the watcher is killed —
either the subreaper prctl was not called, or the kernel did not honour it.
A failing test indicates containers would become orphaned on an unexpected
watcher crash (OOM kill, etc.).

### `healthcheck_tests::test_probe_child_pid_is_killable`
**Requires:** root, rootfs

Verifies that a health-probe child process can be SIGKILL'd from outside, which
is the mechanism `run_probe` uses to clean up a timed-out probe.

Starts a container, then spawns a second `Command::new("sleep").args(["300"])`
inside the container's rootfs (via `with_chroot("/proc/{pid}/root")`).  Records
the spawned probe's host PID, sends `SIGKILL` to it, calls `probe.wait()` to
reap the zombie, then asserts `kill(probe_pid, 0)` returns `ESRCH` — confirming
the PID slot was released.

Failure means that after SIGKILL + wait the process still appears alive (e.g.
because zombie reaping didn't work), which would prevent the health monitor from
detecting that a timed-out probe child was successfully cleaned up.

---

## Log Relay Tests (`cli::relay` unit tests)

These tests live directly in `src/cli/relay.rs` and run via `cargo test --bin remora`
(no root required).

### `cli::relay::tests::test_relay_captures_stdout_and_stderr`
**Requires:** none (no root, no rootfs)

Spawns `sh -c "printf 'hello stdout'; printf 'hello stderr' >&2"` with piped
stdio, passes the handles to `start_log_relay`, joins the relay thread after
`child.wait()`, and asserts both log files contain the expected strings.

Failure indicates the epoll relay loop is not writing pipe data to the log files
(e.g. fd registration failed, write error was silently dropped, or the thread
exited before draining the pipe).

### `cli::relay::tests::test_relay_large_output`
**Requires:** none (no root, no rootfs)

Spawns `yes x | head -c 65536` (65 536 bytes — 8× the `BUF` read size) and
relays its stdout to a log file. After the relay thread finishes, asserts the
log file is exactly 65 536 bytes.

Failure indicates that multi-cycle relay (where epoll fires multiple times because
data exceeds one read buffer) is losing or truncating data.

### `cli::relay::tests::test_relay_none_handles`
**Requires:** none (no root, no rootfs)

Calls `start_log_relay(None, None, ...)` and joins the thread. Verifies the relay
exits immediately when no pipe fds are registered.

Failure indicates the relay loop hangs or panics when given empty input.

---

## Minimal /dev Tests (`dev` module)

### `test_dev_minimal_devices`
**Requires:** root + rootfs

Spawns a container with `with_dev_mount()` and lists `/dev/`. Asserts that safe
devices (`null`, `zero`, `random`, `urandom`, `full`, `tty`) are present, and
host-specific devices (`sda`, `nvme`, `video`) are absent.

Failure indicates the minimal /dev setup is not populating safe devices, or that
host device nodes are leaking into the container.

### `test_dev_null_works`
**Requires:** root + rootfs

Runs `echo ok > /dev/null && echo pass` inside a container with `with_dev_mount()`.
Asserts that the output contains "pass", confirming `/dev/null` is a functional
device (accepts writes without error).

Failure indicates `/dev/null` is not properly bind-mounted from the host.

### `test_dev_zero_works`
**Requires:** root + rootfs

Runs `head -c 4 /dev/zero | wc -c` inside a container with `with_dev_mount()`.
Asserts that output contains "4", confirming `/dev/zero` produces zero bytes.

Failure indicates `/dev/zero` is not properly bind-mounted from the host.

### `test_dev_symlinks`
**Requires:** root + rootfs

Checks that `/dev/fd`, `/dev/stdin`, `/dev/stdout`, and `/dev/stderr` are
symlinks inside a container with `with_dev_mount()`.

Failure indicates the minimal /dev setup is not creating the standard symlinks
that many programs depend on.

### `test_dev_pts_exists`
**Requires:** root + rootfs

Checks that `/dev/pts` and `/dev/shm` directories exist inside a container
with `with_dev_mount()`.

Failure indicates the minimal /dev setup is not creating the required
subdirectories for PTY allocation and shared memory.

---

## Rootless Cgroups

These tests exercise cgroup v2 delegation for non-root users. They skip
automatically if `is_delegation_available()` returns false (no v2, no
delegated controllers, or non-writable cgroup tree).

Run without root:
```bash
cargo test --test integration_tests rootless_cgroups -- --test-threads=1
```

### `test_rootless_cgroup_memory`
**Requires:** non-root + rootfs + cgroup v2 delegation

Sets `with_cgroup_memory(64MB)` on a rootless container and reads
`/sys/fs/cgroup/memory.max` inside it. Asserts the value is `67108864`.

Failure indicates the rootless cgroup path was not created, the memory
controller is not delegated, or the child was not moved into the sub-cgroup.

### `test_rootless_cgroup_pids`
**Requires:** non-root + rootfs + cgroup v2 delegation

Sets `with_cgroup_pids_limit(16)` on a rootless container and reads
`/sys/fs/cgroup/pids.max` inside it. Asserts the value is `16`.

Failure indicates the pids controller is not delegated or the limit was
not written to the sub-cgroup.

### `test_rootless_cgroup_cleanup`
**Requires:** non-root + rootfs + cgroup v2 delegation

Spawns a rootless container with a memory cgroup, waits for it to exit,
then checks that the sub-cgroup directory (`remora-{pid}`) under the
user's cgroup slice has been removed.

Failure indicates `teardown_rootless_cgroup()` did not successfully
remove the directory, which would leak cgroup entries over time.

---

## Rootless ID Mapping Tests (`rootless_idmap`)

Tests for multi-UID/GID mapping via `newuidmap`/`newgidmap` helpers and
subordinate ID ranges from `/etc/subuid` and `/etc/subgid`.

```bash
cargo test --test integration_tests rootless_idmap -- --test-threads=1
```

### `test_rootless_multi_uid_maps_written`
**Requires:** non-root + rootfs + newuidmap/newgidmap + subuid/subgid ranges

Spawns a rootless container without explicitly setting UID maps, letting
auto-config detect subordinate ranges and use the helpers. Reads
`/proc/self/uid_map` inside the container and asserts at least 2 mapping
lines are present (container root → host UID, and subordinate range).

Failure indicates the auto-detection of subordinate ranges failed, the
pipe+thread sync mechanism deadlocked, or `newuidmap` did not write
the multi-range mapping.

### `test_rootless_multi_uid_file_ownership`
**Requires:** non-root + rootfs + newuidmap/newgidmap + subuid/subgid ranges

Spawns a rootless container with multi-UID auto-config and runs
`stat -c '%u' /etc/passwd`. Asserts the file is owned by UID 0 (root)
inside the container.

Failure indicates files owned by root in the image are showing up as
`nobody` (65534) due to missing subordinate UID mappings, meaning the
multi-range mapping was not applied.

### `test_rootless_single_uid_fallback`
**Requires:** non-root + rootfs

Spawns a rootless container with an explicit single-UID map (bypassing
auto-config). Runs `id -u` and asserts it prints `0`.

Failure indicates the single-UID fallback path (existing behavior) is
broken, which would be a regression from the multi-UID changes.

---

## JSON Output Tests

These tests verify the `--format json` flag on all list commands and the
`container inspect` command. They exercise create→list→remove→list cycles
to ensure JSON output is correct and consistent.

### `test_volume_ls_json`
**Requires:** root

Creates a volume, runs `volume ls --format json`, and verifies the JSON array
contains an entry with the correct `name` and `path` fields. Removes the volume
and verifies the entry is gone from the JSON output.

Failure indicates JSON serialization of volumes is broken or the `--format`
flag is not wired correctly to `cmd_volume_ls`.

### `test_rootfs_ls_json`
**Requires:** root

Imports a rootfs entry (symlink to `/tmp`), runs `rootfs ls --format json`,
and verifies the JSON array contains an entry with the correct `name` and
`path` fields. Removes the entry and verifies it is gone from the JSON output.

Failure indicates JSON serialization of rootfs entries is broken or the
`--format` flag is not wired correctly to `cmd_rootfs_ls`.

### `test_ps_json_and_inspect`
**Requires:** root

Writes a synthetic container `state.json` to the containers directory, verifies
`ps -a --format json` includes the container with the correct name. Runs
`container inspect <name>` and verifies the returned JSON object has `name`,
`pid`, and `status` fields. Removes the container via `rm` and verifies it is
gone from the JSON listing.

Failure indicates JSON serialization of container state is broken, the
`--format` flag is not wired correctly, or `container inspect` does not work.

### `test_image_ls_json`
**Requires:** root

Runs `image ls --format json` and verifies the output is a valid JSON array.
If images are present, validates each entry has `reference`, `digest`, and
`layers` fields. If no images exist, verifies the output is `[]`.

Failure indicates JSON serialization of image manifests is broken or the
`--format` flag is not wired correctly to `cmd_image_ls`.

---

## Build Instructions (ENTRYPOINT, LABEL, USER)

### `test_parse_entrypoint_json`
**Requires:** neither root nor rootfs (parser-only)

Parses `ENTRYPOINT ["python3", "-m", "http.server"]` and verifies it produces
`Instruction::Entrypoint` with the correct argument list. Also checks that CMD
on the next line is parsed independently.

Failure indicates the ENTRYPOINT JSON-form parser is broken.

### `test_parse_entrypoint_shell_form`
**Requires:** neither root nor rootfs (parser-only)

Parses `ENTRYPOINT /usr/bin/myapp --flag` (shell form) and verifies it is
wrapped in `/bin/sh -c ...` like CMD shell form.

Failure indicates shell-form ENTRYPOINT wrapping is broken.

### `test_parse_label_quoted_and_unquoted`
**Requires:** neither root nor rootfs (parser-only)

Parses `LABEL maintainer="Jane Doe"` and `LABEL version=2.0`, verifying both
quoted and unquoted value forms produce correct key-value pairs.

Failure indicates LABEL value parsing or quote stripping is broken.

### `test_parse_user_with_gid`
**Requires:** neither root nor rootfs (parser-only)

Parses `USER 1000:1000` and verifies the full string is captured as-is
(parsing uid:gid is the runtime's responsibility, not the parser's).

Failure indicates USER instruction parsing is broken.

### `test_image_config_labels_serde_roundtrip`
**Requires:** neither root nor rootfs (serialization-only)

Creates an `ImageConfig` with labels, serializes to JSON, deserializes, and
verifies labels survive the round-trip. Also verifies that missing `labels`
key in JSON deserializes to an empty HashMap (serde default).

Failure indicates the `labels` field has broken serde attributes.

### `test_image_config_user_field`
**Requires:** neither root nor rootfs (serialization-only)

Verifies `ImageConfig.user` and `ImageConfig.entrypoint` round-trip through
JSON correctly, and that missing `user` key defaults to empty string.

Failure indicates the `user` or `entrypoint` field serde default is broken.

### `test_full_remfile_with_all_instructions`
**Requires:** neither root nor rootfs (parser-only)

Parses a Remfile using every supported instruction type (FROM, LABEL, ENV,
USER, WORKDIR, COPY, RUN, ENTRYPOINT, CMD, EXPOSE) and verifies the complete
instruction list has 10 entries of the correct variant types.

Failure indicates a regression in any instruction parser.

### `test_parse_arg_instruction`
**Requires:** neither root nor rootfs (parser-only)

Parses a Remfile containing ARG before FROM (Docker compat) and ARG after FROM,
verifying both produce correct `Instruction::Arg` variants with names and defaults.
Also exercises `substitute_vars` with `$VAR`, `${VAR}`, and `$$` escape sequences.

Failure indicates the ARG parser or variable substitution engine is broken.

### `test_remignore_filtering`
**Requires:** neither root nor rootfs

Creates a temporary directory with a `.remignore` file excluding `*.log` and `build/`.
Populates the directory with matching and non-matching files, then runs a filtered copy.
Verifies excluded files (`debug.log`, `build/output`) are absent and kept files
(`app.rs`, `src/lib.rs`) are present in the destination.

Failure indicates `.remignore` pattern loading or the filtered copy logic is broken.

### `test_parse_add_instruction`
**Requires:** neither root nor rootfs (parser-only)

Parses a Remfile with ADD instructions for both local archive and URL sources.
Verifies both produce correct `Instruction::Add` variants with src/dest fields.

Failure indicates the ADD parser is broken.

### `test_add_local_tar_extraction`
**Requires:** neither root nor rootfs

Creates a temporary `.tar.gz` archive containing two files (one in a subdirectory),
extracts it using the same tar+flate2 pipeline that ADD uses, and verifies both files
are present with correct contents.

Failure indicates the ADD archive extraction logic is broken.

### `test_parse_multi_stage_remfile`
**Requires:** neither root nor rootfs (parser-only)

Parses a two-stage Remfile (`FROM alpine:3.19 AS builder` + `FROM alpine:3.19` +
`COPY --from=builder`). Verifies:
- First `FROM` has alias `"builder"`
- Second `FROM` has no alias
- `COPY --from=builder` has correct `from_stage` field
- Regular `COPY` has `from_stage: None`

Failure indicates multi-stage `FROM ... AS` or `COPY --from=` parsing is broken.

---

## Port Proxy

### `test_port_proxy_localhost_connectivity`
**Requires:** root, alpine-rootfs, `nc` on host

Spawns a bridge+NAT container running a one-shot TCP server on port 80,
forwarded from host port 19190. Connects from **localhost** (127.0.0.1)
to verify the userspace TCP proxy handles localhost traffic that nftables
DNAT in PREROUTING cannot intercept.

Failure indicates the userspace TCP proxy (`start_port_proxies()`) is broken
or not relaying localhost connections to the container.

### `test_port_proxy_cleanup_on_teardown`
**Requires:** root, alpine-rootfs

Spawns a container with a port forward that exits immediately, waits for it,
then verifies the proxy port is no longer bound (a fresh `TcpListener::bind`
on the same port should succeed).

Failure indicates the proxy runtime is not shut down during teardown, leaving
orphaned listener tasks holding the port.

---

### `test_port_proxy_multiple_connections`
**Requires:** root, alpine-rootfs

Spawns a container with port 19192→8080 running a static-response server
(`while true; do echo PONG | nc -l -p 8080; done`). Makes 5 sequential
connections from the host through the async proxy; each connection reads the
response and verifies it contains "PONG".

Failure indicates the tokio accept loop exits prematurely after the first relay
task completes, or that `copy_bidirectional` does not propagate server-side EOF
cleanly (causing subsequent connections to hang or return empty data).

---

## Multi-Network Tests

### `test_network_create_ls_rm`
**Requires:** root

Creates a `NetworkDef` with subnet `10.99.1.0/24`, saves it to disk, loads it
back, and verifies all fields round-trip correctly. Then cleans up and confirms
the config file is removed.

Failure indicates `NetworkDef::save()`/`load()` serialization or path helpers
are broken.

### `test_network_create_overlap_rejected`
**Requires:** root

Creates a network with subnet `10.77.0.0/16`, then checks that a second network
with `10.77.1.0/24` is detected as overlapping via `Ipv4Net::overlaps()`.

Failure indicates subnet overlap detection is broken, which would allow users
to create networks with conflicting address ranges.

### `test_network_name_validation`
**Requires:** none (API-only)

Verifies name length constraints (> 12 chars), invalid character detection
(underscores), leading-hyphen rejection, and CIDR parsing edge cases.

Failure indicates the name validation logic or `Ipv4Net::from_cidr()` parser
has a regression.

### `test_named_network_container`
**Requires:** root, alpine-rootfs

Creates a custom network `testnet2` with subnet `10.98.1.0/24`, spawns a
container on it using `NetworkMode::BridgeNamed("testnet2")`, and checks that
the container's `eth0` has an IP in the `10.98.1.x` range.

Failure indicates the full named-network pipeline is broken: `NetworkDef`
loading, bridge creation, IPAM allocation, or veth configuration.

### `test_default_network_backwards_compat`
**Requires:** root, alpine-rootfs

Spawns a container using `NetworkMode::Bridge` (the legacy enum variant) and
verifies it gets a `172.19.0.x` IP, confirming that the `Bridge` →
`BridgeNamed("remora0")` normalization and default network bootstrap work.

Failure indicates the backwards-compatibility path from `NetworkMode::Bridge`
to the new per-network architecture is broken.

### `test_network_rm_refuses_default`
**Requires:** root

Bootstraps the default network and verifies the config file exists. This tests
that the default `remora0` network is always available and cannot be removed.

Failure indicates `bootstrap_default_network()` is not persisting the config.

### `test_multi_network_dual_interface`
**Requires:** root, alpine-rootfs

Creates two test networks (`mntest1` at `10.99.1.0/24`, `mntest2` at `10.99.2.0/24`),
spawns a container on both using `with_network()` + `with_additional_network()`, and
verifies that eth0 has a `10.99.1.x` IP and eth1 has a `10.99.2.x` IP. Also checks
the `container_ip()` and `container_ip_on()` accessors return the correct IPs.

Failure indicates `attach_network_to_netns()` is not correctly configuring the secondary
interface, or the IPAM allocation is assigning IPs from the wrong subnet.

### `test_multi_network_isolation`
**Requires:** root, alpine-rootfs

Creates two isolated networks (`mniso1`, `mniso2`). Spawns container A on net1 only,
container B on net2 only, and container C on both. Verifies C can ping both A and B,
but a container on net1 alone cannot ping B (on net2).

Failure indicates network isolation is broken — traffic is leaking between bridges
that should be completely separate.

### `test_multi_network_teardown`
**Requires:** root, alpine-rootfs

Spawns a container on two networks, records the netns name and both veth interface
names, then waits for exit. Verifies that the named netns no longer exists at
`/run/netns/` and both veth pairs (primary and secondary) are removed.

Failure indicates `teardown_secondary_network()` or `teardown_network()` is not
cleaning up properly, which would leak network namespaces or veth interfaces.

### `test_multi_network_link_resolution`
**Requires:** root, alpine-rootfs

Creates two networks, starts a "server" container on both, writes its state.json
with `network_ips` map, then starts a "client" on net2 only with `--link server`.
Verifies that `/etc/hosts` contains the server's net2 IP (the shared network),
not its net1 IP.

Failure indicates `resolve_container_ip_on_shared_network()` is not correctly
matching networks, causing links to resolve to IPs on unreachable networks.

---

## DNS Service Discovery

### `test_dns_resolves_container_name`
**Requires:** root, rootfs

Spawns container A (sleep) on a bridge network, registers it with DNS, then
spawns container B on the same network and runs `nslookup`. Verifies the
resolved IP matches A's bridge IP.

Failure means the embedded DNS daemon isn't resolving container names correctly.

### `test_dns_upstream_forward`
**Requires:** root, rootfs

Registers a dummy DNS entry to start the daemon, then resolves `example.com`
via the gateway DNS. Verifies upstream forwarding works.

Failure means the daemon can't forward queries to upstream DNS servers.

### `test_dns_network_isolation`
**Requires:** root, rootfs

Registers "alpha" on net1 and "beta" on net2. Container on net2 tries to
resolve "alpha" — should get NXDOMAIN. Verifies DNS respects network
boundaries.

Failure means DNS is leaking names across networks.

### `test_dns_multi_network`
**Requires:** root, rootfs

Container A on net1+net2, registers on both. Container B on net2 resolves A —
should get A's net2 IP, not net1 IP.

Failure means DNS is returning the wrong IP for multi-network containers.

### `test_dns_daemon_lifecycle`
**Requires:** root + rootfs

Spawns a holder container to create the bridge, then adds a DNS entry — daemon
should start (PID file appears, process alive). Removes the entry — daemon
should auto-exit.

Failure means the daemon lifecycle management is broken.

### `test_dns_dnsmasq_resolves_container_name`
**Requires:** root, rootfs, dnsmasq installed

Same as `test_dns_resolves_container_name` but with `REMORA_DNS_BACKEND=dnsmasq`.
Container B resolves container A by name via dnsmasq. Verifies the backend marker
file says "dnsmasq" and the resolved IP matches A's bridge IP.

Failure means dnsmasq backend isn't resolving container names correctly or the
hosts file generation is broken.

### `test_dns_dnsmasq_upstream_forward`
**Requires:** root, rootfs, dnsmasq installed

Registers a dummy DNS entry to start dnsmasq, then resolves `example.com` via
the gateway. Verifies upstream forwarding works through dnsmasq's `server=`
directives.

Failure means dnsmasq can't forward queries to upstream DNS servers, likely a
config generation issue.

### `test_dns_dnsmasq_lifecycle`
**Requires:** root, rootfs, dnsmasq installed

Adds a DNS entry with dnsmasq backend — daemon should start (PID file appears,
process alive, backend marker says "dnsmasq"). Removes entry and sends SIGTERM.

Failure means dnsmasq lifecycle management (start/stop/PID tracking) is broken.

---

## Drop Cleanup Tests

### `test_child_drop_cleans_up_netns`
**Requires:** root, rootfs

Spawns a container with bridge networking (which creates a named network namespace
under `/run/netns/rem-{pid}-{n}`), records the netns name, then drops the `Child`
without calling `wait()`. Asserts that the netns mount is removed after drop.

Failure means the `Drop` implementation for `Child` is not properly tearing down
network namespaces, which would cause stale `/run/netns/rem-*` mounts to
accumulate over time (especially from test panics or early returns).

---

## Compose Tests

### `test_sexpr_parse_compose_file`
**Type:** No-root

Parses a full compose file example through the S-expression parser (`remora::sexpr::parse`).
Verifies the top-level structure: the root is a list starting with `compose`, containing
the expected number of declarations (networks, volumes, services).

Failure means the S-expression parser cannot handle the compose file syntax (comments,
nested lists, quoted strings, keyword arguments).

### `test_compose_parse_and_validate`
**Type:** No-root

Parses a compose file through the full pipeline (`remora::compose::parse_compose`) which
includes S-expression parsing, AST-to-struct transformation, and cross-reference validation.
Checks that all fields are correctly populated: networks with subnets, volumes, service
names/images/networks/volumes/env/ports/memory, and dependency with `:ready-port`.

Failure means the compose model parser is dropping or misinterpreting fields from the AST.

### `test_compose_topo_sort`
**Type:** No-root

Verifies topological sort of service dependencies: given web -> api -> db, the sort must
produce db before api before web. Uses `remora::compose::topo_sort`.

Failure means services would be started in wrong order, causing dependency failures.

### `test_compose_cycle_detection`
**Type:** No-root

Verifies that a circular dependency (a -> b -> a) is detected and reported as a
`DependencyCycle` error by the compose parser/validator.

Failure means `compose up` would hang or stack overflow on circular dependencies.

### `test_compose_unknown_dependency`
**Type:** No-root

Verifies that a `depends-on` referencing a nonexistent service produces an
`UnknownDependency` error.

Failure means typos in service names would be silently ignored, causing runtime failures.

### `test_compose_up_down_single_service`
**Requires:** root, rootfs

Verifies compose project state directory creation and cleanup. Creates a compose project
directory, asserts it exists, then cleans it up. This exercises the compose path helpers
(`compose_project_dir`, `compose_state_file`).

Failure means the compose state filesystem layout is broken.

### `test_compose_bind_mount_parse_and_validate`
**Requires:** nothing (no root, no rootfs, no image pull)

Verifies that `(bind-mount host container)` and `(bind-mount host container :ro)` parse
correctly through `parse_compose` in a realistic multi-service monitoring-stack compose file.
Asserts that `BindMount` structs carry the right `host_path`, `container_path`, and
`read_only` values, that named volumes and bind mounts coexist on the same service, and that
the topological sort still orders dependents correctly.

Failure means bind-mount entries would be silently dropped or misread, causing containers to
start without their config files and then crash or produce wrong results.

### `test_compose_tmpfs_parse_and_validate`
**Requires:** nothing (no root, no rootfs, no image pull)

Verifies that `(tmpfs "/path")` entries in a compose service spec parse into
`ServiceSpec.tmpfs_mounts` as plain path strings, in declaration order. Asserts
that a service with a single tmpfs entry carries exactly one path, that a service
with two `(tmpfs ...)` entries carries both in order, and that tmpfs mounts coexist
correctly with `depends-on` without disrupting topological sort.

Failure means `(tmpfs ...)` entries would be silently dropped by the parser,
causing containers to launch without the intended in-memory filesystems — for
example, an app writing to a read-only path would fail immediately on startup.


### `test_compose_health_check_parse`
**Requires:** nothing (no root, no rootfs, no image pull)

Verifies that all `depends-on` health-check expression forms parse into the correct
`HealthCheck` enum variants via `parse_compose`. Exercises every syntax form in a single
compose file:

- `:ready (port N)` → `HealthCheck::Port(N)`
- `:ready (http "URL")` → `HealthCheck::Http(url)`
- `:ready (cmd "str")` (single-string, split on whitespace) → `HealthCheck::Cmd(argv)`
- `:ready (and (port N) (cmd "..."))` → `HealthCheck::And([Port, Cmd])`
- `:ready (or (port N) (http "..."))` → `HealthCheck::Or([Port, Http])`
- `:ready-port N` (backward-compat sugar) → `HealthCheck::Port(N)`

Also asserts that a service with no `depends-on` has an empty `depends_on` vec.

Failure means the parser produces wrong `HealthCheck` variants, so `eval_health_check` would
evaluate incorrect conditions and the compose supervisor would start services out of order or
time out waiting for the wrong signal.


### `test_lisp_compose_basic`
**Requires:** nothing (no root, no rootfs, no container spawning)

End-to-end test of the Lisp interpreter path in the compose subsystem. Evaluates a
`.reml`-style string that:
1. Defines a parameterised service factory `(mk-service name img net)` using `define`
2. Builds three `ServiceSpec` values with `map` and a lambda over a quoted list of pairs
3. Registers an `on-ready` hook for the `"db"` service
4. Calls `compose-up` with a `ComposeSpec` that includes one named network and the three services

After evaluation, retrieves the `PendingCompose` via `Interpreter::take_pending()` and asserts:
- Exactly one network named `"backend"` with subnet `"10.90.0.0/24"`
- Exactly three services named `"db"`, `"api"`, `"web"`
- `"db"` service has image `"postgres:16"` and network `"backend"`
- At least one `on-ready` hook registered for `"db"` via `take_hooks()`

Failure indicates a regression in: parser reader macros (quote/quasiquote), `define`/`lambda`,
`map`, the `service`/`network`/`compose`/`compose-up` builtins, list flattening in `compose`,
or the `on-ready` hook registration pipeline.

### `test_lisp_evaluator_tco_and_higher_order`
**Requires:** nothing (no root, no rootfs, no container spawning)

Pure evaluator correctness and TCO stress test:

1. **TCO**: Defines a named-let loop `(sum-to n)` that accumulates a sum with a tail call.
   Invokes `(sum-to 10000)` — 10,000 iterations that would overflow the stack without TCO.
   Asserts the result equals `Value::Int(50005000)`.

2. **map + lambda**: Evaluates `(map (lambda (x) (* x x)) '(1 2 3 4 5))` and asserts the
   result is the Lisp list `(1 4 9 16 25)` represented as `Value::Pair` chains.

Failure means either: (a) TCO is broken and the evaluator stack-overflows on deep tail
recursion; or (b) `map`, `lambda`, arithmetic, or list construction is incorrect.


### `test_lisp_eval_file_web_stack_fixture`
**Requires:** nothing (no root, no rootfs, no container spawning)

Reads `examples/compose/web-stack/compose.reml` from disk via `Interpreter::eval_file()`.
This is the primary test of the file-read path — all previous Lisp tests used inline strings
via `eval_str()`.

Asserts the full parsed and evaluated `ComposeFile` structure:
- Two networks: `"frontend"` (subnet `10.88.1.0/24`) and `"backend"`
- One volume: `"notes-data"`
- Three services: `"redis"`, `"app"`, `"proxy"`
- `redis`: image `web-stack-redis:latest`, network `backend`, memory `64m`, no deps
- `app`: both networks, `depends-on redis` with `HealthCheck::Port(6379)`, `REDIS_HOST` env set
- `proxy`: network `frontend`, `depends-on app` with `HealthCheck::Port(5000)`,
  host port 8080 (default — `$BLOG_PORT` not set in test environment)
- `on-ready` hooks registered for both `"redis"` and `"app"`

Failure means the `eval_file()` path is broken, the `env`-with-fallback pattern
evaluates incorrectly, named `define` variables don't compose correctly, or the
`depends-on` port extension isn't wired through.

### `test_lisp_depends_on_with_port`
**Requires:** nothing (no root, no rootfs, no container spawning)

Unit test for the `(list 'depends-on "svc" N)` → `HealthCheck::Port(N)` extension
added to `apply_service_opt`. Evaluates a service with two `depends-on` options: one
with a port and one without. Asserts:
- `depends-on "db" 5432` produces `Dependency { service: "db", health_check: Some(Port(5432)) }`
- `depends-on "cache"` (no port) produces `Dependency { service: "cache", health_check: None }`

Failure means the `.reml` format cannot express TCP readiness checks on dependencies,
making the Lisp compose path weaker than the static `.rem` format.

### `test_lisp_env_fallback_and_override`
**Requires:** nothing (no root, no rootfs, no container spawning)

Tests the `(env "VAR")` builtin and the standard Lisp fallback pattern used in
`compose.reml` for environment-driven configuration:

```lisp
(let ((p (env "VAR")))
  (if (null? p) default-value (string->number p)))
```

Asserts that with the env var absent the expression returns the default, and with
the var set it returns the parsed value. Tests the full round-trip through `env`,
`null?`, `if`, `string->number`, and the `let` binding.

Failure means operators cannot reliably use environment variables to configure their
`.reml` stacks without modifying the file itself.

### `test_lisp_eval_file_jupyter_fixture`
**Requires:** nothing (no root, no rootfs, no container spawning)

Evaluates the actual `examples/compose/jupyter/compose.reml` file through the full
Lisp interpreter pipeline and asserts the resulting `ComposeFile` matches the
expected structure:

- Exactly 1 network (`jupyter-net`, subnet `10.89.0.0/24`)
- Volume `jupyter-notebooks` declared
- 2 services: `redis` and `jupyterlab`
- `redis`: image `jupyter-redis:latest`, no deps, memory `64m`
- `jupyterlab`: image `jupyter-jupyterlab:latest`, depends-on `redis:6379`
  with `HealthCheck::Port(6379)`, port mapping `8888→8888`, env vars
  `REDIS_HOST=redis` and `REDIS_PORT=6379`
- `on-ready "redis"` hook registered (1 hook in HookMap)
- `JUPYTER_PORT` absent → `string->number` fallback path produces port 8888

Exercises the full end-to-end Lisp evaluation path: `define`, `let`, `env` with
fallback, `on-ready`, `service`, `network`, `volume`, `compose`, `compose-up`, and
the `depends-on` TCP health-check option.

Failure indicates a regression in the Lisp interpreter, the `depends-on` port
parsing, the `on-ready` hook registration, or the `env`/fallback pipeline — any
of which would make the Jupyter stack silently broken before containers are even
started.

### `test_defmacro_basic` (unit test in `src/lisp/mod.rs`)
**Requires:** nothing

Defines a simple `my-swap` macro via `defmacro` and calls it. Asserts that the
two arguments are exchanged in the output list. Verifies the core macro expansion
pipeline: unevaluated args → quasiquote template → `value_to_sexpr` → re-eval.

### `test_defmacro_generates_define` (unit test in `src/lisp/mod.rs`)
**Requires:** nothing

Defines a macro `def-42` that generates a `(define ...)` form. After calling it,
asserts that the named variable is bound in the environment. This is the minimal
proof that a macro can introduce new bindings — the key capability `define-service`
relies on.

### `test_define_service_macro` (unit test in `src/lisp/mod.rs`)
**Requires:** nothing

Calls `define-service` (the stdlib macro loaded at interpreter startup) with
`:image`, `:network`, and `:memory mem` where `mem` is a variable. Asserts that
the bound `ServiceSpec` has the correct name, image, network, and that the `mem`
variable was evaluated at call-site (not captured as a symbol).

Failure means the `define-service` macro itself is broken or `stdlib.lisp` fails
to load at startup, which would make every `.reml` file using `define-service` fail.

### `test_define_service_with_port_variable` (unit test in `src/lisp/mod.rs`)
**Requires:** nothing

Calls `define-service` with `(:port my-port 80)` where `my-port` is a variable
bound to `9090`. Asserts `ports[0].host == 9090` and `ports[0].container == 80`.

Verifies that multi-argument options with variables work correctly through the
macro expansion: the variable is not quoted in the expansion, so it evaluates to
its value when the generated `(list 'port my-port 80)` is executed.

### `test_lisp_eval_file_monitoring_fixture` (unit test in `src/lisp/mod.rs`)
**Requires:** nothing

Evaluates `examples/compose/monitoring/compose.reml` using `include_str!` and
inspects the resulting `ComposeSpec`. Asserts:

- 3 services in order: prometheus, loki, grafana
- Correct image tags for all three
- Single network `monitoring-net` with subnet `10.89.1.0/24`; all services attached
- 2 volumes: `prometheus-data`, `grafana-data`
- Grafana has exactly 2 `depends_on` entries: prometheus with `Port(9090)` and loki with `Port(3100)`
- Grafana env `GF_SECURITY_ADMIN_PASSWORD` equals `"admin"` (the default fallback)
- Port mappings: prometheus→9090, loki→3100, grafana→3000
- 2 `on-ready` hooks registered for "prometheus" and "loki"

Failure indicates a regression in: multiple `depends-on` per service, dotted-pair
`:env` with variable values, `env` built-in fallback, or `on-ready` hook registration.

### `test_lisp_eval_file_rust_builder_fixture` (unit test in `src/lisp/mod.rs`)
**Requires:** nothing

Evaluates `examples/compose/rust-builder/compose.reml` using `include_str!` and
inspects the resulting `ComposeSpec`. Asserts:

- 1 service: `rust-builder` with image `rust-builder:latest`
- 0 networks (single-service stack needs no inter-service communication)
- 2 compose-level volumes: `cargo-registry`, `sccache-cache`
- Service has 2 volume mounts: `cargo-registry → /root/.cargo/registry`, `sccache-cache → /sccache-cache`
- Service command is `["sleep", "infinity"]`
- Service env: `RUSTC_WRAPPER=sccache`, `SCCACHE_DIR=/sccache-cache`, `RUST_EDITION=2021`

Failure indicates a regression in: the new `:volume` Lisp service option,
`:command` multi-value option, dotted-pair `:env` with literal values, or
`env` built-in with null fallback.

### `test_hardening_combination` (integration test in `tests/integration_tests.rs`)
**Requires:** root, alpine-rootfs

Spawns a container using the same four-call hardening block that `compose up`
and the lisp runtime apply (`with_seccomp_default`, `drop_all_capabilities`,
`with_no_new_privileges(true)`, `with_masked_paths_default`), plus
`Namespace::PID | UTS | IPC | MOUNT`.  The container runs
`grep -E '^(Seccomp|CapEff|NoNewPrivs|NSpid):' /proc/self/status` and
`echo HOSTNAME=$(hostname)` via stdout capture.

Asserts:
- `Seccomp: 2` — Docker-default BPF filter is active
- `CapEff: 0000000000000000` — all capabilities dropped
- `NoNewPrivs: 1` — setuid escalation blocked
- NSpid last field = `1` — container is PID 1 in its own PID namespace
- `HOSTNAME=hardening-test` — UTS namespace is isolated

Failure means one of the four hardening primitives regressed at the raw API
level; every regression in this test will be masked from users unless this
ground-truth test exists.

### `test_lisp_container_spawn_hardening` (integration test in `tests/integration_tests.rs`)
**Requires:** root, alpine:latest in image store

Exercises `do_container_start_inner` (the lisp runtime path) via
`Interpreter::new_with_runtime`, starts a `sleep 30` container, then inspects
the spawned process from the host via `/proc/{inner_pid}/status`.

Steps:
1. Create interpreter with `new_with_runtime("test-iso", tmpdir)`
2. Eval `(container-start ...)` with `alpine:latest` and `sleep 30`
3. Extract intermediate PID from the returned `ContainerHandle`
4. Find the inner child (PID 1 in the namespace) via `/proc/{pid}/task/{pid}/children`
5. Read inner child's `/proc/{inner}/status` from the host
6. Compare UTS namespace symlinks (`/proc/{inner}/ns/uts` vs `/proc/self/ns/uts`)

Asserts same four properties as `test_hardening_combination`.  Skips if
`alpine:latest` is not in the image store.

Failure means the lisp `do_container_start_inner` path diverged from the
security defaults applied by compose, or that a future refactor of that
function accidentally removed the hardening block.

### `test_login_logout` (unit test in `src/cli/auth.rs`)
**Requires:** nothing (no root, no network, uses a tempdir for `HOME`)

Exercises `write_docker_config` and `remove_docker_config` (via `parse_docker_config`).

Steps:
1. Write a synthetic `~/.docker/config.json` with base64-encoded credentials
2. Parse with `parse_docker_config` and assert username/password match
3. Call `write_docker_config` to overwrite an entry
4. Call `remove_docker_config` and assert the entry is gone

Failure means the login/logout lifecycle is broken; registry auth would silently
fall back to anonymous even after `remora image login`.

### `registry_auth::test_local_registry_push_pull_roundtrip` (`#[ignore]`)
**Requires:** root, network (Docker Hub for `registry:2`), overlay support

Starts a `registry:2` OCI registry on a random ephemeral port with no
authentication, then exercises the push → pull round-trip over plain HTTP:

1. Pull `registry:2` from Docker Hub (if not already cached)
2. Start `registry:2` with `remora run --detach -p <port>:5000`
3. Pull `alpine` (source image) to ensure it is in the local store
4. Push `alpine` to `127.0.0.1:<port>/library/alpine:latest` with `--insecure`
5. Assert push output contains `"Pushed"`
6. Remove the local re-tagged reference so the subsequent pull is genuine
7. Pull from the local registry with `--insecure`; assert success
8. Assert the image appears in `remora image ls --format json`

Failure indicates that either `--insecure` HTTP negotiation, blob upload, or
manifest PUT is broken; any regression here would prevent push/pull from
working against local or air-gapped registries.

### `registry_auth::test_local_registry_auth_roundtrip` (`#[ignore]`)
**Requires:** root, network (Docker Hub for `registry:2`), overlay support

Starts a `registry:2` container with htpasswd authentication enforced using a
hard-coded bcrypt entry (docker/distribution ≥2.8 only accepts bcrypt; APR1/MD5
is no longer supported). Uses a temporary `HOME` directory
throughout to avoid touching the real `~/.docker/config.json`. Verifies four
properties end-to-end:

1. **Unauthenticated push fails** — `remora image push alpine --dest <registry>/<ref>
   --insecure` exits non-zero when the registry returns 401.
2. **`remora image login` writes credentials** — `--password-stdin` writes a
   base64-encoded entry into `$TMPHOME/.docker/config.json`; the command prints
   `"Login Succeeded"`.
3. **Authenticated push and pull succeed** — after login, push exits 0 and
   prints `"Pushed"`; after removing the local copy, pull exits 0 and
   downloads from the registry.
4. **`remora image logout` removes credentials** — subsequent pull exits
   non-zero (registry returns 401 again).

Failure at step 1 means the registry isn't actually enforcing auth (test
environment problem). Failure at steps 2–3 means credential resolution or
the `~/.docker/config.json` read/write path is broken. Failure at step 4
means `logout` didn't remove the entry and the credential cache is leaking.

### `image_save_load::test_image_save_load_roundtrip` (`#[ignore]`)
**Requires:** root, network (Docker Hub for `alpine`), overlay support

Full save/load roundtrip test:

1. **Pull** `docker.io/library/alpine:latest` from Docker Hub.
2. **Save** it to `/tmp/remora-test-alpine-save.tar` via `remora image save`.
   Verifies the output file exists and contains an `oci-layout` tar entry
   (i.e., it is a valid OCI Image Layout archive).
3. **Remove** the local image with `remora image rm`.
4. **Load** back from the tar via `remora image load -i <tar>`.
   Verifies the command prints `"Loaded"`.
5. **Verify** the image appears in `remora image ls`.
6. **Run** `/bin/true` inside the loaded image to confirm it is fully usable.

Failure at step 2 means `save` failed to find blobs (re-pull needed to
populate the blob cache, or a regression in blob store write paths).
Failure at step 4 means `load` failed to extract layers or write the manifest.
Failure at step 6 means the overlay mount for the loaded image is broken —
layers are present in the store but the image config or layer order is wrong.

### `image_tag::test_image_tag_roundtrip` (`#[ignore]`)
**Requires:** root, network (Docker Hub for `alpine`), overlay support

1. **Pull** `docker.io/library/alpine:latest`.
2. **Tag** it to `my-alpine:tagged` via `remora image tag`.
3. **Verify** both references appear in `remora image ls`.
4. **Run** `/bin/true` in the tagged image — confirms layers and config are
   shared correctly between source and target references.
5. **Remove** the source reference, then **run** the tagged image again —
   verifies that tag creates an independent manifest entry, not an alias.

Failure at step 2 means `tag` failed to copy the manifest or OCI config.
Failure at step 4 means the shared layer store is broken after tagging.
Failure at step 5 means `tag` stored a reference to the source rather than
creating its own manifest, so removing source broke the tag.

---

## Healthcheck Tests (`healthcheck_tests` module)

### `healthcheck_tests::test_parse_healthcheck_instruction_roundtrip`
**Type:** No-root, no-rootfs (parse-only)

Parses three Remfile snippets containing `HEALTHCHECK` instructions and checks
the resulting `Instruction::Healthcheck` fields:

1. **Shell form** — `HEALTHCHECK --interval=5s --retries=2 CMD /bin/check.sh`
   → `cmd == ["/bin/sh", "-c", "/bin/check.sh"]`, `interval_secs == 5`, `retries == 2`.
2. **JSON form** — `HEALTHCHECK CMD ["pg_isready", "-U", "postgres"]`
   → `cmd == ["pg_isready", "-U", "postgres"]`.
3. **NONE form** — `HEALTHCHECK NONE`
   → `cmd` is empty (healthcheck disabled).

Failure indicates the `HEALTHCHECK` Remfile parser (`parse_healthcheck` /
`parse_duration_str` in `src/build.rs`) is broken.

### `healthcheck_tests::test_health_config_oci_json_roundtrip`
**Type:** No-root, no-rootfs (serde-only)

Creates a `HealthConfig` with non-default values, serializes it to JSON, and
deserializes back, asserting all fields survive the round-trip. Also implicitly
verifies that the default-function annotations for `interval_secs`, `timeout_secs`,
and `retries` are correct (they are only invoked when the field is absent from JSON).

Failure indicates a serde regression in `HealthConfig` — either a missing
`#[serde(default = ...)]` annotation or a broken field name.

### `healthcheck_tests::test_healthcheck_exec_true` (`#[ignore]`)
**Requires:** root + rootfs

Starts a detached container running `sleep 30` via the `remora` CLI, then:

1. Runs `remora exec <name> /bin/true` and asserts exit status 0.
2. Runs `remora exec <name> /bin/false` and asserts non-zero exit status.

Failure at step 1 means `remora exec` can't join the container's namespaces or
`/bin/true` is missing from the rootfs. Failure at step 2 means the exit code
is not being propagated correctly from the exec'd process.

### `healthcheck_tests::test_healthcheck_healthy` (`#[ignore]`)
**Requires:** root + rootfs

Starts a detached container, then patches `state.json` to inject a
`health_config` with `cmd = ["/bin/true"]` and `health = "starting"`. Verifies
that the patched JSON parses correctly (both fields present with expected types).
Then writes `health = "healthy"` and re-reads to confirm the state file correctly
stores and returns the `healthy` variant.

This test primarily validates that the `health` and `health_config` fields in
`state.json` are correctly serialized/deserialized. Failure indicates a serde
regression in `ContainerState.health` or `ContainerState.health_config`.

### `healthcheck_tests::test_healthcheck_unhealthy` (`#[ignore]`)
**Requires:** root + rootfs

Starts a detached container, writes `health = "unhealthy"` to `state.json`, and
re-reads to confirm the `unhealthy` variant round-trips correctly through the
state file.

Failure indicates the `HealthStatus::Unhealthy` serde variant is broken
(wrong serialized string or missing enum arm).


---

## Console-socket tests (`console_socket_tests`)

### `console_socket_tests::test_oci_console_socket`
**Requires:** root + rootfs

Creates an OCI bundle with `process.terminal: true` and provides a Unix socket
path via `--console-socket`. The test binds a `UnixListener` on that path before
running `remora create`, then accepts one connection and calls `recvmsg` to
receive the fd sent via `SCM_RIGHTS` ancillary data.

Asserts:
1. `remora create` exits 0.
2. A connection is accepted within 5 seconds (the runtime connected and sent the fd).
3. The received fd is `>= 0` (a valid file descriptor was transmitted).
4. `isatty(received_fd) == 1` — the fd is a TTY, confirming it is the PTY master.

Failure modes:
- If the runtime ignores `--console-socket`, no connection arrives → poll timeout.
- If no fd is sent via `SCM_RIGHTS`, `received_fd == -1`.
- If the wrong fd is sent (not a PTY), `isatty` returns 0.
