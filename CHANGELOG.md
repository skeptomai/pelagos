# Changelog

All notable changes to Remora will be documented in this file.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)

## [Unreleased]

### Added
- Container runtime with Linux namespace isolation (UTS, Mount, IPC, User, Net, Cgroup, PID)
- CLI: `remora run`, `ps`, `stop`, `rm`, `logs`, `exec`
- OCI image support: `remora image pull/ls/rm`, `remora run --image`
- OCI runtime interface: `create/start/state/kill/delete` for containerd/CRI-O
- Networking: loopback, bridge (veth + remora0), NAT, port forwarding, DNS, pasta (rootless)
- Container linking: `--link` with /etc/hosts injection
- Storage: bind mounts, tmpfs, named volumes, overlay filesystem
- Security: seccomp-BPF (Docker default + minimal profiles), capabilities, no-new-privileges, read-only rootfs, masked paths
- Resource limits: cgroups v2 (memory, CPU, PIDs) + rlimits
- Interactive containers with PTY support and SIGWINCH forwarding
- Rootless mode with auto-detection
- `remora exec` to run commands in running containers
- Container exec with namespace discovery and environment inheritance
- CI pipeline with GitHub Actions (lint, unit tests, integration tests)
- Binary releases for x86_64 Linux (musl static builds supported manually)
