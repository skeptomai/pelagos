# Ongoing Tasks

## Current Task: Embedded DNS Server for Container Name Resolution

### Context

Remora containers currently use `--link name:alias` to resolve other containers
by name. The goal is automatic DNS-based service discovery: any container on a
bridge network can resolve other containers on the same network by name, with no
`--link` needed.

**Architecture model: Podman's aardvark-dns.** A custom Rust micro-daemon binary
(`remora-dns`) that:
- Is forked by the first `remora run` on a network
- Listens on each bridge gateway IP (e.g., `172.19.0.1:53`)
- Reads per-network config files for container name → IP mappings
- Reloads on SIGHUP when containers start/stop
- Auto-exits when all config files are empty (last container left)

### Implementation Steps

#### Step 1: `src/bin/remora-dns.rs` — DNS daemon binary (~400 lines)

New binary. Minimal DNS server compiled as a separate binary in the same crate.

- DNS packet parsing: only A-record queries
- Config file format: one file per network in `/run/remora/dns/`
  ```
  172.19.0.1 8.8.8.8,1.1.1.1
  redis 172.19.0.2
  app 172.19.0.3
  ```
- Server loop: bind UDP sockets to gateway IPs, poll with 100ms timeout
- SIGHUP handler: reload config files, rebind/unbind sockets
- Auto-exit: when all config files are empty/gone
- Upstream forwarding: relay unknown queries to upstream DNS servers
- PID file: `<config-dir>/pid`
- Unit tests: parse/build DNS packets, config parsing

#### Step 2: `src/dns.rs` — DNS daemon management library

- `ensure_dns_daemon()` — start daemon if not running (double-fork + exec)
- `dns_add_entry()` — add container to network config file, SIGHUP daemon
- `dns_remove_entry()` — remove container, SIGHUP daemon
- `daemon_pid()` / `signal_reload()` — PID file management
- Config file locking with flock

#### Step 3: `src/paths.rs` — DNS paths

- `dns_config_dir()` → `<runtime>/dns/`
- `dns_pid_file()` → `<runtime>/dns/pid`
- `dns_network_file(name)` → `<runtime>/dns/<network_name>`

#### Step 4: `src/container.rs` — Auto-inject gateway as nameserver

In `spawn()` and `spawn_interactive()`, auto-inject bridge gateway IP(s) as
primary nameservers in resolv.conf when bridge networking is active. User
`--dns` servers appended as fallback.

#### Step 5: `src/cli/run.rs` — Register/deregister containers with DNS

After spawn: call `dns_add_entry()` for primary + secondary networks.
On container exit: call `dns_remove_entry()` for all networks.

#### Step 6: Module registration + Cargo.toml

- `src/lib.rs`: add `pub mod dns;`
- `Cargo.toml`: add `[[bin]]` for `remora-dns`

#### Step 7: Integration tests (5 new tests)

| Test | Asserts |
|------|---------|
| `test_dns_resolves_container_name` | Container B resolves A by name |
| `test_dns_upstream_forward` | Container resolves `example.com` |
| `test_dns_network_isolation` | A on net1, B on net2 → NXDOMAIN |
| `test_dns_multi_network` | A on net1+net2, B on net2 → resolves A's net2 IP |
| `test_dns_daemon_lifecycle` | Daemon starts/stops with containers |

#### Step 8: Documentation

- `docs/INTEGRATION_TESTS.md` — document 5 new tests
- `CLAUDE.md` — update networking section

### Files Changed

| File | Change |
|------|--------|
| `src/bin/remora-dns.rs` | **NEW** — DNS daemon binary |
| `src/dns.rs` | **NEW** — daemon management library |
| `src/lib.rs` | Add `pub mod dns;` |
| `src/paths.rs` | Add DNS path functions |
| `src/container.rs` | Auto-inject gateway nameserver |
| `src/cli/run.rs` | Register/deregister DNS entries |
| `Cargo.toml` | Add `[[bin]]` |
| `tests/integration_tests.rs` | 5 new tests |
| `docs/INTEGRATION_TESTS.md` | Document new tests |
| `CLAUDE.md` | Update networking docs |

---

## Next Task: `remora compose`

Declarative multi-container stacks from a YAML file — replacing manual shell
scripts like `examples/web-stack/run.sh`.

---

## Previously Completed

### Multi-Network Containers (v0.4.0)
- Containers join multiple bridge networks simultaneously
- `attach_network_to_netns()` for secondary interfaces (eth1, eth2, ...)
- Smart `--link` resolution across shared networks
- 4 new integration tests

### Full feature list in CLAUDE.md
