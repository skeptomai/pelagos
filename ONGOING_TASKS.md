# Ongoing Tasks

## Current: GitHub Actions CI + Release Workflow + Docs — COMPLETE ✅

Added GitHub Actions CI and release workflows, CHANGELOG, install script,
and documentation updates. Tagged and released v0.1.0.

**Files created:**
- `.github/workflows/ci.yml` — CI on push/PR: lint (fmt + clippy), unit tests, integration tests
- `.github/workflows/release.yml` — glibc release binary on `v*` tag push, SHA256 checksum
- `CHANGELOG.md` — Keep a Changelog format, all features under `[Unreleased]`
- `scripts/install.sh` — Build release and install to `/usr/local/bin` (or custom path)

**Files modified:**
- `README.md` — CI badge, user guide link, installation section, CHANGELOG in docs table,
  rootless section updated to reflect pasta (Phase 2) completion
- `docs/USER_GUIDE.md` — Replaced bare `cargo build` with proper install instructions
- `src/container.rs` — Portable `RlimitResource` type alias (glibc/musl), c-string literals,
  ioctl request casts for musl compatibility
- `src/network.rs` — `.truncate(false)` on `OpenOptions`, ioctl casts
- `src/oci.rs` — Redundant closure cleanup, c-string literals
- `src/pty.rs` — ioctl request casts
- `src/main.rs` — Box large enum variant
- All source files — `cargo fmt` applied codebase-wide

**CI details:**
- Three parallel jobs: lint, unit-tests, integration-tests (all parallel)
- Integration tests install nftables, iproute2, passt; build rootfs via tarball script
- `--test-threads=1` for integration tests (shared network state)
- `sudo -E env "PATH=$PATH"` preserves cargo on runner's PATH

**Release details:**
- Builds against glibc (same target as CI — test what you ship)
- musl static builds supported manually and documented in README
- `softprops/action-gh-release@v2` creates GitHub Release with binary + SHA256
- v0.1.0 tagged and released successfully

**Issues fixed during CI bringup:**
- `cargo fmt` — never been run; applied codebase-wide (23 files)
- `cargo clippy -D warnings` — c-string literals, redundant closures, `OpenOptions::truncate`,
  `io::Error::other`, unused imports, large enum variant
- `reset-test-env.sh` — `grep | while` pipeline failed under `set -o pipefail` when no
  stale overlays existed; added `|| true`
- `sudo -E cargo` — runner's PATH didn't include `~/.cargo/bin` for root
- musl rlimit type — `libc::__rlimit_resource_t` is glibc-only; added cfg-gated alias
- musl ioctl type — `ioctl(fd, request)` takes `c_ulong` on glibc, `c_int` on musl;
  use `as _` for portable casts

---

## Previous: Rewrite USER_GUIDE.md — COMPLETE ✅

Rewrote `docs/USER_GUIDE.md` to be CLI-first (like a podman/nerdctl quickstart).
Added sections for OCI images, `remora exec`, networking, storage, security,
resource limits, rootless mode, and full `run` flag reference. Moved Rust API
to a secondary section. Updated `README.md` to link to the guide and fix
outdated `--rootfs`/`--exe` CLI syntax.

---

## Previous: OCI Image Layers — COMPLETE ✅

### Goal

Enable `remora image pull alpine` → `remora run --image alpine /bin/sh`.
Replace the manual rootfs download workflow with native OCI registry pulls.

### Technology

`oci-client` v0.16.0 (Apache-2.0) — native Rust OCI registry client. No
external tools (skopeo, etc.). Brings tokio + reqwest as transitive deps.

### New Dependencies

```toml
# Cargo.toml changes
edition = "2021"                                   # upgrade from 2018
oci-client = "0.16"                                # OCI registry client
tokio = { version = "1", features = ["rt"] }       # async runtime (pulls only)
flate2 = "1"                                       # gzip decompression
tar = "0.4"                                        # tar extraction
tempfile = "3"                                     # move from dev-deps to deps
```

### Storage Layout

```
/var/lib/remora/images/<name>_<tag>/
  manifest.json              # reference, digest, ordered layer digests, config

/var/lib/remora/layers/<sha256-hex>/
  bin/ etc/ usr/ ...         # extracted layer (content-addressable, shared)
```

### New File: `src/image.rs` (~300 lines) — Image Store Library

Pure sync module. No tokio, no networking. Filesystem operations only.

