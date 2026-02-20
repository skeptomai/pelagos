# Plan: E2E Testing & Hardening

## Context

Remora has 72 integration tests exercising the **library API** and three E2E scripts (`test-dev.sh`, `test-rootless.sh`, `test-exec.sh`), but no comprehensive test of the **CLI binary** in root mode. Many CLI subcommands (`ps`, `stop`, `rm`, `logs`, `rootfs`, `volume`, `image`) have zero E2E coverage. Stress scenarios (concurrent containers, signal propagation, cleanup after crashes) are also untested.

We need two new scripts:
1. **`scripts/test-e2e.sh`** — comprehensive root-mode CLI E2E (~12 sections)
2. **`scripts/test-stress.sh`** — stress and edge-case tests (~7 sections)

Both follow the existing `test-rootless.sh` pattern.

## Files Changed

| File | What |
|------|------|
| `scripts/test-e2e.sh` | **New** — root-mode CLI E2E test script |
| `scripts/test-stress.sh` | **New** — stress and edge-case tests |

---

## `scripts/test-e2e.sh` — Root-Mode CLI E2E

**Requires root** — refuse if `EUID != 0`. Same helpers as `test-rootless.sh` (`pass`/`fail`/`skip`/`check_contains`/`check_not_contains`). Builds first, ensures alpine image is pulled.

### Section 1: Foreground Container Basics

- `remora run alpine /bin/echo hello` → contains "hello"
- `remora run alpine /bin/true` → exit 0
- `remora run alpine /bin/false` → exit non-zero
- `--hostname mybox` → `/bin/hostname` outputs "mybox"
- `--workdir /tmp` → `/bin/pwd` outputs "/tmp"
- `--user 1000:1000` → `/bin/id` contains "uid=1000" and "gid=1000"
- `--env MYVAR=hello42` → `echo $MYVAR` outputs "hello42"
- `--env-file` → reads KEY=VALUE from temp file, skips comments/blanks

### Section 2: Detached Container Lifecycle

Core untested path: `run --detach` → `ps` → `logs` → `stop` → `rm`

- Launch `--name e2e-detach --detach alpine /bin/sleep 300`
- `remora ps` → contains "e2e-detach" and "running"
- `remora ps -a` → also shows it
- Launch `--name e2e-logs --detach ... 'echo log-marker'`; `remora logs e2e-logs` → contains "log-marker"
- `remora stop e2e-detach` → exit 0
- `remora ps` → no longer shows e2e-detach; `ps -a` → shows "exited"
- `remora stop e2e-detach` again → error contains "not running"
- `remora rm e2e-detach` → exit 0; `ps -a` → gone
- `rm` on running container without `--force` → error contains "is running"
- `rm -f` on running container → exit 0
- Name collision: second `run --name e2e-collision` → error "already exists"

### Section 3: Rootfs CLI

- `rootfs import test-rootfs /tmp/dir` → exit 0
- `rootfs ls` → contains "test-rootfs"
- `rootfs rm test-rootfs` → exit 0
- `rootfs ls` → no longer contains "test-rootfs"
- `rootfs rm nonexistent` → error "not found"

### Section 4: Volume CLI

- `volume create e2e-vol` → exit 0
- `volume ls` → contains "e2e-vol"
- Write data via `run --volume e2e-vol:/data`, read back in second container → persists
- `volume rm e2e-vol` → exit 0
- `volume ls` → gone
- `volume rm nonexistent` → error exit

### Section 5: Image CLI

- `image ls` → contains "alpine"
- Pull busybox for test; `image ls` → contains "busybox"
- `image rm busybox` → exit 0; `image ls` → no "busybox"
- `image rm nonexistent` → error exit

### Section 6: Exec CLI

- Start detached container for exec tests
- `exec e2e-exec /bin/echo exec-hello` → "exec-hello"
- `exec e2e-exec /bin/cat /etc/alpine-release` → sees container fs
- `exec --env EVAR=test` → env override works
- `exec --workdir /tmp` → pwd is /tmp
- `exec --user 1000:1000` → id shows uid=1000, gid=1000
- `exec` on stopped container → error "not running"
- `exec` on nonexistent container → error "not found"

### Section 7: Networking CLI

- `--network loopback` → `ip addr show lo` contains "LOOPBACK"
- `--network bridge` → `ip addr` contains "172.19"
- Bridge + NAT → `nft list ruleset` contains "masquerade" (skip if no `nft`)
- Port forward: nc listener in container, curl from host → receives data (skip if no `nc`/`curl`)
- `--dns 1.1.1.1` → `/etc/resolv.conf` contains "1.1.1.1"
- Pasta (skip if unavailable): non-lo interface with inet addr

