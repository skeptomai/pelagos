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