```rust
pub const IMAGES_DIR: &str = "/var/lib/remora/images";
pub const LAYERS_DIR: &str = "/var/lib/remora/layers";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageConfig {
    pub env: Vec<String>,           // ["PATH=/usr/bin", "HOME=/root"]
    pub cmd: Vec<String>,           // default command
    pub entrypoint: Vec<String>,    // entrypoint prefix
    pub working_dir: String,        // e.g. "/app"
    pub user: String,               // e.g. "1000" or "nobody"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageManifest {
    pub reference: String,          // "alpine:latest"
    pub digest: String,             // "sha256:abc123..."
    pub layers: Vec<String>,        // ordered layer digests, bottom to top
    pub config: ImageConfig,
}

pub fn reference_to_dirname(reference: &str) -> String  // "alpine:latest" → "alpine_latest"
pub fn image_dir(reference: &str) -> PathBuf
pub fn layer_dir(digest: &str) -> PathBuf               // strips "sha256:" prefix
pub fn layer_exists(digest: &str) -> bool
pub fn extract_layer(digest: &str, tar_gz_path: &Path) -> io::Result<PathBuf>
pub fn save_image(manifest: &ImageManifest) -> io::Result<()>
pub fn load_image(reference: &str) -> io::Result<ImageManifest>
pub fn list_images() -> Vec<ImageManifest>
pub fn remove_image(reference: &str) -> io::Result<()>
pub fn layer_dirs(manifest: &ImageManifest) -> Vec<PathBuf>  // top-first for overlayfs
```

`extract_layer()`:
- Uses `flate2::read::GzDecoder` + `tar::Archive::unpack()`
- Handles OCI whiteout files during extraction:
  - `.wh.NAME` → create overlayfs char device (0,0) named `NAME` via `libc::mknod`
  - `.wh..wh..opq` → set `trusted.overlay.opaque` xattr on parent dir

Add `pub mod image;` to `src/lib.rs`.

### New File: `src/cli/image.rs` (~200 lines) — CLI + Registry Pull

Tokio runtime constructed locally: `Runtime::new().unwrap().block_on(...)`.
Only used for the pull command.

```rust
pub fn cmd_image_pull(reference: &str) -> Result<...>
pub fn cmd_image_ls() -> Result<...>
pub fn cmd_image_rm(reference: &str) -> Result<...>
```

`cmd_image_pull` flow:
1. Parse reference → `oci_client::Reference`
2. Create `oci_client::Client` with `RegistryAuth::Anonymous`
3. `client.pull_manifest_and_config()` → manifest + config JSON
4. For each layer: skip if `layer_exists()`, else `client.pull_blob()` to tempfile → `extract_layer()`
5. Parse config JSON → `ImageConfig` (Env, Cmd, Entrypoint, WorkingDir)
6. `save_image()` with metadata
7. Print summary: reference, layer count, cached vs downloaded

Add `pub mod image;` to `src/cli/mod.rs`.

### Modify: `src/container.rs` — Multi-Layer Overlay

**Extend `OverlayConfig`** (line ~344):
```rust
pub struct OverlayConfig {
    pub upper_dir: PathBuf,
    pub work_dir: PathBuf,
    pub lower_dirs: Vec<PathBuf>,   // NEW: when non-empty, used instead of chroot as lowerdir
}
```

**Update `with_overlay()`** (line ~1042): set `lower_dirs: Vec::new()` for backward compat.

**Add `with_image_layers(layer_dirs: Vec<PathBuf>)`** builder method:
- Sets `chroot_dir` to bottom layer (last in the vec)
- Sets `overlay.lower_dirs` to all layers (top-first, as overlayfs expects)
- Sets `overlay.upper_dir` / `work_dir` to empty `PathBuf` (placeholder — auto-created by spawn)
- Caller should NOT also call `with_chroot()` or `with_overlay()`

**Update overlay mount logic** in both `spawn()` (~line 1472-1812) and
`spawn_interactive()` (~line 2460-2768):
- Build `lowerdir=` string: if `lower_dirs` non-empty, join with `:`;
  else use chroot dir as single lower (existing behavior)
- Auto-create upper/work when empty (image-layer mode):
  `/run/remora/overlay-{pid}-{n}/upper/` and `/work/`

### Modify: `src/main.rs` — Image Subcommand

Add after Volume (~line 76):
```rust
Image {
    #[clap(subcommand)]
    cmd: ImageCmd,
}
```

Add `ImageCmd` enum with `Pull { reference }`, `Ls`, `Rm { reference }`.
Add dispatch in `main()`.

### Modify: `src/cli/run.rs` — `--image` Flag

**Add to `RunArgs`** (before `args` at line ~128):
```rust
#[clap(long)]
pub image: Option<String>,
```

