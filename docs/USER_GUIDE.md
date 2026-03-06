# Pelagos User Guide

Pelagos is a lightweight Linux container runtime with a CLI similar to Podman or nerdctl,
plus a Rust library API for embedding container isolation into your own programs.

---

## Installation & Setup

### Install

```bash
git clone https://github.com/skeptomai/pelagos.git
cd pelagos

# Option A: Install to /usr/local/bin (recommended)
scripts/install.sh

# Option B: Install to ~/.cargo/bin
cargo install --path .

# Option C: Install to /usr/local/bin via cargo
sudo cargo install --path . --root /usr/local
```

You can also download a pre-built binary from the
[Releases](https://github.com/skeptomai/pelagos/releases) page and copy it
to a directory on your PATH.

Verify the installation:

```bash
pelagos --help
```

### Pull an Image

```bash
# Rootless (images stored in ~/.local/share/pelagos/)
pelagos image pull alpine

# Or as root (images stored in /var/lib/pelagos/)
sudo pelagos image pull alpine

pelagos image ls
```

---

## Quick Start

### Rootless (no sudo)

```bash
# Pull and run — no root required
pelagos image pull alpine
pelagos run alpine /bin/echo hello

# Interactive shell with internet (Ctrl-D to exit)
pelagos run -i --network pasta alpine /bin/sh

# Check running containers
pelagos ps
```

### Root (full feature set)

```bash
# Run a one-shot command
sudo pelagos run alpine /bin/echo hello

# Interactive shell (Ctrl-D to exit)
sudo pelagos run -i alpine /bin/sh

# Detached (background) container
sudo pelagos run -d --name mybox alpine \
  /bin/sh -c 'while true; do echo tick; sleep 1; done'

# Check running containers
pelagos ps

# View logs
pelagos logs mybox
pelagos logs -f mybox         # follow (like tail -f)

# Stop and remove
sudo pelagos stop mybox
pelagos rm mybox
```

---

## OCI Images

Pelagos can pull images directly from OCI registries (Docker Hub, etc.) using anonymous
authentication. Image pull works both as root and rootless.

```bash
# Pull an image (rootless or root)
pelagos image pull alpine
pelagos image pull alpine:3.19
pelagos image pull library/ubuntu:latest

# List local images
pelagos image ls

# Run a container from an image
pelagos run alpine /bin/sh           # rootless
sudo pelagos run -i alpine /bin/sh   # root

# Remove a local image
pelagos image rm alpine
```

Pelagos automatically sets up a multi-layer overlayfs mount with an ephemeral upper/work
directory (writes are discarded when the container exits). Image config (Env, Cmd,
Entrypoint, WorkingDir) is applied as defaults that CLI flags override.

**Storage locations (root):**
- `/var/lib/pelagos/images/` -- image manifests and config
- `/var/lib/pelagos/layers/<sha256>/` -- content-addressable layer cache

**Storage locations (rootless):**
- `~/.local/share/pelagos/images/` -- image manifests and config
- `~/.local/share/pelagos/layers/<sha256>/` -- content-addressable layer cache

Root and rootless image stores are separate (matching Podman behavior). An image pulled
as root is not visible rootless, and vice versa.

### Tagging Images

```bash
# Create a new tag pointing to an existing image
pelagos image tag alpine:latest myapp:v1.0
pelagos image tag alpine myregistry.example.com/library/alpine:latest
```

Tags are references — no data is copied or duplicated.

### Pushing Images

```bash
# Push to Docker Hub (requires login)
pelagos image push myuser/myapp:v1.0

# Push to a different destination
pelagos image push myapp:v1.0 --dest myregistry.example.com/myapp:v1.0

# Push to an insecure (HTTP) registry
pelagos image push myapp:v1.0 --dest 127.0.0.1:5000/myapp:v1.0 --insecure

# Push with explicit credentials (without logging in first)
pelagos image push myapp:v1.0 --username myuser --password mypassword
```

### Registry Authentication

Credentials are stored in `~/.docker/config.json` (same format as Docker and Podman).

```bash
# Log in — reads password from stdin (recommended; avoids shell history)
echo "mypassword" | pelagos image login --username myuser --password-stdin ghcr.io

# Log out — removes stored credentials for the registry
pelagos image logout ghcr.io
```

### Saving and Loading Images

Save an image to an OCI Image Layout tar archive for offline transfer or backup:

```bash
# Save to a file
pelagos image save alpine:latest -o alpine.tar

# Save to stdout (pipe to ssh, s3, etc.)
pelagos image save alpine:latest | gzip > alpine.tar.gz

# Load from a file
pelagos image load -i alpine.tar

# Load from stdin
gunzip -c alpine.tar.gz | pelagos image load

# Load and apply a specific tag (overrides any tag in the archive)
pelagos image load -i alpine.tar --tag myalpine:imported
```

The tar format is the OCI Image Layout specification — archives produced by
`docker image save` and `skopeo copy oci-archive:` are compatible.

---

## WebAssembly / WASI

Pelagos runs WebAssembly modules from the same CLI and image store as Linux containers.
No other general-purpose container runtime does this — runc, crun, and youki treat
`.wasm` files as opaque executables and fail immediately at `exec()`. Wasm-native
runtimes (runwasi, Spin, WasmEdge shim) go the other direction: Wasm only, no Linux
OCI containers. Pelagos is the only runtime where both workload types share one CLI,
one image store, one compose format, and one node.

### How it works

`spawn()` reads the first 4 bytes of the target program. WebAssembly modules always
begin with `\0asm` (`0x00 0x61 0x73 0x6D`). If those bytes are present, the full
Linux container machinery — namespaces, overlayfs, seccomp, pivot_root — is bypassed
entirely and the module is handed to wasmtime or wasmedge. For ELF binaries, nothing
changes.

### Running a Wasm module directly

```bash
# From host filesystem — magic-byte detection triggers automatically
sudo pelagos run /path/to/app.wasm

# With WASI environment variables
sudo pelagos run --env DATABASE_URL=postgres://... /path/to/app.wasm

# With a preopened host directory mapped to a guest path
sudo pelagos run --bind /host/data:/app/data /path/to/app.wasm
```

### Wasm OCI images

```bash
# Pull a Wasm image
pelagos image pull ghcr.io/example/my-wasm-app:latest

# TYPE column shows "wasm" or "linux"
pelagos image ls
# REPOSITORY                          TAG     TYPE   SIZE
# ghcr.io/example/my-wasm-app        latest  wasm   1.8 MB
# alpine                              latest  linux  3.2 MB

# Run it — no rootfs, no overlayfs, starts in milliseconds
sudo pelagos run ghcr.io/example/my-wasm-app:latest

# With env and bind mounts
sudo pelagos run \
    --env CONFIG=/config/app.toml \
    --bind /etc/myapp:/config \
    ghcr.io/example/my-wasm-app:latest
```

### Building a Wasm OCI image

Use `FROM scratch` in a Remfile with a build stage that produces a `.wasm` output.
The build engine auto-detects the magic bytes and stores the layer with
`application/wasm` OCI media type:

```dockerfile
FROM rust:latest AS builder
RUN rustup target add wasm32-wasip1
COPY . /src
WORKDIR /src
RUN cargo build --release --target wasm32-wasip1

FROM scratch
COPY --from=builder /src/target/wasm32-wasip1/release/myapp.wasm /myapp.wasm
```

```bash
pelagos build -t myapp:wasm .
pelagos image ls        # TYPE shows "wasm"
sudo pelagos run myapp:wasm
```

### Runtime selection

Pelagos tries wasmtime first, then wasmedge (`Auto` mode). To select explicitly:

```bash
sudo pelagos run --wasm-runtime wasmtime /path/to/app.wasm
sudo pelagos run --wasm-runtime wasmedge /path/to/app.wasm
```

### containerd / Kubernetes

Install the shim and add it to containerd's config to schedule Wasm pods via
a `RuntimeClass` without a separate node agent:

```bash
sudo cp target/release/pelagos-shim-wasm \
    /usr/local/bin/containerd-shim-pelagos-wasm-v1
```

```toml
# /etc/containerd/config.toml
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.wasm]
  runtime_type = "io.containerd.pelagos.wasm.v1"
```

```yaml
# RuntimeClass for Kubernetes
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: pelagos-wasm
handler: wasm
```

### Current limitations

- **WASI surface is env + preopened dirs only.** WASI preview 2 sockets (TCP/UDP)
  are not yet threaded through `WasiConfig`. Wasm modules that open outbound
  network connections require a Linux container wrapping them for now.
- **No Wasm Component Model.** Components are stored with the correct OCI media type
  but executed identically to plain modules. Typed interface composition requires
  embedding the wasmtime Rust crate (planned, behind `--features embedded-wasm`).
- **Subprocess dispatch overhead.** Each invocation spawns a fresh wasmtime/wasmedge
  process (~5ms). Fine for long-running services; high-frequency short-lived
  invocations (>100/s) would benefit from a persistent VM pool (planned).

See [`docs/WASM_SUPPORT.md`](WASM_SUPPORT.md) for the full architecture and
comparison with runwasi, Spin, and WasmEdge.

---

## Building Images

Pelagos can build custom images from a **Remfile** (simplified Dockerfile). The build
process is daemonless (Buildah-style) — each `RUN` instruction spawns a container,
snapshots the overlay upper directory as a new layer, and stores it in the layer cache.

### Remfile Reference

```
FROM alpine:latest
RUN apk add --no-cache curl nginx
COPY index.html /var/www/index.html
ENV APP_PORT=8080
WORKDIR /var/www
CMD ["nginx", "-g", "daemon off;"]
EXPOSE 8080
```

| Instruction | Syntax | Effect |
|-------------|--------|--------|
| `FROM` | `FROM <image>[:<tag>] [AS <alias>]` | Load base image; optional alias for multi-stage builds |
| `RUN` | `RUN <command>` | Execute in a container; filesystem changes become a new layer |
| `COPY` | `COPY [--from=<stage>] <src> <dest>` | Copy from build context (or from a named stage) into image as a new layer |
| `ADD` | `ADD <src> <dest>` | Like COPY, but auto-extracts archives (.tar, .tar.gz, .tar.bz2, .tar.xz) and downloads URLs |
| `CMD` | `CMD ["arg1", "arg2"]` or `CMD command args` | Set default command (JSON array or shell form) |
| `ENTRYPOINT` | `ENTRYPOINT ["cmd"]` or `ENTRYPOINT cmd` | Set the container entrypoint (JSON array or shell form) |
| `ENV` | `ENV KEY=VALUE` or `ENV KEY VALUE` | Set an environment variable |
| `ARG` | `ARG <NAME>[=<default>]` | Declare a build-time variable; override with `--build-arg` |
| `WORKDIR` | `WORKDIR /path` | Set working directory for subsequent RUN and the final image |
| `EXPOSE` | `EXPOSE <port>[/protocol]` | Documentation only (no runtime effect) |
| `LABEL` | `LABEL key=value` | Add metadata label to the image |
| `USER` | `USER <uid>[:<gid>]` | Set the default user for RUN and the final image |

Comments (`#`) and blank lines are ignored. Continuation lines (trailing `\`) are supported.

### Remfile vs Dockerfile

**A valid Remfile is always a valid Dockerfile.** The syntax is identical — same
keywords, same `#` comments, same `\` line continuation. Remfile is a strict subset:
you can rename any Remfile to `Dockerfile` and `docker build` it without changes.

The converse is not true. These Dockerfile instructions are **not supported** and will
cause a parse error if used in a Remfile:

| Unsupported | Notes |
|-------------|-------|
| `HEALTHCHECK` | Not parsed |
| `SHELL` | Not parsed; `RUN` always uses `/bin/sh -c` |
| `STOPSIGNAL` | Not parsed |
| `VOLUME` | Not parsed; use `pelagos volume` or `with_volume()` at runtime |
| `ONBUILD` | Not parsed |

These **flags** are accepted by the parser but **silently ignored**:

| Flag | Instruction | Notes |
|------|-------------|-------|
| `--chown`, `--chmod`, `--link` | `COPY`, `ADD` | Ownership/mode changes not applied |
| `--mount`, `--network`, `--security` | `RUN` | BuildKit extensions, not supported |
| `--platform` | `FROM` | Multi-platform builds not supported |

Also note: `ENV` and `LABEL` only accept a **single `KEY=VALUE` pair per line**.
Docker allows space-separated multiple pairs on one line; Remfile does not.

### ARG and Build Arguments

Use `ARG` to declare build-time variables that can be overridden from the command line:

```
ARG VERSION=1.0
ARG BASE_IMAGE=alpine

FROM ${BASE_IMAGE}:latest
RUN echo "Building version $VERSION"
COPY app-${VERSION}.tar.gz /tmp/
```

- `$VAR` and `${VAR}` are substituted in all instructions after the `ARG` declaration
- Use `$$` to produce a literal `$` (escape)
- Override defaults with `--build-arg`: `pelagos build -t app --build-arg VERSION=2.0 .`
- `ARG` before `FROM` is valid and allows parameterizing the base image (Docker-compatible)

### ADD vs COPY

Both `ADD` and `COPY` bring files from the build context into the image. Use `COPY` for
plain file copies — it's simpler and more predictable. Use `ADD` when you need:

- **Archive auto-extraction**: `.tar`, `.tar.gz`, `.tar.bz2`, and `.tar.xz` sources are
  automatically extracted into the destination directory
- **URL downloads**: `ADD https://example.com/file.txt /opt/` downloads the URL into the image

```
COPY config.json /etc/myapp/config.json
ADD https://example.com/data.tar.gz /opt/data/
```

### Multi-Stage Builds

Multi-stage builds let you use one stage for building and another for the final image,
keeping the output small. Each `FROM` instruction starts a new stage:

```
FROM alpine AS builder
RUN apk add --no-cache gcc musl-dev
COPY hello.c /src/hello.c
RUN gcc -static -o /src/hello /src/hello.c

FROM alpine
COPY --from=builder /src/hello /usr/local/bin/hello
CMD ["/usr/local/bin/hello"]
```

Only the **final stage** produces the output image. Intermediate stages are discarded,
so build tools and source code don't bloat the result. Use `COPY --from=<stage>` to
cherry-pick artifacts from earlier stages.

### `.remignore`

Place a `.remignore` file in the build context root to exclude files from `COPY` and `ADD`
instructions. The syntax is identical to `.gitignore`:

```
# Ignore build artifacts
target/
*.o

# Ignore version control
.git/

# But include this specific file
!important.o
```

This keeps the build context lean and prevents large or sensitive files from being copied
into the image.

### Building

```bash
# Build from Remfile in current directory
sudo pelagos build -t myapp:latest .

# Build from a specific Remfile
sudo pelagos build -t myapp:latest -f path/to/Remfile .

# Specify network mode for RUN steps (default: bridge for root, pasta for rootless)
sudo pelagos build -t myapp:latest --network bridge .

# Rootless build (uses pasta networking for RUN steps)
pelagos build -t myapp:latest .
```

The base image must be pulled locally first:

```bash
pelagos image pull alpine
pelagos build -t myapp:latest .
```

### Running Built Images

Built images are stored in the same image store as pulled images and can be run
with `pelagos run`:

```bash
sudo pelagos build -t myapp:latest .
sudo pelagos run myapp:latest
sudo pelagos run -i myapp:latest /bin/sh   # override CMD with interactive shell
```

### Build Example

```bash
# Create a build context
mkdir myapp && cd myapp
echo '<h1>Hello from Pelagos</h1>' > index.html

cat > Remfile <<'EOF'
FROM alpine:latest
RUN apk add --no-cache curl
COPY index.html /srv/index.html
ENV GREETING=hello
WORKDIR /srv
CMD ["/bin/sh", "-c", "cat index.html && echo $GREETING"]
EOF

# Pull base image, build, and run
sudo pelagos image pull alpine
sudo pelagos build -t myapp:latest .
sudo pelagos run myapp:latest
```

### Multi-Stage Build Example

```bash
cat > Remfile <<'EOF'
ARG PROFILE=release

FROM alpine AS builder
RUN apk add --no-cache rust cargo musl-dev
COPY . /src
WORKDIR /src
RUN cargo build --$PROFILE

FROM alpine
COPY --from=builder /src/target/release/myapp /usr/local/bin/myapp
CMD ["/usr/local/bin/myapp"]
EOF

sudo pelagos image pull alpine
sudo pelagos build -t myapp:latest .
sudo pelagos build -t myapp:debug --build-arg PROFILE=debug .
```

See `examples/multi-stage/` for a complete working example.

### `build` Reference

```
pelagos build [OPTIONS] [CONTEXT]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--tag <TAG>` | `-t` | Image tag (required), e.g. `myapp:latest` |
| `--file <PATH>` | `-f` | Path to Remfile (default: `<context>/Remfile`) |
| `--network <MODE>` | | Network for RUN steps: `bridge`, `pasta`, `none`, `auto` (default) |
| `--build-arg <KEY=VALUE>` | | Set a build-time variable (can be repeated) |
| `--no-cache` | | Disable build cache; re-run all steps |
| `CONTEXT` | | Build context directory (default: `.`) |

`--network auto` selects `bridge` when running as root, `pasta` when rootless.

---

## Container Lifecycle

```bash
# List running containers
pelagos ps

# List all containers (including exited)
pelagos ps --all

# View logs (detached containers)
pelagos logs <name>
pelagos logs --follow <name>

# Stop a container (sends SIGTERM)
sudo pelagos stop <name>

# Remove a stopped container
pelagos rm <name>

# Force remove (SIGKILL + remove even if running)
pelagos rm --force <name>
```

---

## Compose

`pelagos compose` orchestrates multi-service applications using an S-expression compose file
(default: `compose.reml`).

### Basic Usage

```bash
# Start all services (daemonised)
sudo pelagos compose up

# Start in foreground
sudo pelagos compose up --foreground

# Use a custom file and project name
sudo pelagos compose up -f mystack.reml -p myproject

# List services
pelagos compose ps

# View logs
pelagos compose logs
pelagos compose logs --follow api

# Stop and remove all services
sudo pelagos compose down

# Stop and remove services + volumes
sudo pelagos compose down -v
```

### Compose File Format

```lisp
(compose
  (network backend (subnet "10.88.1.0/24"))
  (volume pgdata)

  (service db
    (image "postgres:16")
    (network backend)
    (volume pgdata "/var/lib/postgresql/data")
    (env POSTGRES_PASSWORD "secret")
    (port 5432 5432)
    (memory "512m"))

  (service api
    (image "my-api:latest")
    (network backend)
    (depends-on (db :ready-port 5432))
    (port 8080 8080)))
```

### `depends-on` and Health Checks

`depends-on` controls startup order. Without a health check, Pelagos just verifies the
dependency process is alive. With a health check, Pelagos polls until the check passes
(60-second timeout, 250ms interval).

#### Shorthand: `:ready-port N`

```lisp
; TCP connect to port 5432
(depends-on (db :ready-port 5432))
```

#### Full `:ready` Syntax

```lisp
; TCP connect
(depends-on (db :ready (port 5432)))

; HTTP GET — host is replaced with the container IP; returns true for 2xx
(depends-on (api :ready (http "http://localhost:8080/healthz")))

; Command in container — true if exit code 0 (single-string form, split on whitespace)
(depends-on (db :ready (cmd "pg_isready -U postgres")))

; Multi-argument form
(depends-on (db :ready (cmd "pg_isready" "-U" "postgres")))
```

#### Composable Operators: `and` / `or`

```lisp
; All checks must pass
(depends-on (db :ready (and (port 5432) (cmd "pg_isready -U postgres"))))

; Any check may pass
(depends-on (api :ready (or (http "http://localhost:8080/health") (port 8080))))

; Nested: port AND (http OR cmd)
(depends-on (svc :ready (and (port 8080) (or (http "http://localhost:8080/ready") (cmd "test -f /var/ready")))))

; Multiple dependencies with mixed checks
(depends-on
  (db    :ready (and (port 5432) (cmd "pg_isready")))
  (cache :ready (or  (port 6379) (http "http://localhost:6380/ping"))))
```

#### Check Types Reference

| Expression | What it does |
|---|---|
| `(port N)` | TCP connect to the container's IP on port N |
| `(http "URL")` | HTTP GET (host replaced with container IP); passes on 2xx |
| `(cmd "str")` | Run command in container's namespaces; passes if exit 0 |
| `(cmd "exe" "a1" ...)` | Multi-arg form of cmd |
| `(and e1 e2 ...)` | All sub-checks must pass |
| `(or e1 e2 ...)` | Any sub-check must pass |

---

## Compose Files (`.reml`)

All compose files use the `.reml` Lisp format. Simple stacks are just data declarations;
complex stacks can use the full language — loops, conditionals, parameterised services,
shared templates. The default file is `compose.reml`.

```bash
sudo pelagos compose up -f compose.reml
sudo pelagos compose up                   # defaults to compose.reml
```

### Language

`.reml` is a Scheme-like Lisp with:

- **`define`** — bind variables and define functions
- **`lambda`** — first-class functions
- **`let` / `let*` / `letrec`** — lexical scoping
- **Named `let`** — idiomatic loops
- **`do`** — imperative loops
- **`if` / `cond` / `when` / `unless`** — conditionals
- **`begin`** — sequence
- **`and` / `or`** — short-circuit boolean
- **Quasiquote** — `` ` ``, `,`, `,@` (splice)
- **TCO** — tail calls don't grow the stack
- **~55 standard builtins** — arithmetic, list operations, strings, higher-order functions

### Domain Builtins

These return typed values that `compose` collects:

| Form | Returns |
|------|---------|
| `(service name opts...)` | `ServiceSpec` |
| `(network name opts...)` | `NetworkSpec` |
| `(volume name)` | `VolumeSpec` |
| `(compose items...)` | `ComposeSpec` — flattens nested lists |
| `(compose-up spec [project] [foreground?])` | runs the spec (deferred) |
| `(on-ready "svc" lambda)` | registers hook fired when service becomes healthy |
| `(env "VAR")` | string value or `()` if unset |
| `(log msg ...)` | logs via `log::info!` |

### Service Options

Options are passed as Lisp lists with a symbol key:

```lisp
(service "db"
  (list 'image   "postgres:16")
  (list 'network "backend")
  (list 'env     "POSTGRES_PASSWORD" "secret")
  (list 'port    5432 5432)
  (list 'memory  "512m")
  (list 'depends-on "cache"))
```

Alternatively, use `quote` shorthand:

```lisp
(service "db"
  '(image   "postgres:16")
  '(network "backend"))
```

### Example: Parameterised Services

```lisp
; Service factory
(define (web-service name port)
  (service name
    (list 'image   "myapp:latest")
    (list 'network "backend")
    (list 'port    port port)
    (list 'depends-on "db")))

; Scale out three replicas
(define web-services
  (map (lambda (pair)
         (web-service (car pair) (cadr pair)))
       '(("web-1" 8081) ("web-2" 8082) ("web-3" 8083))))

; Hook: run migration after db is ready
(on-ready "db"
  (lambda ()
    (log "db is ready — migrations would run here")))

; Assemble and run
(compose-up
  (compose
    (network "backend" (list 'subnet "10.89.0.0/24"))
    (service "db"
      (list 'image "postgres:16")
      (list 'network "backend")
      (list 'env "POSTGRES_PASSWORD" "secret"))
    web-services))
```

### Example: Environment-driven Config

```lisp
(define db-password
  (let ((p (env "DB_PASSWORD")))
    (if (null? p)
        (error "DB_PASSWORD must be set")
        p)))

(compose-up
  (compose
    (service "db"
      (list 'image "postgres:16")
      (list 'env "POSTGRES_PASSWORD" db-password))))
```

### `on-ready` Hooks

Hooks fire in the compose supervisor after a service's health check passes and before
dependent services start. Each hook is a zero-argument lambda:

```lisp
(on-ready "db"
  (lambda ()
    (log "running post-start initialisation")))
```

Multiple hooks may be registered for the same service; they fire in registration order.

---

## Imperative Orchestration with Futures

`.reml` files can express imperative orchestration: start containers in
dependency order, compute connection strings from assigned IPs, thread data
between services, and run independent services in parallel — all from plain
Lisp.

### Futures and Executors

The model separates **what** from **when**:

- **Futures** (`start`, `then`, `then-all`) are pure descriptions of work.
  Nothing executes when a future is created.
- **Executors** (`run`, `resolve`) decide when and how to run the work.

### The `define-*` macro family

Four macros cover the common patterns without boilerplate:

| Macro | Purpose |
|-------|---------|
| `(define-service var "name" opts...)` | Declare a service spec (no container starts) |
| `(define-nodes (var svc) ...)` | Declare multiple lazy start nodes at once |
| `(define-then name upstream (param) body...)` | Compute a value once `upstream` resolves |
| `(define-run [opts] (var future) ...)` | Execute the graph and bind results in one form |
| `(define-results results (var "key") ...)` | Destructure a `run` result alist |

### A complete example

```lisp
;; ── Service declarations ──────────────────────────────────────────────
(define-service svc-db "db"
  :image   "postgres:16"
  :network "app-net"
  :env     ("POSTGRES_PASSWORD" . "secret")
           ("POSTGRES_DB"       . "appdb"))

(define-service svc-cache "cache"
  :image   "redis:7-alpine"
  :network "app-net")

(define-service svc-migrate "migrate"
  :image   "alpine:latest"
  :network "app-net"
  :command '("/bin/sh" "-c" "echo \"migrating ${DATABASE_URL}\"; exit 0"))

(define-service svc-app "app"
  :image   "myapp:latest"
  :network "app-net")

;; ── Declare the graph — nothing executes yet ─────────────────────────
;;
;;   db ──→ db-url ──→ migrate ──→ app
;;   db ──→ db-url ─────────────→ app  (DATABASE_URL env)
;;   cache ──→ cache-url ─────────→ app
;;
(define-nodes
  (db    svc-db)
  (cache svc-cache))

;; Compute connection strings once each container is up
(define-then db-url db (h)
  (format "postgres://app:secret@~a/appdb" (container-ip h)))

(define-then cache-url cache (h)
  (format "redis://~a:6379" (container-ip h)))

;; Migration runs first; app waits for it to complete
(define migrate (start svc-migrate
  :needs (list db-url)
  :env   (lambda (url) `(("DATABASE_URL" . ,url)))))

;; App waits for migration, then starts with both URLs
(define app (start svc-app
  :needs (list migrate db-url cache-url)
  :env   (lambda (_ db-url cache-url)
           `(("DATABASE_URL" . ,db-url)
             ("CACHE_URL"    . ,cache-url)))))

;; ── Execute and bind ──────────────────────────────────────────────────
;; migrate, db-url, cache-url are discovered automatically from app's
;; :needs — only list the containers whose handles you need.
(define-run :parallel
  (db-handle    db)
  (cache-handle cache)
  (app-handle   app))

;; ── Wait and clean up ─────────────────────────────────────────────────
;; container-wait cascades container-stop through app's transitive deps
;; (migrate, cache, db) automatically — the same graph that governed
;; startup governs shutdown.  No manual stop calls needed.
(with-cleanup (lambda (result)
                (if (ok? result)
                  (logf "app exited cleanly (code ~a)" (ok-value result))
                  (logf "app failed: ~a" (err-reason result))))
  (container-wait app-handle))
```

### `run` — the static executor

`run` receives a list of *terminal* futures (the ones whose handles you need),
discovers all transitive `:needs` dependencies automatically, topologically
sorts the full graph, detects cycles, and executes in order.  Returns an alist
of `("name" . resolved-value)` pairs for the listed futures only.

```lisp
(run (list db cache app))                    ; serial
(run (list db cache app) :parallel)          ; tier-parallel
(run (list db cache app) :parallel           ; capped at 4 simultaneous
     :max-parallel 4)
```

With `:parallel`, independent futures in each tier run concurrently.  The
executor blocks between tiers — dependencies are always respected.

```
Tier 1:  db ∥ cache                    (no dependencies)
Tier 2:  db-url ∥ cache-url            (unblocked after tier 1)
Tier 3:  migrate                        (unblocked after db-url)
Tier 4:  app                            (unblocked after migrate + cache-url)
```

The graph declaration is unchanged between serial and parallel execution.
`:parallel` is an execution-policy keyword, not a structural one.

### `resolve` — the dynamic executor

`resolve` walks a single future chain depth-first, without needing the full
graph declared upfront.  If a `then` lambda returns a new `Future`, `resolve`
executes it automatically (**monadic flatten**).  Use this for linear pipelines
where each step's next container depends on the previous step's value:

```lisp
(define pipeline
  (then db
    (lambda (db-handle)
      (let ((db-url (format "postgres://...@~a/db" (container-ip db-handle))))
        (then (start svc-migrate
                :needs (list db)
                :env   (lambda (_) `(("DATABASE_URL" . ,db-url))))
          (lambda (_)
            (start svc-app
              :needs (list db)
              :env   (lambda (_) `(("DATABASE_URL" . ,db-url))))))))))

(define app (resolve pipeline))
```

`resolve` does not support `:parallel` or upfront cycle detection.  Prefer
`run` when the full graph is known upfront.

### Eager (imperative) execution

The graph model describes *what* depends on *what*; the executor decides *when*
to run each node.  The eager model is imperative: you call functions and receive
results immediately, writing ordinary sequential or parallel Lisp code.

**`(container-start svc [:env list])`** — starts a container synchronously and
returns a `ContainerHandle`.  `:env` is an optional list of `(KEY . value)` pairs
merged into the service environment:

```lisp
(define db  (container-start svc-db))
(define url (format "postgres://...@~a/db" (container-ip db)))
(define app (container-start svc-app :env (list (cons "DATABASE_URL" url))))
```

**`(container-start-bg svc [:env list])`** — starts a container in a background
thread and returns a `PendingContainer` *immediately*.  The calling thread is
not blocked.

**`(container-join pending)`** — blocks until the background container is ready
and returns a `ContainerHandle`.  The first join consumes the pending handle;
a second join raises `"already joined"`.

```lisp
;; Parallel eager — overlap startup latency of db and cache.
(define db-p    (container-start-bg svc-db))
(define cache-p (container-start-bg svc-cache))
;; Both are starting now.
(define db    (container-join db-p))
(define cache (container-join cache-p))
(logf "db at ~a, cache at ~a" (container-ip db) (container-ip cache))
```

### Choosing between executors

| Situation | Use |
|-----------|-----|
| Multiple independent services (db ∥ cache) | `run :parallel` |
| Upfront cycle detection | `run` |
| Parallel dispatch across tiers | `run :parallel` |
| Linear chain; next step determined by previous value | `resolve` |
| Short pipeline, no intermediate names needed | `resolve` |
| Sequential imperative; immediate results | `container-start` |
| Parallel imperative without declarative graph | `container-start-bg` + `container-join` |

**`define-run`** combines `run` and `define-results` into one form, removing
the redundancy of listing the same services twice.  The result key for each
binding is derived automatically from the future variable name:

```lisp
(define-run :parallel
  (db-handle    db)
  (cache-handle cache)
  (app-handle   app))
```

This replaces the two-step pattern:
```lisp
(define results (run (list db cache app) :parallel))
(define-results results
  (db-handle    "db")
  (cache-handle "cache")
  (app-handle   "app"))
```

Use `define-run` when the future variable names match the service names
(the standard `define-nodes` convention).  Use `run` + `define-results`
when the names differ or you only need a subset of results.

See `docs/REML_EXECUTOR_MODEL.md` for the full design reference: transitive
discovery, hybrid static+conditional patterns, threading model, eager execution,
and error message behaviour.

---

## Exec Into a Running Container

Run a command inside an already-running container by joining its namespaces:

```bash
# Run a command
pelagos exec mybox /bin/ls /

# Interactive shell
pelagos exec -i mybox /bin/sh

# With environment variables
pelagos exec -e FOO=bar mybox /bin/env

# With a specific working directory
pelagos exec -w /tmp mybox /bin/pwd

# As a specific user
pelagos exec -u 1000:1000 mybox /bin/id
```

`exec` discovers the container's namespaces from `/proc/<pid>/ns/*` and joins them with
`setns()`. The container's environment is inherited from `/proc/<pid>/environ`, with
`-e` flags as overrides.

---

## Networking

By default, containers have no network (`--network none`).

### Network Modes

```bash
# No network (default)
sudo pelagos run alpine /bin/sh

# Loopback only (127.0.0.1, no external access)
sudo pelagos run --network loopback alpine /bin/sh

# Bridge networking (veth pair + pelagos0 bridge, 172.19.0.x/24)
sudo pelagos run --network bridge --nat alpine /bin/sh

# Pasta (rootless, full internet access via user-mode networking)
pelagos run --network pasta alpine /bin/sh    # no sudo needed
```

### NAT, Port Forwarding, and DNS

```bash
# Enable outbound internet (MASQUERADE via nftables)
sudo pelagos run --network bridge --nat alpine /bin/sh

# Publish ports (host:container TCP forwarding)
sudo pelagos run --network bridge --nat -p 8080:80 alpine /bin/sh

# Custom DNS servers
sudo pelagos run --network bridge --nat --dns 1.1.1.1 --dns 8.8.8.8 alpine /bin/sh
```

### DNS Backend

Pelagos supports two DNS backends for container name resolution on bridge networks:

- **builtin** (default): the embedded `pelagos-dns` daemon — minimal, zero-dependency A-record server with upstream forwarding
- **dnsmasq**: production-grade DNS with caching, AAAA support, EDNS, and DNSSEC

Select the backend via the `--dns-backend` flag or the `PELAGOS_DNS_BACKEND` environment variable:

```bash
# Use dnsmasq for DNS (requires dnsmasq installed)
sudo pelagos run --network bridge --nat --dns-backend dnsmasq alpine /bin/sh

# Or via environment variable
sudo PELAGOS_DNS_BACKEND=dnsmasq pelagos run --network bridge --nat alpine /bin/sh
```

If dnsmasq is requested but not found on PATH, Pelagos logs a warning and falls back to the builtin backend.

### Container Linking

```bash
# Link containers by name (injects /etc/hosts entry)
sudo pelagos run -d --name db --network bridge --nat alpine /bin/sh -c 'sleep 3600'
sudo pelagos run --network bridge --nat --link db alpine /bin/sh -c 'ping -c1 db'
```

### Rootless Networking

For rootless containers (no sudo), use `--network pasta`. This requires
[pasta](https://passt.top) (from the passt project) to be installed.

```bash
# Full internet access without root
pelagos run --network pasta -i alpine /bin/sh
```

Bridge networking requires root and is rejected in rootless mode.

---

## Storage

### Named Volumes

Volumes persist data across container runs. They are stored in the data directory
(root: `/var/lib/pelagos/volumes/`, rootless: `~/.local/share/pelagos/volumes/`).

```bash
# Create a volume (works rootless)
pelagos volume create mydata

# List volumes
pelagos volume ls

# Use a volume (auto-created if it doesn't exist)
pelagos run -v mydata:/data alpine /bin/sh

# Remove a volume
pelagos volume rm mydata
```

### Bind Mounts

Map host directories into the container:

```bash
# Read-write bind mount
sudo pelagos run --bind /host/path:/container/path alpine /bin/sh

# Read-only bind mount
sudo pelagos run --bind-ro /etc/hosts:/etc/hosts alpine /bin/sh
```

### tmpfs

In-memory writable directories (useful with `--read-only`):

```bash
sudo pelagos run --read-only --tmpfs /tmp --tmpfs /run alpine /bin/sh
```

### Overlay (with OCI images)

When using OCI images, Pelagos automatically creates a multi-layer overlayfs mount. The
base image layers are read-only; writes go to an ephemeral upper directory that is
discarded when the container exits.

In rootless mode, Pelagos uses the `userxattr` mount option (kernel 5.11+) or falls back
to `fuse-overlayfs` automatically. See [Rootless Overlay](#rootless-overlay) for details.

---

## Security & Isolation

### Read-Only Rootfs

```bash
sudo pelagos run --read-only alpine /bin/sh
# Combine with --tmpfs for writable scratch space
sudo pelagos run --read-only --tmpfs /tmp alpine /bin/sh
```

### Capabilities

```bash
# Drop all capabilities (most restrictive)
sudo pelagos run --cap-drop ALL alpine /bin/sh

# Drop all, then add back specific ones
sudo pelagos run --cap-drop ALL --cap-add NET_BIND_SERVICE alpine /bin/sh
```

Supported capabilities: CHOWN, DAC_OVERRIDE, FOWNER, SETGID, SETUID,
NET_BIND_SERVICE, NET_RAW, SYS_CHROOT, SYS_ADMIN, SYS_PTRACE.

### Seccomp Profiles

```bash
# Docker's default seccomp profile (recommended)
sudo pelagos run --security-opt seccomp=default alpine /bin/sh

# Minimal profile (tighter restrictions)
sudo pelagos run --security-opt seccomp=minimal alpine /bin/sh

# Disable seccomp entirely
sudo pelagos run --security-opt seccomp=none alpine /bin/sh
```

### Other Security Options

```bash
# Prevent privilege escalation via setuid/setgid binaries
sudo pelagos run --security-opt no-new-privileges alpine /bin/sh

# Mask sensitive kernel paths
sudo pelagos run --masked-path /proc/kcore --masked-path /proc/sysrq-trigger alpine /bin/sh

# Set kernel parameters inside container
sudo pelagos run --sysctl net.ipv4.ip_forward=1 alpine /bin/sh

# Run as non-root user inside container
sudo pelagos run --user 1000:1000 alpine /bin/id
```

---

## Resource Limits

### Cgroups v2

```bash
# Memory limit (supports k, m, g suffixes)
sudo pelagos run --memory 256m alpine /bin/sh

# CPU quota (fractional CPUs: 0.5 = 50% of one core)
sudo pelagos run --cpus 0.5 alpine /bin/sh

# CPU shares/weight (relative to other containers)
sudo pelagos run --cpu-shares 512 alpine /bin/sh

# Max number of processes
sudo pelagos run --pids-limit 50 alpine /bin/sh
```

### rlimits

```bash
# Set file descriptor limit
sudo pelagos run --ulimit nofile=1024:2048 alpine /bin/sh

# Set max processes
sudo pelagos run --ulimit nproc=100:200 alpine /bin/sh
```

Supported ulimit resources: nofile (openfiles), nproc (maxproc), as (vmem), cpu,
fsize, memlock, stack, core, rss, msgqueue, nice, rtprio.

---

## Rootless Mode

### Rootless-First Design Philosophy

Pelagos is designed rootless-first: the default path never requires root, and
root access is required only when the kernel genuinely demands it (host bridge
manipulation, nftables, joining namespaces owned by root processes).

Auto-detection is automatic: `getuid() != 0` → rootless mode. No flag needed —
just omit `sudo`.

```bash
# Pull and run — fully rootless
pelagos image pull alpine
pelagos run alpine /bin/echo hello

# Interactive shell with internet
pelagos run -i --network pasta alpine /bin/sh
```

**What works rootless (no sudo):**
- `pelagos image pull/push/ls/rm/save/load/tag/login/logout`
- `pelagos build` — pasta for RUN networking, native or fuse overlay for layers
- `pelagos run` with no network, `--network pasta`, or `--network loopback`
- `pelagos compose` when no `(network ...)` declarations are used
- `pelagos ps`, `pelagos logs`, `pelagos rm` — state file operations
- `pelagos volume create/ls/rm`
- Named volumes (`-v mydata:/data`), tmpfs mounts
- User namespace isolation (auto-configured: container UID 0 maps to your host UID)

**What requires root (`sudo`):**
- Bridge networking (`--network bridge`) — host veth/bridge setup, `CAP_NET_ADMIN`
- NAT and port mapping (`--nat`, `-p`) — nftables MASQUERADE, requires root
- `pelagos network create/rm` — host bridge + nftables manipulation
- `pelagos exec` on a root-spawned container — joining root namespaces needs `CAP_SYS_PTRACE`
- OCI lifecycle commands: `create`, `start`, `state`, `kill`, `delete`
- `pelagos stop` on a root-owned container — signalling a root process

### Overlay Fallback Chain

Pelagos automatically selects the best available overlay implementation:

1. **Kernel overlayfs with `userxattr`** (kernel ≥ 5.11) — zero-copy, kernel-native, best performance
2. **`fuse-overlayfs`** (any kernel) — FUSE round-trip overhead; perceptible only for heavy random I/O workloads
3. **Error with instructions** if neither is available

**Performance trade-offs:**

| Backend | When used | Performance |
|---------|-----------|-------------|
| Kernel overlayfs | root, or rootless + kernel ≥5.11 + `userxattr` | Best: zero-copy, kernel-native |
| fuse-overlayfs | rootless, kernel < 5.11 or no `userxattr` | FUSE overhead; negligible for typical `apk add`, `go build`, `npm install` |

For typical workloads the difference between backends is not perceptible. For
pathological cases (millions of small file ops in a single build step), kernel
5.11+ or root mode will be faster.

### Rootless Overlay

Pelagos uses overlayfs with the `userxattr` mount option on kernel 5.11+. This stores
whiteout metadata in `user.*` extended attributes instead of `trusted.*`, which doesn't
require `CAP_SYS_ADMIN`.

On older kernels, Pelagos automatically falls back to
[fuse-overlayfs](https://github.com/containers/fuse-overlayfs). If neither works, you'll
see a clear error with instructions.

```bash
# Install fuse-overlayfs (only needed for kernels < 5.11)
# Arch Linux
pacman -S fuse-overlayfs
# Debian/Ubuntu
apt install fuse-overlayfs
# Fedora
dnf install fuse-overlayfs
```

### Rootless Storage

Rootless mode uses XDG Base Directory paths:

| Purpose | Root path | Rootless path |
|---------|-----------|---------------|
| Images & layers | `/var/lib/pelagos/` | `~/.local/share/pelagos/` |
| Volumes | `/var/lib/pelagos/volumes/` | `~/.local/share/pelagos/volumes/` |
| Runtime state | `/run/pelagos/` | `$XDG_RUNTIME_DIR/pelagos/` |

Root and rootless stores are completely separate (same as Podman). An image pulled as
root is not available in rootless mode, and vice versa.

---

## Complete `run` Reference

```
pelagos run [OPTIONS] <IMAGE> [COMMAND [ARGS...]]
pelagos run [OPTIONS] --rootfs <ROOTFS> [COMMAND [ARGS...]]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--name <NAME>` | | Container name (auto-generated if omitted) |
| `--detach` | `-d` | Run in background |
| `--interactive` | `-i` | Allocate a PTY (incompatible with `--detach`) |
| `--rootfs <ROOTFS>` | | Use a local rootfs instead of an OCI image (advanced) |
| `--network <MODE>` | | `none` (default), `loopback`, `bridge`, `pasta` |
| `--publish <H:C>` | `-p` | TCP port forward host:container (repeatable) |
| `--nat` | | Enable MASQUERADE NAT (requires bridge) |
| `--dns <IP>` | | DNS server (repeatable) |
| `--dns-backend <BE>` | | DNS backend: `builtin` (default) or `dnsmasq` |
| `--link <NAME[:ALIAS]>` | | Link to another container |
| `--volume <V:/PATH>` | `-v` | Named volume (auto-created) |
| `--bind <H:C>` | | RW bind mount host:container (repeatable) |
| `--bind-ro <H:C>` | | RO bind mount (repeatable) |
| `--tmpfs <PATH>` | | tmpfs mount (repeatable) |
| `--read-only` | | Make rootfs read-only |
| `--env <K=V>` | `-e` | Environment variable (repeatable) |
| `--env-file <PATH>` | | Load environment from file |
| `--workdir <PATH>` | `-w` | Working directory inside container |
| `--user <UID[:GID]>` | `-u` | User/group to run as |
| `--hostname <NAME>` | | Container hostname |
| `--memory <LIMIT>` | | Memory limit (e.g. `256m`, `1g`) |
| `--cpus <FRAC>` | | CPU quota as fraction (e.g. `0.5`) |
| `--cpu-shares <N>` | | CPU weight (relative) |
| `--pids-limit <N>` | | Max number of processes |
| `--ulimit <R=S:H>` | | rlimit (e.g. `nofile=1024:2048`) (repeatable) |
| `--cap-drop <CAP>` | | Drop capability (repeatable, or `ALL`) |
| `--cap-add <CAP>` | | Add capability (repeatable) |
| `--security-opt <OPT>` | | `seccomp=default\|minimal\|none`, `no-new-privileges` |
| `--sysctl <K=V>` | | Kernel parameter (repeatable) |
| `--masked-path <PATH>` | | Path to mask inside container (repeatable) |
| `--rm` | | Remove container automatically when it exits |

---

## OCI Runtime Interface

For integration with container managers like containerd or CRI-O, Pelagos implements the
OCI Runtime Spec lifecycle commands. These operate on OCI bundles (a directory with
`config.json` and `rootfs/`).

```bash
# Create a container (fork shim, pause before exec)
pelagos create mycontainer /path/to/bundle

# Start it (signal shim to exec the process)
pelagos start mycontainer

# Query state (JSON output)
pelagos state mycontainer
# {"ociVersion":"1.0.2","id":"mycontainer","status":"running","pid":12345,...}

# Send a signal
pelagos kill mycontainer SIGTERM

# Clean up state directory
pelagos delete mycontainer
```

State is stored under `/run/pelagos/<id>/`. The shim double-forks so `pelagos create`
returns as soon as the container is in the "created" state.

---

## Rust Library API

For developers embedding Pelagos as a library in Rust programs.

### The Command Builder

```rust
use pelagos::container::{Command, Namespace, Stdio};

let mut child = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
    .with_proc_mount()
    .stdout(Stdio::Piped)
    .spawn()?;

let status = child.wait()?;
```

`spawn()` forks, runs `pre_exec` setup in the child (namespaces, mounts, capabilities,
seccomp), then execs the target binary. It returns a `Child` handle whose `wait()` and
`wait_with_output()` methods block until the container exits and clean up all resources.

### Interactive Shell

```rust
let session = Command::new("/bin/sh")
    .with_chroot("/path/to/rootfs")
    .with_namespaces(Namespace::UTS | Namespace::MOUNT)
    .with_proc_mount()
    .spawn_interactive()?;

// Blocks: relays stdin/stdout, forwards SIGWINCH, restores terminal on exit
let status = session.run()?;
```

### CLI-to-API Translation

| CLI flag | Rust API equivalent |
|---|---|
| `-i` | `spawn_interactive()?.run()` |
| `--rm` | Automatic -- `wait()` tears everything down |
| `--network none` | Default (no `.with_network()` call) |
| `--network bridge` | `.with_network(NetworkMode::Bridge)` |
| `--network pasta` | `.with_network(NetworkMode::Pasta)` |
| `-p 8080:80` | `.with_port_forward(8080, 80)` |
| `--dns 1.1.1.1` | `.with_dns(&["1.1.1.1"])` |
| `--bind /host:/ctr` | `.with_bind_mount("/host", "/ctr")` |
| `--bind-ro /host:/ctr` | `.with_bind_mount_ro("/host", "/ctr")` |
| `--tmpfs /run` | `.with_tmpfs("/run")` |
| `-v mydata:/data` | `.with_volume(&vol, "/data")` |
| `--read-only` | `.with_readonly_rootfs(true)` |
| `--memory 256m` | `.with_cgroup_memory(256 * 1024 * 1024)` |
| `--cpus 0.5` | `.with_cgroup_cpu_quota(50_000, 100_000)` |
| `--pids-limit 50` | `.with_cgroup_pids_limit(50)` |
| `--cpu-shares 512` | `.with_cgroup_cpu_shares(512)` |
| `--cap-drop ALL` | `.drop_all_capabilities()` |
| `--cap-add NET_ADMIN` | `.with_capabilities(&[Capability::NetAdmin])` |
| `--security-opt seccomp=default` | `.with_seccomp_default()` |
| `--security-opt no-new-privileges` | `.with_no_new_privileges(true)` |
| `--user 1000:1000` | `.with_uid(1000).with_gid(1000)` |
| `-e FOO=bar` | `.env("FOO", "bar")` |
| `-w /app` | `.with_cwd("/app")` |
| `--ulimit nofile=1024` | `.with_rlimit(Resource::NOFILE, 1024, 1024)` |
| `--hostname myhostname` | `.with_hostname("myhostname")` |
| `--masked-path /proc/kcore` | `.with_masked_paths(&["/proc/kcore"])` |
| `--sysctl net.ipv4.ip_forward=1` | `.with_sysctl("net.ipv4.ip_forward", "1")` |
| `alpine` (positional) | `.with_image_layers(layer_dirs)` |

---

## Advanced: Local Rootfs

For development or testing, you can bypass OCI images and use a local rootfs directory
directly. This is mainly useful for Pelagos contributors and custom rootfs builds.

```bash
# Build a rootfs from Docker:
scripts/build-rootfs-docker.sh       # requires Docker + sudo
# or from an Alpine tarball:
scripts/build-rootfs-tarball.sh      # requires sudo

# Register it with Pelagos:
sudo pelagos rootfs import alpine ./alpine-rootfs

# List registered rootfs entries:
pelagos rootfs ls

# Run with a local rootfs (advanced):
sudo pelagos run --rootfs alpine /bin/echo hello

# Remove a rootfs entry:
sudo pelagos rootfs rm alpine
```

See `docs/BUILD_ROOTFS.md` for detailed rootfs build instructions.

---

## Storage Layout

### Root (`sudo pelagos ...`)

```
/var/lib/pelagos/
  rootfs/<name>              symlink to imported rootfs directory
  volumes/<name>/            named volume data
  images/<ref>/              OCI image manifests and config
  layers/<sha256>/           content-addressable layer cache
  container_counter          monotonic counter for auto-naming

/run/pelagos/
  containers/<name>/
    state.json               container metadata and status
    stdout.log               captured stdout (detached mode)
    stderr.log               captured stderr (detached mode)
  <oci-id>/
    state.json               OCI lifecycle state
    exec.sock                sync socket (create/start handshake)
  next_ip                    IPAM counter for bridge networking
  nat_refcount               reference count for nftables NAT rules
  dns-<pid>-<n>/             per-container resolv.conf
```

### Rootless (`pelagos ...`)

```
~/.local/share/pelagos/       ($XDG_DATA_HOME/pelagos/)
  rootfs/<name>              symlink to imported rootfs directory
  volumes/<name>/            named volume data
  images/<ref>/              OCI image manifests and config
  layers/<sha256>/           content-addressable layer cache
  container_counter          monotonic counter for auto-naming

$XDG_RUNTIME_DIR/pelagos/     (fallback: /tmp/pelagos-$UID/)
  containers/<name>/
    state.json               container metadata and status
    stdout.log               captured stdout (detached mode)
    stderr.log               captured stderr (detached mode)
  dns-<pid>-<n>/             per-container resolv.conf
```

---

## Testing

Pelagos has several layers of tests. Unit tests and lint run without root; everything
else requires `sudo -E` to preserve the rustup/cargo environment.

### 1. Unit Tests + Lint (no root)

```bash
cargo test --lib
cargo clippy -- -D warnings
cargo fmt -- --check
```

53 unit tests covering parsers, builders, seccomp filter compilation, cgroup path
parsing, image manifest handling, and namespace flags.

### 2. Integration Tests (root)

```bash
sudo -E cargo test --test integration_tests
```

72 integration tests that exercise the full container lifecycle: spawn, namespaces,
mounts, seccomp, capabilities, cgroups, networking, overlay, exec, OCI lifecycle,
and rootless mode. Requires an Alpine rootfs (pulled automatically or via
`scripts/build-rootfs-docker.sh`).

### 3. E2E Test Suite (root)

```bash
sudo -E ./scripts/test-e2e.sh
```

End-to-end CLI tests covering `pelagos run`, `ps`, `stop`, `rm`, `logs`, `exec`,
detached mode, environment variables, bind mounts, volumes, bridge networking,
port forwarding, DNS, container linking, and security options. Builds the binary
and runs real containers.

### 4. Build E2E (root)

```bash
sudo -E ./scripts/test-build.sh
```

Tests `pelagos build` end-to-end: Remfile parsing, RUN steps with networking,
COPY, ENV, WORKDIR, CMD (JSON and shell form), multi-step builds, and image
tagging. Verifies that built images can be run.

### 5. Stress Tests (root)

```bash
sudo -E ./scripts/test-stress.sh
```

18 stress tests across 7 categories: rapid lifecycle, parallel containers,
resource-constrained containers, overlay filesystem stress, network stress,
OCI lifecycle stress, and edge cases.

### 6. Web Stack Example (root)

```bash
cargo build --release
sudo PATH=$PWD/target/release:$PATH ./examples/web-stack/run.sh
```

Builds and runs a 3-container blog stack (nginx → Bottle API → Redis) on bridge
networking with container linking. Runs 5 HTTP verification tests. Unlike the
other scripts, this one doesn't call `cargo build` internally — it expects
`pelagos` in your PATH, so pass the release binary path via `PATH` instead of `-E`.

### Full Pre-Release Checklist

```bash
# 1. Lint + unit tests (no root)
cargo test --lib && cargo clippy -- -D warnings && cargo fmt -- --check

# 2. Integration tests (root)
sudo -E cargo test --test integration_tests

# 3. E2E suites (root)
sudo -E ./scripts/test-e2e.sh
sudo -E ./scripts/test-build.sh
sudo -E ./scripts/test-stress.sh

# 4. Example app (root, release build)
cargo build --release
sudo PATH=$PWD/target/release:$PATH ./examples/web-stack/run.sh
```

---

## Troubleshooting

### "rootfs not found"

Pull an OCI image first:

```bash
pelagos image pull alpine
pelagos run alpine /bin/sh
```

Or if using a local rootfs, import it:

```bash
scripts/build-rootfs-docker.sh
sudo pelagos rootfs import alpine ./alpine-rootfs
sudo pelagos run --rootfs alpine /bin/sh
```

### Permission denied / EPERM

Try rootless mode first (no sudo needed for image pull, run, volumes). If a specific
feature requires root (bridge networking, cgroups), run with `sudo`.

### Rootless overlay fails

If you see an error about overlay mount failing:
- **Kernel 5.11+:** native overlay with `userxattr` should work automatically
- **Older kernels:** install `fuse-overlayfs` (see [Rootless Overlay](#rootless-overlay))
- Check that your filesystem supports user xattrs (`tmpfs`, `ext4`, `btrfs` all do)

### Integration tests fail

Tests require root and an Alpine rootfs:

```bash
sudo -E cargo test --test integration_tests
```

### Bridge networking: "No such device" or "Operation not permitted"

Bridge mode requires root, `ip` (iproute2), and `nft` (nftables). Ensure both are
installed and you're running as root.

### Pasta not found

Install [passt](https://passt.top) for rootless networking:

```bash
# Arch Linux
pacman -S passt
# Debian/Ubuntu
apt install passt
# Fedora
dnf install passt
```

### Container exec fails with "no container namespaces found"

The target container must be running. Check with `pelagos ps`.

### Stale networking state after crash

If a container or test run is killed without clean shutdown (e.g. `kill -9`, system
crash, interrupted test suite), nftables tables, iptables rules, and refcount files
may be left behind. Symptoms include containers failing to start networking, or
cleanup assertions failing in integration tests.

To reset:

```bash
# List and remove stale nftables tables
sudo nft list tables | grep pelagos
sudo nft delete table ip pelagos-pelagos0  # (or whatever table name)

# Clear stale refcount and port-forward files
sudo rm -f /run/pelagos/networks/*/nat_refcount /run/pelagos/networks/*/port_forwards

# Remove stale iptables FORWARD rules (if using iptables-nft)
sudo iptables -S FORWARD | grep pelagos  # inspect first
sudo iptables -D FORWARD -s 172.19.0.0/24 -j ACCEPT 2>/dev/null
sudo iptables -D FORWARD -d 172.19.0.0/24 -j ACCEPT 2>/dev/null
```
