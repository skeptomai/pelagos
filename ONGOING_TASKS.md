# Ongoing Tasks

## Current Task: OCI Compliance

### Context

OCI (Open Container Initiative) compliance allows Remora to interoperate with standard
container tooling: Kubernetes, containerd, BuildKit, and anything that speaks the
OCI Runtime Specification.

An OCI runtime must implement four lifecycle commands (`create`, `start`, `state`,
`kill`, `delete`) against a **bundle** — a directory containing:
- `config.json` — the OCI runtime config (namespaces, mounts, process, hooks, etc.)
- `rootfs/` — the container root filesystem

This is the next planned task, but no implementation has started yet.
When ready to implement, expand this section with the full plan before proceeding.

---

## Planned (after OCI)

1. **Rootless Mode** — discuss slirp4netns vs pasta before implementing

---

## Completed Tasks

### DNS Fix ✅

Replaced the incorrect `write_dns_config()` approach (which permanently mutated the
shared rootfs) with a per-container temp file + bind mount:

- Parent writes nameservers to `/run/remora/dns-{pid}-{n}/resolv.conf` before fork
- `pre_exec` bind-mounts that file over `effective_root/etc/resolv.conf` inside the
  container's private mount namespace — the shared rootfs is never touched
- Temp dir removed in `wait()` / `wait_with_output()` via `remove_dir_all`
- Requires `Namespace::MOUNT` (so the bind mount stays in the container's namespace)
  and `with_chroot`; returns an error if either is missing

### Overlay Filesystem ✅

Implemented `with_overlay(upper_dir, work_dir)` — copy-on-write layered rootfs.

- Lower layer = `chroot_dir` (shared, never modified)
- Upper layer = user-supplied writable dir (writes land here)
- Work dir = required by overlayfs kernel driver (same fs as upper)
- Merged dir = auto-created at `/run/remora/overlay-{pid}-{n}/merged/`, cleaned up in `wait()`

Integration tests: `test_overlay_writes_to_upper`, `test_overlay_lower_unchanged`,
`test_overlay_merged_cleanup` (49 total integration tests).