**Change `args` field**: Remove `required = true` (not required with `--image`).

**Update `cmd_run()`** (line ~133) — branch on `args.image`:
- `--image`: load manifest via `image::load_image()`, resolve layer dirs,
  determine command (CLI args override image Entrypoint+Cmd, fall back to
  `/bin/sh`), call `build_command_for_image()`
- No `--image`: existing rootfs flow (error if `args` empty)

**Add `build_command_for_image()`**: Uses `with_image_layers()` instead of
`with_chroot()`. Applies image config defaults (Env, WorkingDir) before
CLI overrides.

**Refactor**: Extract common CLI option logic (network, volumes, bind mounts,
tmpfs, env, caps, security, sysctl, masked paths) from `build_command()` into
`apply_cli_options(cmd, args, ...)` shared by both paths.

### Implementation Order

1. `Cargo.toml` — bump edition, add deps, verify build
2. `src/image.rs` — image store + layer extraction + unit tests
3. `src/container.rs` — extend OverlayConfig, add `with_image_layers`, update mount logic
4. `src/cli/image.rs` — registry pull + CLI commands
5. `src/main.rs` — Image subcommand + dispatch
6. `src/cli/run.rs` — `--image` flag, `build_command_for_image`, refactor shared logic
7. Integration tests
8. Docs (INTEGRATION_TESTS.md, ONGOING_TASKS.md, CLAUDE.md)

### Tests

**Unit tests in `src/image.rs`:**
- `test_reference_to_dirname` — name sanitization
- `test_layer_dir_strips_prefix` — "sha256:abc" → "abc"
- `test_manifest_roundtrip` — save + load

**Integration tests (new `images` module in `tests/integration_tests.rs`):**
- `test_layer_extraction` — create synthetic tar.gz, extract, verify files
- `test_multi_layer_overlay_merge` — two temp layers, container sees both files
- `test_multi_layer_overlay_shadow` — top layer file shadows bottom layer
- `test_image_layers_cleanup` — ephemeral upper/work removed after wait()

Document all new tests in `docs/INTEGRATION_TESTS.md`.

**Manual verification (requires internet, user runs):**
```bash
sudo -E cargo run -- image pull alpine
sudo -E cargo run -- image ls
sudo -E cargo run -- run --image alpine /bin/sh -c "cat /etc/alpine-release"
sudo -E cargo run -- image rm alpine
```

### Notes / Risks

- **Edition 2021**: Safe upgrade from 2018, backward compatible
- **Binary size**: oci-client brings tokio+reqwest+rustls — significant increase.
  Could gate behind cargo feature flag later if needed
- **Auth**: Anonymous-only for v1. Docker Hub public images work. Credential
  helpers are a future enhancement
- **Platform**: oci-client handles multi-arch manifest selection automatically
- **Whiteout files**: Convert OCI `.wh.*` to overlayfs char device (0,0) via `libc::mknod`
- **tempfile**: Move from dev-deps to deps (needed for layer download temp files)

---

## Current: `remora exec` — Run a Command in a Running Container — COMPLETE ✅

### Goal

Enable `remora exec <name> <command>` to run a new process inside a running
container's namespaces. Analogous to `docker exec`. Supports interactive mode
(`-i`) with PTY.

### How It Works

1. Look up container by name → get PID from `/run/remora/containers/{name}/state.json`
2. Discover which namespaces the container has by comparing `/proc/{pid}/ns/{type}`
   inodes against `/proc/1/ns/{type}` (same approach as `nsenter`)
3. Read the container's environment from `/proc/{pid}/environ`
4. Build a `Command` with:
   - `with_chroot("/proc/{pid}/root")` — enters the container's root filesystem
   - `with_namespace_join("/proc/{pid}/ns/X", Namespace::X)` for each discovered ns
   - No `with_proc_mount()`, no `with_namespaces()`, no overlay/cgroup/network config
5. Spawn (interactive with PTY or foreground with inherited stdio)

**No changes to `container.rs` needed.** The existing pre_exec order (chroot at
step 4, setns at step 6) works for exec: `chroot("/proc/{pid}/root")` resolves the
container's root via procfs while still in the host mount namespace, then
`setns(CLONE_NEWNS)` switches the mount table.

**No resource teardown.** The exec'd process is ephemeral — the `Child` has no
cgroup, network, or overlay state, so `wait()` won't clean up the container.

### New File: `src/cli/exec.rs` (~120 lines)

