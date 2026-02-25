# Ongoing Tasks

## Current Task: Imperative Runtime Builtins (Feb 25, 2026)

### Context

Remora's `.reml` Lisp model adds imperative container orchestration on top of the
declarative `compose-up` model. After this task, a `.reml` file can directly call
`container-start`, `await-port`, `container-stop`, etc. instead of only using
`compose-up`.

### Scope

**Phase 1 — Language additions** (no runtime deps) ✅
- `(format fmt arg...)` — `~a` display / `~s` write style formatting → returns string
- `(sleep secs)` — thread sleep; accepts int or float → returns `()`
- `(guard (var clause...) body...)` — SRFI-34 error handling; catches `LispError`,
  binds message to `var`, dispatches clauses like `cond`, re-raises if no match
- `(with-cleanup cleanup-thunk body...)` — try/finally macro (stdlib.lisp)

**Phase 2 — `Value::ContainerHandle`** ✅
- New variant in `src/lisp/value.rs`: `ContainerHandle { name, pid, ip }`
- `type_name` → `"container"`, `is_truthy` → `true` (via wildcard), `Display` → `#<container name>`

**Phase 3 — `src/lisp/runtime.rs`** ✅
- New file in the **library crate** (not binary)
- Implements container spawning directly using `crate::image`, `crate::container`,
  `crate::network`, `crate::dns` — no cross-crate dependency needed
- Registers: `container-start`, `container-stop`, `container-wait`, `container-run`,
  `container-ip`, `container-status`, `await-port`
- `container-start` scopes network/volume names to project; spawns log sink + DNS
  waiter threads; registers in registry; returns `ContainerHandle`
- `container-wait` polls `kill(pid,0)` until ESRCH (waiter thread owns waitpid)

**Phase 4 — `Interpreter::new_with_runtime(project, compose_dir)`** ✅
- Added `container_registry: Rc<RefCell<Vec<(String, i32)>>>` field
- `new_with_runtime(project: String, compose_dir: PathBuf)` registers runtime builtins
- `Drop` impl: sends SIGTERM to all registry entries on interpreter drop

**Phase 5 — CLI update** ✅
- `cmd_compose_up_reml` uses `Interpreter::new_with_runtime(preliminary_project, compose_dir)`
- Handles purely imperative scripts (no `compose-up` call) by returning `Ok(())`

**Phase 6 — Tests + example** ✅
- 8 new unit tests: `test_format_*`, `test_sleep_*`, `test_guard_*`, `test_with_cleanup_*`
- `examples/compose/imperative/compose.reml` demonstrates the full API

### Files Modified

| File | Change |
|------|--------|
| `src/lisp/value.rs` | `ContainerHandle` variant |
| `src/lisp/mod.rs` | `container_registry`, `new_with_runtime`, `Drop`, `mod runtime` |
| `src/lisp/builtins.rs` | `format`, `sleep` |
| `src/lisp/eval.rs` | `guard` special form |
| `src/lisp/stdlib.lisp` | `with-cleanup` macro |
| `src/lisp/runtime.rs` | **NEW** — `register_runtime_builtins` + all imperative fns |
| `src/cli/compose.rs` | `cmd_compose_up_reml` uses `new_with_runtime`; handles no-pending case |
| `examples/compose/imperative/compose.reml` | **NEW** — imperative example |

### Verification

1. `cargo test --lib` — all 215+ existing tests pass; 8 new tests pass
2. `cargo clippy -- -D warnings` — clean
3. `cargo fmt --check` — clean

---

## Session Summary (Feb 24, 2026) — git SHA 2b9bbc6

### Completed this session

- **Dotted pair syntax** — `SExpr::DottedList`, full round-trip through macro
  expansion and `value_to_sexpr`; `define-service` handles both proper lists and
  dotted pairs in value position; variadic lambda/define shorthand now uses
  `DottedList` natively
- **monitoring/ stack** — Prometheus + Loki + Grafana compose example; all 6
  smoke tests pass; fixed Grafana startup (binary name, no ini file needed),
  fixed `image rm` to try local ref first before docker.io normalization
- **rust-builder/ stack** — Alpine + rustc + cargo + sccache; named volume mounts
  for cargo registry and sccache cache; 7 smoke tests pass including sccache
  cache activity; added `:volume`, `:bind`, `:bind-ro` Lisp service options
- **215 lib tests** pass; `cargo clippy -D warnings` and `cargo fmt` clean

### Remaining developer stack backlog

Next: **`node-dev/`** → then **`forgejo/`** (see detail below).

---

## Completed: `defmacro` + `define-service` + dotted pairs (Feb 24, 2026) ✅

### Context

Add a general macro system to the Lisp interpreter, then implement `define-service`
as a Lisp macro so service definitions are concise and keyword-driven:

```lisp
(define-service svc-jupyterlab "jupyterlab"
  (:image      "jupyter-jupyterlab:latest")
  (:network    "jupyter-net")
  (:depends-on "redis" 6379)
  (:env        "REDIS_HOST" "redis")
  (:port       jupyter-port 8888)
  (:memory     mem-jupyter)
  (:cpus       cpu-jupyter))
```

### Status

**COMPLETE.** All files created, `cargo build` + `cargo clippy -- -D warnings` + `cargo fmt`
+ `cargo test --lib` (205 tests) all pass. Two integration tests pass:
`test_lisp_compose_basic` and `test_lisp_evaluator_tco_and_higher_order`. Docs updated.

---

## Pending: Developer Stack Examples (Feb 24, 2026)

### Context

Build a suite of developer-oriented compose examples under `examples/compose/`,
each with a `Remfile` per service, a `compose.reml` demonstrating Lisp features,
a `run.sh` smoke test, and a `README.md`. All stacks use Alpine base images.

### Stack Backlog (priority order)

---

#### 4. `node-dev/` — Node.js app with hot reload + PostgreSQL  ⬅ NEXT
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

---

#### 5. `forgejo/` — Self-hosted Git (Forgejo + PostgreSQL)
**Status:** Not started

**Architecture:**
```
network: forgejo-net (10.89.4.0/24)
  postgres   — port 5432 (internal)
  forgejo    — port 3000 → host; SSH port 2222 → host; depends-on postgres:5432
```

---

### Implementation Notes (all stacks)

- Each stack lives under `examples/compose/<name>/`
- Remfiles use `FROM alpine:latest` unless Alpine is genuinely not viable
- `run.sh` pattern mirrors `examples/compose/web-stack/run.sh`
- Each `compose.reml` must use at least: `define`, `env` with fallback, `on-ready`
