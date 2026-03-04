# Pelagos Tutorial: From Zero to Kubernetes (Sort Of)

A progressive guide that starts with one line and ends at the edge of the
production stack. Each section builds on the last. Skip ahead if you know
what you're doing; work through in order if you don't.

> **Prerequisites:** Pelagos installed, `sudo` available, an internet connection
> for image pulls.  Run `sudo ./scripts/setup.sh` once if you haven't already.

---

## Part 1 — Hello, Container

The simplest possible thing: run a command in an Alpine Linux container.

```bash
pelagos image pull alpine
sudo pelagos run alpine /bin/echo "hello from a container"
```

That's it.  The image was fetched from Docker Hub, its layers were stacked into
an overlay filesystem, and `/bin/echo` ran inside a Mount + UTS namespace with
its own isolated hostname.

**What's in a run?**

```bash
sudo pelagos run alpine /bin/sh -c "hostname && whoami && cat /etc/os-release"
```

You get the container's own hostname (e.g. `pelagos-5`), root (uid 0 inside
the container), and a real Alpine environment — all without Docker.  Pass
`--hostname mybox` to choose a name explicitly.

**Inspect what's running:**

```bash
# In one terminal:
sudo pelagos run alpine /bin/sleep 30 &

# In another:
sudo pelagos ps
sudo pelagos logs <name>
sudo pelagos stop <name>
```

---

## Part 2 — Building Your Own Image

Pelagos builds images from **Remfiles** — a Dockerfile dialect that uses `RUN`,
`COPY`, `FROM`, `ENV`, etc.

Create a project directory:

```
myapp/
  Remfile
  server.sh
```

`server.sh`:
```bash
#!/bin/sh
echo "Content-Type: text/plain"
echo ""
echo "Hello from pelagos! PID=$$  Host=$(hostname)"
```

`Remfile`:
```dockerfile
FROM alpine
RUN apk add --no-cache busybox-extras
COPY server.sh /usr/local/bin/server.sh
RUN chmod +x /usr/local/bin/server.sh
CMD ["/usr/local/bin/server.sh"]
```

Build and run:

```bash
pelagos build -t myserver:latest myapp/
sudo pelagos run myserver:latest
```

**Multi-stage build** — keep your images lean.

This is a separate example — create a new directory for it:

```
mygoapp/
  main.go
  Remfile
```

`main.go`:
```go
package main

import (
    "fmt"
    "os"
)

func main() {
    host, _ := os.Hostname()
    fmt.Printf("Hello from Go! PID=%d Host=%s\n", os.Getpid(), host)
}
```

`Remfile`:
```dockerfile
FROM alpine AS builder
RUN apk add --no-cache go
COPY . /src
WORKDIR /src
RUN go mod init mygoapp && CGO_ENABLED=0 go build -o /app .

FROM alpine
COPY --from=builder /app /app
CMD ["/app"]
```

`apk add go` needs internet access, so pass `--network bridge`:

```bash
pelagos build --network bridge -t mygoapp:latest mygoapp/
sudo pelagos run mygoapp:latest
# Hello from Go! PID=1  Host=pelagos-12
```

Pelagos handles the two-stage dance: builder's `/app` (a static binary,
`CGO_ENABLED=0`) is copied into the final image. Go, the build cache, and
all intermediate files stay in the builder stage and never reach the output.

---

## Part 3 — Isolation Deep Dive

Let's actually test the isolation rather than just trust it.

**Read-only rootfs — the container can't write to its own filesystem:**

```bash
sudo pelagos run --read-only alpine /bin/sh -c "echo test > /readonly.txt" || true
# exit 1 — write blocked
```

**Resource limits — cap memory at 64 MB:**

```bash
sudo pelagos run --memory 67108864 alpine /bin/sh -c \
  'dd if=/dev/zero bs=1M count=200 | cat > /dev/null'
# Killed by OOM at 64 MB
```

**Capabilities — drop everything, keep nothing:**

