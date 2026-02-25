# Ongoing Tasks

## Current Task: Developer Stack Examples (Feb 24, 2026)

### Context

Build a suite of developer-oriented compose examples under `examples/compose/`,
each with a `Remfile` per service, a `compose.reml` demonstrating Lisp features,
a `run.sh` smoke test, and a `README.md`. All stacks use Alpine base images.

### Stack Backlog (priority order)

---

#### 1. `jupyter/` — JupyterLab + Redis
**Status:** COMPLETE ✅ (Feb 24, 2026)

**Architecture:**
```
network: jupyter-net (10.89.0.0/24)
  jupyterlab  — port 8888 → host
  redis       — notebook result caching / shared state
```

**Remfile notes:**
- Base: `FROM alpine:latest`
- APK: `python3 py3-pip py3-numpy py3-pandas py3-matplotlib py3-scipy py3-scikit-learn gcc g++ python3-dev musl-dev`
- PIP: `jupyterlab ipykernel redis` (pure Python redis client)
- CMD: `jupyter lab --ip=0.0.0.0 --port=8888 --no-browser --allow-root --NotebookApp.token='' --NotebookApp.password=''`
- EXPOSE 8888

**compose.reml features to demonstrate:**
- `env` fallback for `JUPYTER_PORT` (default 8888)
- `on-ready "redis"` hook: log "redis ready — notebook kernel can use redis cache"
- `define` for resource limits and port
- Named volume for `/root` to persist notebooks across restarts
- Bind-mount pattern for a `notebooks/` host directory

**Named volumes:** `jupyter-notebooks` (`/root`), or bind-mount to `./notebooks`

**run.sh smoke tests:**
- Curl `http://localhost:8888/api` → 200 JSON
- Curl `http://localhost:8888/lab` → 200 HTML containing "JupyterLab"
- Verify redis is reachable from jupyterlab container (exec `redis-cli ping`)

---

#### 2. `monitoring/` — Prometheus + Grafana + Loki  ⬅ CURRENT
**Status:** Not started

**Architecture:**
```
network: monitoring-net (10.89.1.0/24)
  prometheus  — port 9090, scrapes itself + node-exporter
  grafana     — port 3000, queries prometheus + loki; depends-on prometheus:9090
  loki        — port 3100, log aggregation; grafana depends-on loki:3100
```

**Remfile notes:**
- All three are Go binaries; APK packages: `prometheus grafana loki`
- Prometheus config bind-mounted from host (`prometheus.yml`)
- Grafana config: env vars for admin password, datasource provisioning
- Loki config: minimal local filesystem storage

**compose.reml features to demonstrate:**
- `env` for `GRAFANA_PASSWORD` with fallback `"admin"`
- `on-ready "prometheus"` → log "metrics backend ready"
- `on-ready "loki"` → log "log aggregation ready"
- `depends-on` chain: grafana waits for both prometheus:9090 and loki:3100
- `define` for all ports and memory limits
- Named volumes for Prometheus TSDB and Grafana state

**run.sh smoke tests:**
- `GET /api/health` on Grafana → 200 `{"database":"ok"}`
- `GET /-/ready` on Prometheus → 200
- `GET /ready` on Loki → 200
- Grafana datasource list API → both Prometheus and Loki configured

---

#### 3. `rust-builder/` — Rust build environment with sccache
**Status:** Not started

**Architecture:**
```
network: (none needed — single service)
  rust-builder — interactive build container
```

**Remfile notes:**
- Base: `FROM alpine:latest`
- APK: `rust cargo musl-dev build-base openssl-dev pkgconfig sccache`
- ENV: `RUSTC_WRAPPER=sccache`, `SCCACHE_DIR=/sccache-cache`
- cargo-chef installed at build time: `RUN cargo install cargo-chef`
- CMD: `cargo build --release` or interactive shell

**compose.reml features to demonstrate:**
- Named volumes for `cargo-registry` (`/root/.cargo/registry`) and `sccache-cache` (`/sccache-cache`)
- `env` for `SCCACHE_BUCKET` (optional S3 backend)
- Bind-mount for source code (`/workspace`)
- `define` for Rust edition and toolchain constraints

**run.sh smoke tests:**
- Container starts, `rustc --version` exits 0
- `sccache --show-stats` exits 0
- Build a minimal hello-world Rust project, assert exit 0
- Rebuild same project, assert sccache hit count > 0

