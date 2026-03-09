# Pelagos Design Principles

These principles guide every decision in Pelagos — from API design to file
formats to error handling. They are non-negotiable unless explicitly revised
here.

---

## 1. Security First

No feature is worth compromising isolation. Defaults must be secure:
- Seccomp applied, capabilities dropped, rootfs read-only where possible
- The pre-exec hook order is sacred architecture — seccomp is always last
  because setup requires syscalls it would otherwise block
- Never trust container input at system boundaries
- Prefer denial over permissiveness when ambiguous

## 2. Library First, CLI Second

Pelagos is a **library** that happens to ship a CLI. The public Rust API in
`src/lib.rs` and `src/container.rs` is the primary interface. The CLI is a
thin consumer of the library.

- Every capability must be accessible through the library API
- The CLI must never contain logic that belongs in the library
- Builder pattern (`Command::new().with_*().spawn()`) is the canonical API shape
- Types are the documentation — if a method signature is unclear, the design
  is wrong

## 3. Native Over Delegated

Implement directly using kernel interfaces rather than shelling out to
external tools or delegating to plugin frameworks.

- Namespaces via `unshare(2)` / `setns(2)`, not wrapper binaries
- Networking via netlink / ioctl / nftables, not CNI plugins
- Seccomp via BPF compiled in-process, not external profile loaders
- S-expression parser hand-written, not a library dependency

When we must call external programs (e.g. `ip link`, `nft`, `pasta`), it
is a conscious trade-off documented in the code, not the default approach.

## 4. No Daemon

Pelagos has no long-running daemon process. Each `pelagos run` is
self-contained. State lives on the filesystem, not in a server's memory.
Compose supervisors are per-project and ephemeral — they exit when their
services do.

## 5. Rootless as a First-Class Mode

Rootless is not an afterthought or a degraded path. It is auto-detected
(`getuid() != 0`) and works transparently:

- `pelagos image pull`, `pelagos image ls`, `pelagos image rm`, `pelagos build`
  all work **without root** for users in the `pelagos` group (after `sudo
  ./scripts/setup.sh` has initialised `/var/lib/pelagos/` with group-writable
  directories and the setgid bit)
- Container operations (`run`, `exec`, `compose`) require root because they
  create namespaces, configure network interfaces, and mount filesystems
- Overlay uses kernel `userxattr` (5.11+) or `fuse-overlayfs` fallback
- Storage under `~/.local/share/pelagos/` and `$XDG_RUNTIME_DIR/pelagos/`
  when `/var/lib/pelagos/` has not been initialised
- Bridge networking is cleanly rejected with a message pointing to pasta
- Cgroups are skipped gracefully, not failed fatally

**`pelagos image pull` does NOT require sudo.** Saying or implying otherwise
is a documentation bug. If a non-root pull fails with "Permission denied",
the cause is either: (a) the user's shell session predates their `pelagos`
group membership and needs `newgrp pelagos` or a new login, or (b) existing
image/layer directories were created by root before `setup.sh` was run —
fixed by running `sudo ./scripts/setup.sh` again (it repairs permissions
idempotently).

## 6. Compose Files Are Valid Lisp

The `.rem` compose format uses S-expressions. **This is not an
approximation of Lisp syntax — it is actual Lisp syntax.** Any valid
`.rem` file must be parseable by a standards-compliant Scheme or Common
Lisp reader.

Consequences:

- No inventing new syntax sugar that a Lisp reader would reject
- `:keyword` arguments follow Lisp keyword conventions
- Strings use `"double quotes"` with `\"` and `\\` escapes only
- Comments use `;` (semicolon to end of line)
- The grammar is exactly: `sexpr = atom | '(' sexpr* ')'`
- Future extensions (conditionals, variables, includes) must be
  expressible as valid S-expressions: `(if ...)`, `(let ...)`,
  `(include "base.rem")` — never as special syntax outside the grammar
- The parser in `src/sexpr.rs` must accept anything a Lisp reader accepts
  and reject anything a Lisp reader rejects (within the subset we use)

The long-term goal is that a Scheme interpreter can evaluate compose files
directly, with `compose`, `service`, `network`, etc. defined as macros or
functions. The current parser is a stepping stone, not a destination.

## 7. Minimal Dependencies

Every dependency is a liability — it can break, introduce CVEs, or
bloat compile times. Prefer hand-written code for small, well-bounded
problems:

- S-expression parser: ~150 lines, zero dependencies
- DNS daemon: `std::net::UdpSocket`, no async runtime
- TCP readiness probe: `std::net::TcpStream::connect_timeout`
- Date formatting: manual epoch arithmetic, no `chrono`

Add a dependency only when the alternative is reimplementing something
complex and error-prone (e.g. `oci-client` for registry protocol,
`seccompiler` for BPF compilation, `nix` for safe syscall wrappers).

## 7a. Library Dependencies vs. Subsystem Dependencies

These are not the same kind of dependency and must not be treated alike.

**Library dependencies** are subordinate. They do what you tell them at
the call sites you choose, under the contract you define. If they
misbehave, you replace them. You are in control.

**Subsystem dependencies** invert that relationship. A subsystem has its
own opinions about lifecycle, socket paths, configuration format, and
update cadence. You build *around* its model, not on top of it. When it
changes, your product breaks on someone else's schedule. This is closer
to "we are a plugin for X" than "we use X."

The practical consequence: pelagos should have **no subsystem-sized
external dependencies**. This is what separates a product from an
integration. AWS Finch is an integration — Lima + containerd + nerdctl
assembled by Amazon. That is fine for Finch's goals. Pelagos has a
coherent design philosophy and a specific UX; delegating a major
subsystem permanently cedes that UX to a project with different goals
and different maintainers.

This principle is what drives the macOS design toward owning the VM
orchestration layer (AVF bindings + virtiofsd) rather than building on
Lima. Lima is an excellent project; that is not the point. The point is
that pelagos's failure modes and release cadence should belong to
pelagos.

## 8. Test Everything

A feature is not done until it has integration tests in the same commit.
Not in a follow-up. Not "when we have time." In the same commit.

- Parser/model features: unit tests in the module, integration tests
  in `tests/integration_tests.rs`
- Runtime features: root-requiring tests that spawn real containers
- Every test documented in `docs/INTEGRATION_TESTS.md`

## 9. Incremental Value

Each change must be usable on its own. No half-implemented features
behind flags. No "this will work once we also do X." If a phase is too
large to land atomically, break it into sub-phases that each deliver
working functionality.

## 10. Clean Boundaries

Modules have clear responsibilities and minimal coupling:

- `src/sexpr.rs` — parsing only, no I/O, no compose knowledge
- `src/compose.rs` — AST-to-model, validation, topo-sort; no I/O, no
  container spawning
- `src/cli/compose.rs` — orchestration, I/O, process management; calls
  the library
- `src/container.rs` — the kernel interface; knows nothing about CLI,
  images, or compose
- `src/network.rs` — networking primitives; no knowledge of how they are
  invoked

A module at layer N may call layer N-1 but never layer N+1. The CLI
calls the library. The library calls the kernel. Never the reverse.

## 11. Explicit Over Clever

- Prefer verbose match arms over trait magic
- Prefer duplicated-but-clear code over premature abstraction
- Error messages must say what went wrong AND what to do about it
- Log through `log::*` crate (respects `RUST_LOG`), never `eprintln!`
  (reserved for user-facing CLI errors only)

## 12. No Time Estimates

Never put time estimates in documentation, plans, or commit messages.
Use effort descriptors: "Quick", "Moderate Effort", "Significant Work."
Time estimates are always wrong and create false expectations.