```rust
#[derive(Debug, clap::Args)]
pub struct ExecArgs {
    pub name: String,                              // container name
    #[clap(long, short = 'i')]
    pub interactive: bool,                         // allocate PTY
    #[clap(long = "env", short = 'e')]
    pub env: Vec<String>,                          // KEY=VALUE overrides
    #[clap(long = "workdir", short = 'w')]
    pub workdir: Option<String>,                   // cwd inside container
    #[clap(long = "user", short = 'u')]
    pub user: Option<String>,                      // UID[:GID]
    #[clap(multiple_values = true, required = true, allow_hyphen_values = true)]
    pub args: Vec<String>,                         // command + args
}

pub fn cmd_exec(args: ExecArgs) -> Result<(), Box<dyn std::error::Error>>
fn discover_namespaces(pid: i32) -> Result<Vec<(PathBuf, Namespace)>, ...>
fn read_proc_environ(pid: i32) -> Vec<(String, String)>
```

`discover_namespaces()`: compares inodes of `/proc/{pid}/ns/{type}` vs
`/proc/1/ns/{type}` for: mnt, uts, ipc, net, pid, user, cgroup. Returns
only those that differ.

`read_proc_environ()`: reads `/proc/{pid}/environ` (NUL-separated KEY=VALUE).

`cmd_exec()` flow:
1. `read_state(name)` + `check_liveness(pid)` — validate container is running
2. `discover_namespaces(pid)` — find which namespaces to join
3. `read_proc_environ(pid)` — get container's environment as base
4. Build `Command::new(exe).args(rest)`:
   - `.with_chroot(format!("/proc/{}/root", pid))`
   - `.with_namespace_join(path, ns)` for each discovered namespace
   - `.env(k, v)` for container env, then CLI `-e` overrides
   - `.with_cwd(workdir)` if specified
   - `.with_uid(uid)` / `.with_gid(gid)` if specified
5. If `--interactive`: `cmd.spawn_interactive()?.run()` → exit with status
6. Else: `cmd.stdin/stdout/stderr(Inherit).spawn()?.wait()` → exit with status

### Modify: `src/cli/mod.rs`

Add `pub mod exec;`

### Modify: `src/main.rs`

Add to `CliCommand` enum (after `Run`):
```rust
/// Run a command in a running container
Exec(cli::exec::ExecArgs),
```
Add dispatch: `CliCommand::Exec(args) => cli::exec::cmd_exec(args),`

### Implementation Order

1. `src/cli/exec.rs` — new file
2. `src/cli/mod.rs` — add module
3. `src/main.rs` — add subcommand + dispatch
4. Integration tests
5. Docs (INTEGRATION_TESTS.md, ONGOING_TASKS.md, CLAUDE.md)

### Integration Tests (new `exec` module)

**`test_exec_basic`** — root + rootfs. Start `sleep 30` container, exec
`/bin/cat /etc/hostname` inside it. Verify output and exit code 0.

**`test_exec_sees_container_filesystem`** — root + rootfs. Start container that
creates `/tmp/exec-marker`, then exec `/bin/cat /tmp/exec-marker`. Confirms
the exec'd process sees the container's mount namespace.

**`test_exec_environment`** — root + rootfs. Start container with env `FOO=bar`,
exec `/bin/sh -c 'echo $FOO'`. Verify output is "bar". Also test `-e` override.

**`test_exec_nonrunning_container_fails`** — root. Try to exec into a stopped
container. Verify error message.

Document all new tests in `docs/INTEGRATION_TESTS.md`.

### Manual Verification (user runs)

```bash
# Terminal 1: start a long-running container
sudo -E cargo run -- run --name test-exec --detach alpine-rootfs /bin/sleep 300

# Terminal 2: exec into it
sudo -E cargo run -- exec test-exec /bin/sh -c "echo hello from exec"
sudo -E cargo run -- exec -i test-exec /bin/sh

# Cleanup
sudo -E cargo run -- stop test-exec
sudo -E cargo run -- rm test-exec
```

### Notes / Risks

- **PID namespace**: `setns(CLONE_NEWPID)` affects children only — the exec'd
  process (child of fork) will be in the container's PID namespace. Correct.
- **Race condition**: If container exits between liveness check and spawn,
  `/proc/{pid}/ns/*` disappears and spawn fails. Acceptable (same as `docker exec`).
- **No /proc remount**: Container already has `/proc` mounted. Must NOT set
  `with_proc_mount()`.
- **No new namespaces**: We join existing namespaces, not create new ones.

---

## Previous Tasks — COMPLETE

- `5477cd0` — OCI image layers: `remora image pull` + `remora run --image`
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
