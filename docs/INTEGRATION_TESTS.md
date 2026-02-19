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
