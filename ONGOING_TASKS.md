# Ongoing Tasks

## Current State (v0.10.0, 2026-02-22)

No active tasks. All features shipped and released.

### Recent Session (2026-02-22)

- Expanded `docs/USER_GUIDE.md` build section: ARG, ADD, multi-stage, .remignore, new CLI flags
- Created `examples/multi-stage/` (Rust+musl HTTP server demonstrating multi-stage builds)
- Tagged and released **v0.10.0**

### What's Fully Implemented

- Core isolation (6/7 namespaces, chroot/pivot_root, /proc /sys /dev)
- Security (seccomp, capabilities, no-new-privs, read-only rootfs, masked paths, rlimits)
- Interactive containers (PTY, SIGWINCH, session isolation)
- Cgroups v2 (memory, CPU, PIDs, stats, auto-cleanup)
- Filesystem (bind mounts, tmpfs, named volumes, overlay)
- OCI images (pull, run, ls, rm, multi-layer overlay, whiteouts)
- Image build (Remfile: FROM/RUN/COPY/ADD/CMD/ENTRYPOINT/ENV/ARG/WORKDIR/EXPOSE/LABEL/USER, multi-stage, .remignore, build cache)
- Networking (loopback, bridge, named networks, multi-network, NAT, port forwarding, DNS, DNS-SD, pasta)
- Container exec (namespace join, env inheritance, PTY, user/workdir)
- Compose (S-expression format, dependency order, TCP readiness, supervisor, logs)
- OCI runtime compat (create/start/state/kill/delete)

### 133 unit tests, 84 integration tests (root-required)

## Next Task

(Awaiting user direction.)
