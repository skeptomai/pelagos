# Rust Builder Stack

A containerised Rust build environment with **sccache** compiler caching.
Named volumes persist the cargo registry and sccache cache across container restarts,
so every rebuild after the first is faster — even across `compose down` / `compose up` cycles.

## Architecture

```
(no network)
  rust-builder — Alpine + rustc + cargo + sccache
                 volumes: cargo-registry  → /root/.cargo/registry
                          sccache-cache   → /sccache-cache
```

## Quick Start

```bash
# Build image and run smoke tests
sudo ./examples/compose/rust-builder/run.sh

# Or: start the container and shell into it manually
sudo pelagos compose up -f examples/compose/rust-builder/compose.reml -p rust-builder
sudo pelagos exec rust-builder-rust-builder /bin/sh

# Inside the container:
cd /workspace
cargo new hello && cd hello
cargo build        # compiles + populates sccache
cargo clean
cargo build        # rebuilds from sccache (faster)
sccache --show-stats
```

## Lisp Features Demonstrated

| Feature | Where |
|---------|-------|
| `define` | `rust-edition`, `mem-builder`, `cpu-builder` defined at the top |
| `env` with fallback | `MEM` / `CPU` / `SCCACHE_BUCKET` from host env, with safe defaults |
| `:volume name path` | Named volumes mount cargo registry and sccache into the container |
| `:command` | `sleep infinity` keeps the container alive for `pelagos exec` |
| `:env ("KEY" . value)` | Dotted pair syntax; `rust-edition` variable is evaluated at call-site |
| `define-service` | Flat keyword-style service definition via the stdlib macro |

## Configuration

Override without editing `compose.reml`:

```bash
# More memory and CPU
MEM=8g CPU=8.0 sudo pelagos compose up -f compose.reml -p rust-builder

# Distributed S3 sccache backend
SCCACHE_BUCKET=my-bucket sudo pelagos compose up -f compose.reml -p rust-builder
```

## Volumes

| Volume | Mounted At | Purpose |
|--------|-----------|---------|
| `rust-builder-cargo-registry` | `/root/.cargo/registry` | Cached crate downloads |
| `rust-builder-sccache-cache` | `/sccache-cache` | Compiled artefact cache |

Remove volumes on teardown with:

```bash
sudo pelagos compose down -f compose.reml -p rust-builder -v
```
