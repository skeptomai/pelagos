# Compose Web Stack Example

The same 3-container blog stack as `examples/web-stack/`, but orchestrated
with `pelagos compose`. Two compose files are provided side by side:

| File | Format | Features |
|------|--------|---------|
| `compose.rem` | Static S-expressions | Declarative baseline |
| `compose.reml` | Lisp program | `define`, `env`, `on-ready` hooks |

`run.sh` uses `compose.reml` by default.

## Architecture

```
frontend (10.88.1.0/24):  proxy ←→ app
backend  (10.88.2.0/24):           app ←→ redis
```

| Service | Networks | Depends On | Ports | Role |
|---------|----------|------------|-------|------|
| **redis** | backend | — | — | Redis data store |
| **app** | frontend + backend | redis:6379 (TCP) | — | Python/Bottle REST API |
| **proxy** | frontend | app:5000 (TCP) | 8080→80 | nginx reverse proxy |

The proxy and redis share no network — isolation is enforced by topology, not
firewall rules.

## What `compose.reml` Adds

The `.reml` version is a Lisp program evaluated before the supervisor starts.
It produces the same `ComposeFile` as the static `.rem` but through code:

### `define` — named configuration

All tuneable values live at the top of the file:

```lisp
(define host-port 8080)
(define mem-redis "64m")
(define mem-app   "128m")
(define mem-proxy "32m")
(define cpu-app   "0.5")
```

No magic numbers buried inside service blocks. Change one line to retune the
whole stack.

### `env` — runtime configuration without file editing

The published host port is read from the environment at startup:

```lisp
(define host-port
  (let ((p (env "BLOG_PORT")))
    (if (null? p) 8080 (string->number p))))
```

```bash
# Run on a non-default port — no file edit required
BLOG_PORT=9090 sudo pelagos compose up -f compose.reml -p blog
```

### `on-ready` — observable tier transitions

Two hooks make startup sequencing visible in the log:

```lisp
(on-ready "redis"
  (lambda ()
    (log "redis: datastore layer ready — application tier starting")))

(on-ready "app"
  (lambda ()
    (log "app: application tier healthy — proxy starting")))
```

These fire after each service's TCP health check passes, immediately before
the next tier is allowed to start.

## Running

```bash
# Build pelagos first
cargo build --release
export PATH=$PWD/target/release:$PATH

# Run the demo (requires root)
sudo ./examples/compose/web-stack/run.sh
```

The script:
1. Pulls `alpine:latest` and builds the 3 images (from `examples/web-stack/` Remfiles)
2. Runs `pelagos compose up -f compose.reml` in foreground
3. Waits for the stack to accept connections on port 8080
4. Runs 5 verification tests (static page, health check, CRUD, persistence)
5. Tears down with `pelagos compose down -v`

## Comparison

| | `web-stack/run.sh` | `compose.rem` | `compose.reml` |
|---|---|---|---|
| Network setup | 6 commands | 2 declarations | 2 named constants + declarations |
| Port configuration | Hardcoded | Hardcoded | `$BLOG_PORT` env var with default |
| Startup visibility | `sleep` guards | Silent readiness polling | `on-ready` log messages |
| Resource limits | Hardcoded flags | Inline values | Named constants at top |
| Lines of orchestration | ~120 (bash) | ~45 | ~80 (with comments) |