---

#### 4. `node-dev/` — Node.js app with hot reload + PostgreSQL
**Status:** Not started

**Architecture:**
```
network: node-net (10.89.3.0/24)
  postgres   — port 5432 (internal only)
  node-app   — port 3000 → host; depends-on postgres:5432
```

**Remfile notes:**
- Node base: `FROM alpine:latest`; APK: `nodejs npm build-base python3 gcompat`
- `gcompat` for packages with precompiled glibc binaries
- Global: `npm install -g nodemon`
- Named volume for `node_modules` (prevents host/container platform conflicts)
- Source bind-mounted at `/app`

**compose.reml features to demonstrate:**
- `env` for `DATABASE_URL` constructed from service name
- Named volume for `node_modules` separating host and container module trees
- `on-ready "postgres"` hook: log "database ready — starting app"
- Bind-mount for live source reload

**run.sh smoke tests:**
- App container starts, `node --version` exits 0
- GET `http://localhost:3000/health` → 200
- Verify nodemon is watching (process list contains nodemon)

---

#### 5. `forgejo/` — Self-hosted Git (Forgejo + PostgreSQL)
**Status:** Not started

**Architecture:**
```
network: forgejo-net (10.89.4.0/24)
  postgres   — port 5432 (internal)
  forgejo    — port 3000 → host; SSH port 2222 → host; depends-on postgres:5432
```

**Remfile notes:**
- APK: `forgejo git git-lfs gnupg`
- PostgreSQL external DB mode (better than SQLite for multi-user)
- Config via env: `FORGEJO__database__DB_TYPE=postgres`, etc.
- Named volume for `/var/lib/gitea` (repositories + config)
- SSH server enabled: port 22 inside, 2222 on host

**compose.reml features to demonstrate:**
- Two `port` mappings (HTTP + SSH)
- `env` for all DB connection params
- `on-ready "postgres"` hook: log "database ready — starting Forgejo"
- Named volume for persistent repository storage
- `define` for DB name, user, host constants

**run.sh smoke tests:**
- GET `http://localhost:3000` → 200 (Forgejo install/login page)
- Forgejo healthcheck API → `{"status":"pass"}`
- Git clone over HTTP succeeds after initial setup

---

### Implementation Notes (all stacks)

- Each stack lives under `examples/compose/<name>/`
- Structure per stack:
  ```
  <name>/
    compose.reml        — Lisp compose program
    README.md           — architecture + usage + .reml features called out
    run.sh              — build images, compose up, smoke tests, teardown
    <service>/
      Remfile           — image definition
      [config files]    — nginx.conf, prometheus.yml, etc.
  ```
- Remfiles use `FROM alpine:latest` unless Alpine is genuinely not viable
- `run.sh` pattern mirrors `examples/compose/web-stack/run.sh`
- Each `compose.reml` must use at least: `define`, `env` with fallback, `on-ready`

---

## Completed: Lisp Interpreter for Remora (Feb 24, 2026)

**Branch:** `lisp-interpreter`

### Context

The compose DSL uses S-expressions as data, not code. As configs grow, users hit the
limits of a fixed schema: no loops, no abstraction, no variables. This task adds a real
Lisp interpreter that uses the existing S-expression parser as its reader and exposes
remora's compose model as first-class values. Old `.rem` files continue to work unchanged;
new `.reml` files are Lisp programs.

**Decisions made:**
- **Execution model**: Hybrid — `service`/`network`/`volume` return typed values;
  `compose` collects them into a spec; `compose-up` runs the spec; `on-ready` registers
  hooks that fire after a service becomes healthy.
- **File detection**: Extension-based. `.rem` = old format unchanged. `.reml` = Lisp.
  `compose up -f compose.reml` auto-dispatches. Default discovery: `compose.reml` first,
  then `compose.rem`.
- **Lisp scope**: Full Scheme subset — TCO, quasiquote/unquote-splicing, named let, do
  loops, R5RS-ish core (~55 builtins).

### Target `.reml` Syntax