```bash
sudo pelagos run --network loopback --cap-drop ALL alpine /bin/sh -c \
  "id && ip link set lo mtu 1280 2>&1 || echo 'ip link set: denied'"
# uid=0(root) gid=0(root) groups=0(root)
# ip: ioctl 0x8922 failed: Operation not permitted
# ip link set: denied
```

Two things to notice: `--network loopback` gives the container its own network
namespace (without it the container sees all host interfaces). `ip link` alone
(read-only listing) never requires any capability — you must attempt a *mutating*
operation like setting the MTU to prove `CAP_NET_ADMIN` is gone.

**Seccomp — Docker's default profile out of the box:**

```bash
sudo pelagos run alpine /usr/bin/strace /bin/true 2>&1 | head -5
# strace fails — ptrace is blocked
```

**Networking modes:**

```bash
# Loopback only (default for most workloads)
sudo pelagos run --network loopback alpine /bin/sh -c "ping -c1 8.8.8.8 || echo 'no internet — good'"

# Full internet via pasta (rootless-compatible, no kernel bridge)
sudo pelagos run --network pasta alpine /bin/sh -c "wget -qO- https://example.com | head -5"

# Bridge with NAT and a port mapping
sudo pelagos run --network bridge --nat --port 8080:80 alpine \
  /bin/sh -c 'busybox httpd -f -p 80 -h /var/www &; sleep 5'
```

---

## Part 4 — Compose: Running a Stack

Real applications are more than one process.  Pelagos compose uses an
S-expression format that's more expressive than YAML.

**`stack.rem`** — a web app + Redis:

```lisp
(compose
  (network frontend)

  (service redis
    (image "redis:alpine")
    (network "frontend"))

  (service web
    (image "myserver:latest")
    (network "frontend")
    (depends-on (redis :ready-port 6379))
    (port "8080:80")
    (environment
      (REDIS_HOST "redis")
      (APP_ENV "production"))))
```

```bash
sudo pelagos compose up -f stack.rem
# Pelagos starts Redis first, waits for port 6379 to accept connections,
# then starts web.  DNS: "redis" resolves inside "web" automatically.

sudo pelagos compose ps -f stack.rem
sudo pelagos compose logs -f stack.rem
sudo pelagos compose down -f stack.rem
```

**Health-aware dependency:**

```lisp
(depends-on (db :ready-port 5432))
```

Pelagos polls TCP every 250 ms for up to 60 s before declaring the
dependency ready.  No `sleep 5` hacks.

---

## Part 5 — WebAssembly: No Kernel, No Problem

This is where Pelagos diverges from every other Linux runtime.

### 5.1 Run a Wasm module directly

```bash
# Compile a Rust program to WASI P1 (plain module)
cat > hello.rs << 'EOF'
fn main() {
    println!("Hello from Wasm! pid={}", std::process::id());
}
EOF

rustup target add wasm32-wasip1
rustc --target wasm32-wasip1 --edition 2021 -o hello.wasm hello.rs

# Run it — pelagos detects the \0asm magic bytes automatically
sudo pelagos run --wasm hello.wasm
```

No Alpine rootfs.  No kernel image loading.  The module runs directly via
an installed Wasm runtime (`wasmtime` or `wasmedge`) — or in-process if you
built Pelagos with `--features embedded-wasm`.

**Environment variables and bind mounts work exactly like Linux containers:**

```bash
sudo pelagos run --wasm \
  --env MY_VAR=hello \
  --bind /tmp/data:/data \
  hello.wasm
```

### 5.2 Build a Wasm OCI image

```bash
pelagos build -t my-wasm-app:latest - << 'EOF'
FROM scratch
COPY hello.wasm /hello.wasm
EOF

sudo pelagos image ls
# REPOSITORY        TAG     TYPE    SIZE
# my-wasm-app       latest  wasm    1.8 MB
```

The `TYPE` column shows `wasm` — the layer is stored with media type
`application/wasm` and the runtime knows to execute it without a Linux
environment.

```bash
sudo pelagos run my-wasm-app:latest
```

