# Ongoing Tasks

## Last completed: image save / load (2026-02-27)

### Context

`remora image save` and `remora image load` export/import locally stored images
as OCI Image Layout tar archives.  The blob store (populated by `image pull` and
`remora build`) provides all the raw data; the commands just need to package/
unpackage it into the standard layout.

The format is OCI Image Layout (identical to `docker save` / `docker load`):

```
oci-layout                        # {"imageLayoutVersion":"1.0.0"}
index.json                        # OCI image index pointing at the manifest blob
blobs/sha256/<hex>                # manifest blob
blobs/sha256/<hex>                # config blob
blobs/sha256/<hex>                # layer blob (tar.gz) — one per layer
```

This makes archives interoperable with Docker, Podman, skopeo, crane, etc.

---

### API design

**`remora image save <reference> [-o <file.tar>]`**

```
1. load_image(reference)         → ImageManifest
2. load_oci_config(reference)    → config JSON bytes
3. compute sha256(config_json)   → config digest
4. for each layer digest in manifest.layers:
     load_blob(digest)           → layer bytes  (error if missing)
5. build OCI manifest JSON:
     { schemaVersion: 2,
       mediaType: "application/vnd.oci.image.manifest.v1+json",
       config: { mediaType: "...", digest: "sha256:<hex>", size: N },
       layers: [ { mediaType: "application/vnd.oci.image.layer.v1.tar+gzip",
                   digest: "sha256:<hex>", size: N }, ... ] }
6. compute sha256(manifest_json) → manifest digest
7. build index.json:
     { schemaVersion: 2,
       manifests: [ { mediaType: "...", digest: "sha256:<hex>", size: N,
                      annotations: { "org.opencontainers.image.ref.name": reference } } ] }
8. write tar to -o file (or stdout if omitted):
     oci-layout
     index.json
     blobs/sha256/<config-hex>
     blobs/sha256/<layer-hex>   (one entry per layer)
     blobs/sha256/<manifest-hex>
```

**`remora image load [-i <file.tar>] [--tag <reference>]`**

```
1. open tar from -i file (or stdin)
2. find and parse oci-layout  → verify imageLayoutVersion == "1.0.0"
3. find and parse index.json  → get manifest descriptor
4. read blobs/sha256/<manifest-hex> → parse OCI manifest
5. read blobs/sha256/<config-hex>   → config JSON bytes
6. for each layer descriptor in manifest.layers:
     read blobs/sha256/<layer-hex>  → blob bytes
     save_blob(digest, &bytes)      → blob store
     extract_layer(digest, blob_path) → layer dir
7. parse config JSON → ImageConfig
8. reference = --tag if supplied, else annotation "org.opencontainers.image.ref.name",
               else manifest digest (sha256:<hex>)
9. save_oci_config(reference, config_json)
10. save_image(ImageManifest { reference, digest: manifest_digest, layers, config })
11. println!("Loaded {reference}")
```

---

### Files to modify

| File | Change |
|------|--------|
| `src/cli/image.rs` | Add `cmd_image_save()`, `cmd_image_load()` |
| `src/main.rs` | Add `ImageCmd::Save`, `ImageCmd::Load`; wire dispatch |
| `docs/FEATURE_GAPS.md` | Mark image save/load as COMPLETE |
| `docs/INTEGRATION_TESTS.md` | Add entries for new tests |
| `tests/integration_tests.rs` | Add `test_image_save_load_roundtrip` |
| `ONGOING_TASKS.md` | This file |

No new dependencies needed: `tar` and `sha2`/inline sha256 are already present
(we use sha256 inline in `auth.rs`; `flate2` and `tar` crates already in deps).

Wait — check Cargo.toml: we have `flate2` and `tar` as deps, but no `sha2`.
The inline sha256 in auth.rs is for base64 only.  We need sha256 for blob
digests.  Options:
- Add `sha2` crate (clean, idiomatic)
- Use `std::process::Command` to call `sha256sum` (hacky)
- Reuse the sha256 already computed by the blobs (digests are already stored!)

**Decision:** for `save`, the layer digests are already known (stored in
`manifest.layers`); for the config blob digest we compute sha256 over the
config bytes.  We already depend on nothing for sha256 — but `build.rs` uses
the `sha2` + `hex` crates or computes via `flate2` piping.  Check Cargo.toml.

If `sha2` is already a dep (transitively used by `oci-client`), we can use it
directly.  Otherwise add it explicitly.

---

### Tests

**Unit (no root, no network):**
- `test_save_produces_oci_layout` — build a minimal fake ImageManifest + fake
  blob bytes in a tempdir, call the save logic, extract the tar, verify:
  - `oci-layout` present and valid JSON
  - `index.json` contains one manifest entry with correct ref annotation
  - `blobs/sha256/<hex>` present for config + each layer + manifest
  - manifest JSON references correct config and layer digests

**Integration (`#[ignore]`, requires root):**
- `test_image_save_load_roundtrip`:
  1. `remora image pull alpine:latest` (or use already-pulled image)
  2. `remora image save alpine:latest -o /tmp/alpine-test.tar`
  3. `remora image rm alpine:latest`
  4. `remora image load -i /tmp/alpine-test.tar`
  5. `remora run alpine:latest /bin/true` — must succeed
  - Asserts: exit code 0, image re-appears in `image ls`

---

### Verification

1. `cargo test --lib` — all existing + new unit tests pass
2. `cargo clippy -- -D warnings` + `cargo fmt --check`
3. Please run: `sudo -E cargo test --test integration_tests image_save -- --ignored --nocapture`
4. Manual: `remora image save alpine:latest | gzip -d | tar -tv` — inspect layout
5. Manual: `docker load < alpine-remora.tar` — Docker should accept the archive

### Verification done
- `cargo build` — clean
- `cargo clippy -- -D warnings` — clean
- `cargo fmt` — clean
- `cargo test --lib` — 254 tests pass
- `cargo test --bin remora -- cli::image::tests::test_build_oci_tar` — passes
- `cargo test --test integration_tests --no-run` — compiles clean
- Please run: `sudo -E cargo test --test integration_tests image_save_load -- --ignored --nocapture`

### Next suggested task

**Credential helper support** (`credHelpers`, `credsStore`) — delegate auth to
`docker-credential-ecr-login`, OS keychain, etc., so ECR/GCR users don't have
to pass `--password` or call `image login` with a short-lived token.

Or: **`remora image tag`** — assign a new local reference to an existing image
without pulling.

---

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
