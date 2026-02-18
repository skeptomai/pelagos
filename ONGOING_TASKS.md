# Ongoing Tasks

## Current: OCI Image Layers — COMPLETE ✅

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

## Planned Feature 2: `remora exec` — Attach to Running Container

**Priority:** Medium — quality-of-life for debugging running containers
**Effort:** Moderate

Run a new process inside an already-running container's namespaces, similar to
`docker exec`. See git history for full design notes.

---

## Previous Tasks — COMPLETE

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