```lisp
; Define a parameterized service template
(define (web-service name port)
  (service name
    (image "myapp:latest")
    (network "backend")
    (port port port)
    (depends-on (db :ready (port 5432)))))

; Scale out with map
(define services
  (map (lambda (pair)
         (web-service (car pair) (cadr pair)))
       '(("web" 8080) ("worker" 9090))))

; on-ready hook fires after db health check passes
(on-ready "db" (lambda ()
  (log "db is ready — starting app tier")))

; compose collects specs; compose-up runs them
(compose-up
  (compose
    (network "backend" (subnet "10.89.0.0/24"))
    (service "db"
      (image "postgres:16")
      (network "backend")
      (env "POSTGRES_PASSWORD" "secret"))
    services))   ; spliced list of ServiceSpec values
```

---

### Architecture

#### `src/lisp/value.rs` — Value type

```rust
pub type NativeFn = Rc<dyn Fn(&[Value]) -> Result<Value, LispError>>;

pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Symbol(String),
    Pair(Rc<(Value, Value)>),   // proper cons cells; lists are Pair-terminated-by-Nil
    Lambda { params: Params, body: Vec<SExpr>, env: Env },
    Native(String, NativeFn),
    // Remora domain values
    ServiceSpec(Box<remora::compose::ServiceSpec>),
    NetworkSpec(Box<remora::compose::NetworkSpec>),
    VolumeSpec(String),
    ComposeSpec(Box<ComposeFile>),
}

pub enum Params {
    Fixed(Vec<String>),              // (lambda (a b c) ...)
    Variadic(Vec<String>, String),   // (lambda (a b . rest) ...)
    Rest(String),                    // (lambda args ...)
}

pub struct LispError { pub message: String, pub line: usize, pub col: usize }
```

#### `src/lisp/env.rs` — Environment

```rust
pub type Env = Rc<RefCell<EnvFrame>>;

pub struct EnvFrame {
    bindings: HashMap<String, Value>,
    parent: Option<Env>,
}
// Methods: lookup, define, set (walks up for set!), child (new frame)
```

#### `src/lisp/eval.rs` — Evaluator with TCO

```rust
enum Step { Done(Value), Tail(SExpr, Env) }
fn eval_step(expr: SExpr, env: Env) -> Result<Step, LispError>

pub fn eval(expr: SExpr, env: Env) -> Result<Value, LispError> {
    let mut cur = (expr, env);
    loop {
        match eval_step(cur.0, cur.1)? {
            Step::Done(v)      => return Ok(v),
            Step::Tail(e, env) => cur = (e, env),
        }
    }
}
pub fn eval_apply(func: &Value, args: &[Value]) -> Result<Value, LispError>
```

Special forms: `quote`, `if`, `cond`, `when`, `unless`, `begin`, `define`, `set!`,
`lambda`, `let`, `let*`, `letrec`, named-`let`, `and`, `or`, `quasiquote`
(with `unquote`/`unquote-splicing`), `do` (desugars to named let).

Tail positions: `if` branches, last form of `begin`/`let`/`letrec`, last `cond` clause.

#### `src/lisp/builtins.rs` — ~55 standard functions

| Category | Functions |
|----------|-----------|
| Arithmetic | `+` `-` `*` `/` `quotient` `remainder` `modulo` `abs` `min` `max` `expt` |
| Comparison | `=` `<` `>` `<=` `>=` `equal?` `eqv?` `eq?` |
| Boolean | `not` `boolean?` |
| Pairs/Lists | `cons` `car` `cdr` `cadr` `caddr` `list` `null?` `pair?` `length` `append` `reverse` `list-ref` `iota` `assoc` |
| Higher-order | `map` `filter` `for-each` `apply` `fold-left` `fold-right` |
| Strings | `string?` `string-append` `string-length` `substring` `string->number` `number->string` `string-upcase` `string-downcase` `string=?` `string<?` |
| Symbols | `symbol?` `symbol->string` `string->symbol` |
| Type predicates | `number?` `procedure?` `list?` |
| I/O | `display` `newline` `error` |

#### `src/lisp/remora.rs` — Remora builtins + hook system

```rust
type HookMap = HashMap<String, Vec<Rc<dyn Fn() -> Result<(), LispError>>>>;

pub fn register_remora_builtins(env: &Env, hooks: Rc<RefCell<HookMap>>)
```

| Function | Returns |
|----------|---------|
| `(service name opts...)` | `Value::ServiceSpec` |
| `(network name opts...)` | `Value::NetworkSpec` |
| `(volume name)` | `Value::VolumeSpec` |
| `(compose items...)` | `Value::ComposeSpec` — flattens nested lists of specs |
| `(compose-up spec [project] [foreground?])` | Runs compose, fires hooks |
| `(on-ready "svc" lambda)` | Registers zero-arg hook closure |
| `(env "VAR")` | `Value::Str` or `Value::Nil` |
| `(log msg ...)` | `Value::Nil`; calls `log::info!` |