### Section 8: Filesystem & Mount Flags

- `--read-only` → `touch /file` fails with READONLY
- `--read-only --tmpfs /tmp` → tmpfs writable
- `--bind /host:/container` → read + write through
- `--bind-ro` → write blocked
- `--sysctl net.ipv4.ip_nonlocal_bind=1 --network loopback` → reads back "1"

### Section 9: Security Options

- `--security-opt seccomp=default` → echo works
- `--security-opt seccomp=minimal` → echo works
- `--security-opt no-new-privileges` → echo works
- `--cap-drop ALL` → echo works
- `--cap-drop ALL --cap-add CAP_CHOWN` → chown succeeds
- `--ulimit nofile=16:16` → `ulimit -n` shows 16
- `--memory 128m` → echo works
- `--pids-limit 32` → echo works
- `--cpus 0.5` → echo works

### Section 10: Container Linking

- Start named server container on bridge
- `--link e2e-linkserver` → `/etc/hosts` contains entry
- `--link e2e-linkserver:alias` → `/etc/hosts` contains alias

### Section 11: OCI Lifecycle Commands

- Create OCI bundle from alpine-rootfs with minimal config.json
- `remora create e2e-oci <bundle>` → `state` shows "created"
- `remora start e2e-oci` → runs; `state` eventually shows "stopped"
- `remora delete e2e-oci` → state dir removed

### Section 12: Error Cases

- `run` with no image → non-zero exit
- `--detach --interactive` → "mutually exclusive"
- `stop nonexistent` → "no container named"
- `rm nonexistent` → "no container named"
- `logs` on foreground container → "no log files" (or "was it started with --detach")

---

## `scripts/test-stress.sh` — Stress & Edge Cases

**Requires root.** Uses a cleanup trap to remove all test containers on exit.

### Section 1: Concurrent Bridge Containers (IPAM)

- Launch 5 concurrent `--network bridge` containers
- Read bridge_ip from each container's state.json
- Assert 5 unique 172.19.x.x IPs (no collisions)

### Section 2: NAT Refcount

- Launch 3 NAT containers → refcount >= 3
- Remove one → NAT rule still present
- Remove remaining → refcount back to 0, rule gone

### Section 3: Signal Propagation

- `remora stop` sends SIGTERM → container exits (status "exited")
- `remora rm -f` sends SIGKILL → container dies quickly (< 5s)

### Section 4: Cleanup After Crash / Failure

- Spawn with nonexistent binary → not stuck in "running" state
- Foreground container exit → overlay merged dirs cleaned up
- Bridge container exit → no leaked veth interfaces

### Section 5: Combined Resource Limits

- `--memory 64m --pids-limit 16` together → runs
- `--memory 32m --security-opt seccomp=default --cap-drop ALL` → runs
- `--ulimit nofile=32:32 --pids-limit 20` → both applied

### Section 6: OCI Orphan / Timeout

- `create` with no `start` → can be `delete`d cleanly
- `kill` on a started container → works

### Section 7: Rapid Sequential Containers

- 10 sequential foreground `run` → all produce correct output
- 10 sequential detach+stop+rm cycles → all succeed, no leaked state

---

## Key Error Strings (from source)

These exact strings appear in the CLI source and tests must match:

| Scenario | Error message pattern |
|----------|----------------------|
| Name collision | `"container '{}' already exists and is running"` |
| Stop not running | `"container '{}' is not running (status: {})"` |
| rm running (no force) | `"container '{}' is running; use --force"` |
| No such container | `"no container named '{}'"` |
| Exec not running | `"container '{}' is not running"` |
| Exec not found | `"container '{}' not found"` |
| Detach+interactive | `"--detach and --interactive are mutually exclusive"` |
| Logs no files | `"has no log files (was it started with --detach?)"` |
| Rootfs not found | `"rootfs '{}' not found"` |

## `ps` Output Format

- Status values: `running`, `exited`
- Header: `NAME  STATUS  PID  ROOTFS  COMMAND  STARTED`
- `ps` shows only running; `ps -a` shows all

## Verification

```bash
# Root-mode E2E:
sudo scripts/test-e2e.sh

# Stress tests:
sudo scripts/test-stress.sh

# Expected: section headers, PASS/FAIL/SKIP per check, summary at end
# Exit 0 = all pass, 1 = any fail
```
