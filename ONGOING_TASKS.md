# Ongoing Tasks

## Last completed: Registry Auth + Image Push (2026-02-27)

### What was done

Implemented full registry authentication and image push support:

**`src/cli/auth.rs`** (new)
- `resolve_auth(registry, username, password)` — resolution order: CLI flags →
  `REMORA_REGISTRY_USER`/`REMORA_REGISTRY_PASS` env vars → `~/.docker/config.json` → Anonymous
- `parse_docker_config(registry)` — reads and decodes `auths[registry].auth` (base64 `user:pass`)
- `write_docker_config(registry, user, pass)` — `remora image login`
- `remove_docker_config(registry)` — `remora image logout`
- Inline pure-Rust base64 encoder/decoder (no new dep)
- Unit tests: roundtrip, synthetic config.json, env var priority, CLI priority, anonymous fallback

**`src/paths.rs`**
- `blobs_dir()` — `<data>/blobs/`
- `blob_path(digest)` — `<data>/blobs/<hex>.tar.gz`
- `blob_diffid_path(digest)` — `<data>/blobs/<hex>.diffid`

**`src/image.rs`**
- `blob_exists()`, `save_blob()`, `load_blob()` — blob store CRUD
- `save_blob_diffid()`, `load_blob_diffid()` — uncompressed-tar sha256 sidecar
- `oci_config_path()`, `save_oci_config()`, `load_oci_config()` — raw OCI config JSON
- `ensure_image_dirs()` now creates `blobs_dir()` too

**`src/cli/image.rs`**
- `cmd_image_pull` now accepts `--username`, `--password`, `--password-stdin`
- `pull_image` persists raw blob bytes via `save_blob` + `save_oci_config`
- `cmd_image_push` — loads blobs from store, builds `ImageLayer::oci_v1_gzip`, calls `client.push()`
- `cmd_image_login` — prompts, writes `~/.docker/config.json`
- `cmd_image_logout` — removes entry from `~/.docker/config.json`
- `read_password_from_tty` — no-echo password input via `/dev/tty`

**`src/build.rs`**
- `create_layer_from_dir` now builds raw tar first → computes diff_id → compresses → saves blob + diffid sidecar
- `execute_build` calls `generate_oci_config_json` after building, saves to `oci-config.json`
- `generate_oci_config_json` — produces valid OCI config JSON with `diff_ids` from sidecars

**`src/main.rs`**
- `ImageCmd::Pull` — added `username`, `password`, `password_stdin` flags
- `ImageCmd::Push` — new variant with `reference`, `dest`, `username`, `password`, `password_stdin`
- `ImageCmd::Login` — new variant
- `ImageCmd::Logout` — new variant

**`src/cli/image.rs`** — `oci_client_config(registry, insecure)` helper
- Auto-detects localhost / RFC-1918 / 172.16–31.x as insecure
- Uses `ClientProtocol::HttpsExcept(vec![registry])` for plain-HTTP registries
- `--insecure` flag added to `image pull` and `image push`

**`docs/FEATURE_GAPS.md`** — marked registry auth + image push as COMPLETE

**`docs/INTEGRATION_TESTS.md`** — documented new tests including the two
registry auth integration tests

**`tests/integration_tests.rs`** — `mod registry_auth` (two `#[ignore]` tests):
- `test_local_registry_push_pull_roundtrip` — no-auth push/pull via `registry:2`
- `test_local_registry_auth_roundtrip` — htpasswd auth enforcement: anon push
  fails → login → push/pull succeed → logout → pull fails

**`scripts/test-registry-auth-e2e.sh`** — shell E2E against a real registry
(GHCR / Docker Hub / any); reads `REMORA_E2E_REGISTRY`, `REMORA_E2E_USER`,
`REMORA_E2E_TOKEN`, `REMORA_E2E_IMAGE`; tests login → push → pull → env-var
fallback → logout → post-logout-pull-fails

**`src/cli/run.rs`** — fixed `-v host:container:ro` parsing bug
- `split_once(':')` only splits on first colon, so `host:container:ro` produced
  `tgt = "container:ro"` (wrong). Changed to `rsplit_once(':')` so `:ro`/`:rw`
  suffix is correctly stripped; added `test_cli_volume_flag_ro` integration test.

**`tests/integration_tests.rs`** — fixed `test_local_registry_auth_roundtrip`
- Was using `openssl passwd -apr1` (APR1/MD5) which docker/distribution ≥2.8
  no longer accepts. Changed to the same hard-coded bcrypt entry (`$2y$05$...`,
  password `testpassword`) used by oci-client's own integration tests.

### Verification done
- `cargo build` — clean
- `cargo clippy -- -D warnings` — clean
- `cargo fmt` — clean
- `cargo test --lib` — 254 tests pass
- `cargo test --test integration_tests --no-run` — compiles clean
- `sudo -E cargo test --test integration_tests registry_auth -- --ignored --nocapture` — both registry auth tests pass
- `scripts/test-registry-auth-e2e.sh` against GHCR private + public, Docker Hub private + public — 44/44 pass

**`src/cli/auth.rs`** — fixed Docker Hub registry key lookup
- `registry_keys("index.docker.io")` now includes `"docker.io"` so creds stored
  by `remora image login docker.io` are found when pushing `docker.io/...` refs

**`scripts/test-registry-auth-e2e.sh`** — rewrote for multi-registry support
- Profiles: ghcr, dockerhub, ecr (each skipped if not configured)
- ECR: token auto-fetched via `aws ecr get-login-password`
- Per-registry pass/fail totals + global summary
- 8 tests per registry: anon-push-fails → login → push → pull-back → env-var
  fallback → CLI-flag fallback → logout → post-logout-pull-fails

**`scripts/e2e-creds.env.example`** — committed credential template (gitignored actual)

**`.gitignore`** — added `scripts/e2e-creds.env`

---

## Next suggested task

**`remora image save` / `remora image load`** — export/import images as tar archives
(see `docs/FEATURE_GAPS.md`).  Prerequisite: the blob store is now populated, so
save/load can use the same blobs.

Or: **credential helper support** (`credHelpers`, `credsStore`) for ECR/GCR/keychain
auth without typing passwords.
