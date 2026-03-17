# Ongoing Tasks

All work is tracked in GitHub Issues. This file is a brief index.

## Session completed: 2026-03-17 (SHA afb1b7f, v0.55.0)

### Issues closed this session

| # | Title | Fixed in |
|---|-------|---------|
| #117 | feat: -a/--attach + --sig-proxy for detached output streaming | v0.55.0 |
| #116 | feat: SpawnConfig.tmpfs; multi-name `pelagos start n1 n2` | v0.54.0 |
| #115 | fix(exec): load image-config ENV for exec'd processes | v0.53.0 |
| #114 | fix(run): preserve image-config PATH (apply_cli_options) | v0.52.0 |
| #112 | fix: CA cert EBUSY in build containers (overlay upper dir pre-seed) | v0.51.0 |
| #109 | fix: pelagos run finds locally-built images (FROM local-tag) | v0.34.0 (stale) |

### Key implementation details

**#117 (-a/--attach):**
- `RunArgs`: `--attach`/`-a STREAM` + `--sig-proxy` (Docker compat, ignored)
- `DetachedArgs` struct wraps run_detached args (clippy ≤7 limit)
- `pipe2(O_CLOEXEC)` before fork; watcher tees via `start_tee_relay`; parent relays to stdout/stderr
- Container name → stderr in attach mode; stdout clean for caller

**#116 (SpawnConfig.tmpfs + multi-name start):**
- `SpawnConfig.tmpfs: Vec<String>` — now persisted through restart
- `pelagos start n1 n2 ...` — starts sequentially; OCI fallback for single unknown ID

**#115 (exec image-config ENV):**
- `cmd_exec` loads `manifest.config.env` from `state.spawn_config.image`
- Merge: image_env base → proc/environ gaps filled → CLI `-e` wins

**#114 (image-config PATH):**
- Removed unconditional PATH from `apply_cli_options`
- `build_image_run`: inject fallback only if manifest.config.env omits it, then apply image env

**#112 (CA cert EBUSY):**
- Copy host CA cert to overlay upper dir in PARENT before fork (not bind-mount in pre_exec)

---

## Open Issues (GitHub)

| # | Title | Kind |
|---|-------|------|
| #73 | feat(wasm): persistent Wasm VM pool (epic #67 P4) | feat/low-pri |
| #71 | feat(wasm): WASI preview 2 socket passthrough (epic #67 P2) | feat/low-pri |
| #70 | feat(wasm): mixed Linux+Wasm compose validation (epic #67 P1) | feat/low-pri |
| #67 | epic: Wasm/WASI deeper support | epic/low-pri |
| #62 | feat: minimal --features build for embedded/IoT | feat/low-pri |
| #61 | feat: CRIU checkpoint/restore support | feat/low-pri |
| #49-47 | track: upstream runtime-tools test bugs | tracking |
| #60 | feat: io_uring opt-in seccomp profile | feat/low-pri |

---

## Next session: suggested starting point

```bash
cd /home/cb/Projects/pelagos
git log --oneline -5
cargo test --lib
gh issue list --state open
```

All issues filed as of 2026-03-17 are either closed or low-priority.
No in-progress work.  Repo is clean at v0.55.0.