`on-ready` wraps the lambda value in a Rust closure `move || eval_apply(lambda, &[], env)`
and stores it in `HookMap` under the service name.

#### `src/lisp/mod.rs` — Interpreter

```rust
pub struct Interpreter {
    global_env: Env,
    hooks: Rc<RefCell<HookMap>>,
}
impl Interpreter {
    pub fn new() -> Self
    pub fn eval_file(&mut self, path: &Path) -> Result<Value, LispError>
    pub fn eval_str(&mut self, input: &str) -> Result<Value, LispError>
}
```

#### Hook integration in `src/cli/compose.rs`

Extract from `run_supervisor`:
```rust
pub fn run_compose_with_hooks(
    compose: &ComposeFile,
    compose_dir: &Path,
    project: &str,
    foreground: bool,
    on_ready: &HookMap,
) -> Result<(), Box<dyn std::error::Error>>
```

After `wait_for_dependency` passes and PID/IP recorded, fire hooks:
```rust
if let Some(hooks) = on_ready.get(svc_name) {
    for hook in hooks { hook()?; }
}
```

`.rem` path passes empty `HookMap` — zero behavioural change.

---

### Files To Create/Modify

| File | Change |
|------|--------|
| `src/lisp/mod.rs` | **NEW** |
| `src/lisp/value.rs` | **NEW** |
| `src/lisp/env.rs` | **NEW** |
| `src/lisp/eval.rs` | **NEW** |
| `src/lisp/builtins.rs` | **NEW** |
| `src/lisp/remora.rs` | **NEW** |
| `src/sexpr.rs` | Add `pub fn parse_all()` |
| `src/lib.rs` | Add `pub mod lisp;` |
| `src/cli/compose.rs` | `.reml` dispatch + `run_compose_with_hooks()` |
| `src/main.rs` | Default discovery: `compose.reml` before `compose.rem` |
| `tests/integration_tests.rs` | `test_lisp_compose_basic` |
| `docs/USER_GUIDE.md` | New `.reml` section |
| `docs/INTEGRATION_TESTS.md` | Document new test |

### Implementation Order

1. `src/sexpr.rs` — `parse_all()`
2. `src/lisp/value.rs` — Value, LispError, Params, NativeFn
3. `src/lisp/env.rs` — Env, EnvFrame
4. `src/lisp/eval.rs` — core evaluator, TCO, all special forms, quasiquote
5. `src/lisp/builtins.rs` — arithmetic, lists, strings, predicates
6. `src/lisp/mod.rs` — Interpreter, unit tests
7. **Checkpoint**: `cargo test --lib`
8. `src/lisp/remora.rs` — domain builtins + hook system
9. `src/cli/compose.rs` — extract `run_compose_with_hooks`, add `.reml` dispatch
10. `src/main.rs` — default file discovery
11. Tests + docs

### Verification

1. `cargo build` — clean
2. `cargo test --lib` — all pass
3. Manual: write `test.reml` using `define`/`lambda`/`map`, run `remora compose up -f test.reml`
4. Verify `(on-ready "db" ...)` fires at the right moment in logs
5. Verify existing `compose.rem` still works unchanged
6. Integration test: eval a `.reml` string, assert `ComposeSpec` structure

### Status

**COMPLETE.** All files created, `cargo build` + `cargo clippy -- -D warnings` + `cargo fmt`
+ `cargo test --lib` (205 tests) all pass. Two integration tests pass:
`test_lisp_compose_basic` and `test_lisp_evaluator_tco_and_higher_order`. Docs updated.

---

### Notes / Risks

- `Value` needs `Clone`; `Pair(Rc<...>)`, `Env(Rc<...>)`, `NativeFn(Rc<...>)` all clone cheaply.
- `unquote-splicing` into pair structure: build right-to-left with `cons`.
- `(compose ... services)` where `services` is a Lisp list of `ServiceSpec`: `compose` builtin flattens one level.
- Hooks survive fork (heap Rc closures in child process). Correct.
- `HookMap` pub-re-exported from `lisp::mod` so `cli::compose` doesn't need deep import path.
- All existing `.rem` compose tests unaffected — they use the old path exclusively.