### 5.3 Wasm Component Model (P3b)

The Component Model (WASI Preview 2) gives you proper interfaces, typed
imports/exports, and composability.  Pelagos runs components natively with
the embedded wasmtime path.

```bash
rustup target add wasm32-wasip2
rustc --target wasm32-wasip2 --edition 2021 -o hello-component.wasm hello.rs

# Build and run as a component image
pelagos build -t my-component:latest - << 'EOF'
FROM scratch
COPY hello-component.wasm /hello.wasm
EOF

sudo pelagos image ls
# TYPE column shows: component  (media type: application/vnd.bytecodealliance.wasm.component.layer.v0+wasm)

sudo pelagos run my-component:latest
```

Pelagos auto-detects component vs plain module from bytes 4-7 of the binary
and routes to the correct runtime path — no flags needed.

### 5.4 The embedded path (no runtime in PATH)

Build Pelagos with `--features embedded-wasm` and you get in-process wasmtime
execution — zero subprocess overhead, no dependency on a system Wasm runtime:

```bash
cargo build --features embedded-wasm
sudo ./target/debug/pelagos run my-wasm-app:latest
# Runs in-process even with wasmtime stripped from PATH
```

Both P1 (plain module) and P2 (Component Model) binaries are supported on the
embedded path.

---

## Part 6 — The containerd Shim

> ⚠️ **Status: experimental.** The shim implements the correct protocol and
> has been code-reviewed, but has **not yet been tested with a live containerd
> deployment or under Kubernetes**.  Treat this section as a preview of what's
> coming.  PRs and field reports very welcome.

The shim lets containerd (and therefore Kubernetes) drive Pelagos's Wasm
execution path as a first-class runtime class.

### How it works

```
kubelet
  └─ containerd (CRI)
       └─ containerd-shim-pelagos-wasm-v1  (ttrpc)
            └─ pelagos::wasm::spawn_wasm()
                 └─ wasmtime / wasmedge subprocess
                      └─ your .wasm module
```

The shim speaks the containerd shim v2 protocol over ttrpc.  Containerd
creates a shim process per container; the shim parses the OCI bundle's
`config.json`, calls `spawn_wasm()`, and reports status back.

### Installation

```bash
cargo build --release
sudo cp target/release/pelagos-shim-wasm \
    /usr/local/bin/containerd-shim-pelagos-wasm-v1
```

Configure containerd (`/etc/containerd/config.toml`):

```toml
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.pelagos-wasm]
  runtime_type = "io.containerd.pelagos.wasm.v1"
```

```bash
sudo systemctl restart containerd
```

### Use from ctr (manual smoke test)

Before touching Kubernetes, validate with `ctr` directly:

```bash
# Pull your Wasm OCI image into containerd's store
sudo ctr image pull docker.io/myrepo/my-wasm-app:latest

# Create and start a container with the pelagos-wasm runtime
sudo ctr run \
  --runtime io.containerd.pelagos.wasm.v1 \
  docker.io/myrepo/my-wasm-app:latest \
  my-wasm-test

# Check state
sudo ctr task ls
sudo ctr task delete my-wasm-test
```

> This is the step we haven't exercised yet.  If you try it and it breaks,
> the most likely failure modes are: (a) wasmtime not in the containerd
> process's PATH — fix with a wrapper script or `Environment=` in the
> containerd systemd unit, or (b) the OCI config.json not having a
> `process.args` pointing at the `.wasm` file inside the bundle rootfs.

### Kubernetes RuntimeClass

Once `ctr` works, Kubernetes is just a RuntimeClass declaration away:

```yaml
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: pelagos-wasm
handler: pelagos-wasm
---
apiVersion: v1
kind: Pod
metadata:
  name: wasm-hello
spec:
  runtimeClassName: pelagos-wasm
  containers:
  - name: wasm
    image: myrepo/my-wasm-app:latest
    command: ["/hello.wasm"]
```

```bash
kubectl apply -f wasm-pod.yaml
kubectl logs wasm-hello
```

### Known limitations of the shim (today)

| Limitation | Impact |
|---|---|
| Uses subprocess path only | Requires wasmtime/wasmedge in containerd's PATH |
| No event publishing | Containerd won't receive async exit events |
| No `exec` handler | `kubectl exec` / `ctr task exec` unsupported |
| Single container per shim | No pod-level sandbox sharing |
| WASI P1 only via subprocess | Component Model needs embedded path (P3b) |

These are all tractable; they're on the roadmap in `ONGOING_TASKS.md` (#72 is
P3b — already done for the standalone path) and the CRI compliance doc.

---

## Part 7 — Putting it Together: Mixed Linux + Wasm Compose

Pelagos can run Linux and Wasm services side-by-side in the same compose stack.

```lisp
(compose
  (network app-net)

  ;; Linux service — standard OCI image
  (service postgres
    (image "postgres:alpine")
    (network "app-net")
    (environment
      (POSTGRES_PASSWORD "secret")
      (POSTGRES_DB "mydb")))

  ;; Wasm service — pure WASI module, no Linux image needed
  (service api-wasm
    (image "myrepo/api:wasm")
    (network "app-net")
    (depends-on (postgres :ready-port 5432))
    (environment
      (DB_URL "postgres://postgres:secret@postgres/mydb"))
    (port "3000:3000")))
```

```bash
sudo pelagos compose up -f mixed.rem
```

The Wasm service gets DNS resolution for `postgres`, port 3000 mapped to the
host, and env vars passed through WASI — all from the same orchestration layer
as the Linux service.

---

## What's Next

| Feature | Status |
|---|---|
| Linux containers | ✅ Full feature set |
| OCI image pull/build | ✅ |
| Wasm subprocess (wasmtime/wasmedge) | ✅ |
| Embedded Wasm P1 (plain module) | ✅ `--features embedded-wasm` |
| Embedded Wasm P2 (Component Model) | ✅ `--features embedded-wasm` |
| Mixed Linux+Wasm compose | 🔄 Basic (issue #70) |
| WASI P2 socket passthrough | 🔄 Issue #71 |
| containerd shim (local) | 🔧 Experimental — needs field testing |
| containerd shim under Kubernetes | 🔧 Needs ctr validation first |
| AppArmor / SELinux profiles | 📋 Issue #52 |
| CRIU checkpoint/restore | 📋 Issue #61 |

The gap between "compiles and implements the protocol" and "works reliably
under production Kubernetes" is real — the shim needs a live containerd
integration test before we can call it production-ready.  That's the honest
state of things.

---

## Quick-Reference Cheatsheet

```bash
# Images
pelagos image pull alpine
pelagos image ls
pelagos image rm alpine

# Run
sudo pelagos run alpine /bin/sh
sudo pelagos run --env FOO=bar --bind /data:/data alpine /bin/sh
sudo pelagos run --network pasta --port 8080:80 myserver:latest
sudo pelagos run --memory 134217728 --read-only alpine /bin/sh
sudo pelagos run --wasm hello.wasm          # Wasm module
sudo pelagos run my-wasm-app:latest         # Wasm OCI image

# Lifecycle
sudo pelagos ps
sudo pelagos logs <name>
sudo pelagos logs -f <name>
sudo pelagos stop <name>
sudo pelagos rm <name>

# Exec
sudo pelagos exec <name> /bin/sh
sudo pelagos exec -i <name> /bin/sh         # PTY

# Build
pelagos build -t myapp:latest .
pelagos build -t myapp:latest --no-cache .
pelagos build -t myapp:latest --build-arg VERSION=1.2 .

# Compose
sudo pelagos compose up -f stack.rem
sudo pelagos compose up -f stack.rem --foreground
sudo pelagos compose ps -f stack.rem
sudo pelagos compose logs -f stack.rem
sudo pelagos compose down -f stack.rem

# Networks & Volumes
sudo pelagos network create mynet
sudo pelagos network ls
sudo pelagos volume create myvol
sudo pelagos volume ls
```
