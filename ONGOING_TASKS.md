# Ongoing Tasks

## Current Task: Domain-Oriented Orchestration API (Feb 26, 2026)

### Goal

Replace implementation-vocabulary names (`container-start-async`, `then`,
`then-all`, `run-all`, `:after`, `:inject`) with domain-vocabulary names
(`start`, `derive`, `derive-all`, `run`, `:needs`, `:env`). Remove the
session macros (`define-future`, `define-futures`, `define-transform`,
`define-results`, `define-run`) — the new API is clean enough to not need
them.

### Target API (what `compose.reml` looks like after)

```lisp
(define db       (start svc-db))
(define cache    (start svc-cache))

(define db-url    (derive db    (format "postgres://...@~a/appdb" (container-ip db))))
(define cache-url (derive cache (format "redis://~a:6379" (container-ip cache))))

(define migrate (start svc-migrate
  :needs (list db-url)
  :env   (lambda (url) `(("DATABASE_URL" . ,url)))))

(define app (start svc-app
  :needs (list db-url cache-url)
  :env   (lambda (db-url cache-url) `(("DATABASE_URL" . ,db-url)
                                      ("CACHE_URL"    . ,cache-url)))))

(define results (run (list db cache db-url cache-url migrate app) :parallel))
```

### Design decisions

- `start`   = `container-start-async` (renamed); `:after` → `:needs`, `:inject` → `:env`
- `derive`  = `then` (renamed); same signature `(derive node lambda)`
- `derive-all` = `then-all` (renamed)
- `run`     = `run-all` (renamed); takes explicit list — `(run (list ...) opts...)`
  keeping composability: the list is a first-class value, buildable at runtime
- Remove session macros from stdlib; `c[ad]+r`, `result-ref`, `assoc`,
  Result type, `with-cleanup` all stay
- Keep old names as aliases during transition; hard-remove after tests pass

### Files to change

| File | Change |
|------|--------|
| `src/lisp/runtime.rs` | Hard-rename builtins; update keyword parsing |
| `src/lisp/stdlib.lisp` | Remove `define-future/s`, `define-transform`, `define-results`, `define-run` |
| `src/lisp/mod.rs` | Update all tests using old names; remove tests for removed macros |
| `examples/compose/imperative/compose.reml` | Rewrite to new API |
| `examples/compose/imperative/compose-chain.reml` | Update to `derive`/`launch` |
| `docs/REML_EXECUTOR_MODEL.md` | Update API table and examples |
| `docs/USER_GUIDE.md` | Update orchestration section |

---

## Previous: stdlib Orchestration Macros (Feb 26, 2026) — SUPERSEDED

### Context

This session (Feb 26, 2026) shipped v0.13.0 and then spent time exploring
syntax-reduction macros for `.reml` compose files. The work is committed and
tests pass, but the overall design direction needs a rethink before going
further.

### What shipped this session (git SHA 2d8e5f0)

**v0.13.0 released** — tag pushed, GitHub Actions built static musl binaries
for x86_64 and aarch64, release published at:
https://github.com/skeptomai/remora/releases/tag/v0.13.0

**Documentation polish** (pre-release):
- `src/lisp/runtime.rs` doc comment: expanded function table to include
  `then`, `then-all`, `run-all [:parallel] [:max-parallel N]`, `resolve`;
  rewrote "Executor model" section to accurately describe the parallel
  tier executor rather than the old aspirational "future work" description.
- `examples/compose/imperative/compose.reml`: updated comments and `run-all`
  call to use `:parallel` (was still treating parallel as future work).
- `README.md`: added "Multi-Service Orchestration" section under Features,
  added compose row to comparison table, added `REML_EXECUTOR_MODEL.md`
  to the documentation index.

**Style preferences applied to compose.reml**:
- `(list ...)` for all-variable lists; `'(...)` for all-literal lists;
  quasiquote only for templates with mixed literal/runtime values.
- `(cons k v)` → dotted pair literal `(k . v)` style.

**stdlib orchestration macros** (all in `src/lisp/stdlib.lisp`):

| Macro | What it does |
|-------|-------------|
| `(define-future name svc)` | Binds `name-fut = (container-start-async svc)` |
| `(define-future name svc :after (p…) :inject body)` | `:after` params become `-fut` deps AND lambda params for the inject body |
| `(define-futures (name svc) …)` | Batch form of `define-future` |
| `(define-transform name upstream body…)` | `name-fut = (then upstream-fut (lambda (upstream) body…))` |
| `(define-results alist name …)` | Batch `(define name (result-ref alist "name"))` |
| `(define-run alist (binds…) (futures…) opts…)` | `run-all` + `define-results` in one form; appends `-fut` to future names |

**R7RS `c[ad]+r` family**: full set through 4 levels added to stdlib
(2-level forms and `caddr` remain as Rust builtins; everything else is stdlib).

**247 lib tests pass; clippy clean; fmt clean.**

---

### Open Design Question

The macros above were built incrementally in response to "this feels clunky"
observations on `compose.reml`. The result is functional and the example is
compact, but the overall approach needs a principled rethink before more macros
are added. Key tensions identified:

**Syntax reduction vs debuggability:**
- Generated names (`db-url-fut`, `migrate-fut`) don't appear in source; error
  messages referencing them require mental macro reversal.
- `define-future` with `:after/:inject` hides the lambda structure entirely —
  a runtime error inside the inject body points into generated code.
- `define-run` hides the full futures list; if `run-all` errors, the failing
  future name is not visible in the source form.

**Readability cost:**
- The convention (bare name → `-fut` suffix) is load-bearing and documented
  only in a comment block. New readers must understand it before the code
  makes sense.
- `define-future app svc-app :after (db-url cache-url) :inject \`(...)` is
  doing three things implicitly (dependency, lambda params, name mangling).

**What's worth keeping:**
- `define-futures` — thin, mechanical, the batch benefit is clear.
- `define-transform` — reads as a data-flow declaration; the name convention
  is predictable.
- `define-results` — thin batch extraction, no hidden logic.
- `c[ad]+r` family — standard, no controversy.

**What needs rethinking:**
- `define-future` with `:after/:inject` — too much implicit in one form.
- `define-run` — hides the futures list; may want explicit `-fut` names back.

**Possible directions to explore tomorrow:**
1. Drop `define-future :after/:inject` and `define-run`; keep the thin macros.
2. Keep them but require explicit `-fut` names in `:after` (no name mangling).
3. Introduce a different notation that makes the graph structure more visible
   (e.g. a dedicated graph DSL rather than function-call syntax).
4. Accept the current macros but improve error reporting so generated names
   map back to source forms.

---

## Parallel Execution in `run-all` — COMPLETE (Feb 25, 2026) ✅

### What shipped

- **Registry type change**: `container_registry: Rc<RefCell<...>>` → `Arc<Mutex<...>>`
  in `src/lisp/mod.rs` (field + Drop) and throughout `src/lisp/runtime.rs`.
  Zero performance regression in single-threaded code.

- **`SpawnResult` struct**: thread-safe (no `Rc`) result of container spawning.
  `do_container_start_inner` returns `SpawnResult`; `do_container_start` wraps
  it with `Value::ContainerHandle` for callers on the main thread.

- **`apply_inject_env` helper**: extracted from inject logic shared between
  serial and parallel paths.

- **Tier-aware Kahn's sort**: flat `Vec<usize>` → `Vec<Vec<usize>>` tiers.
  Serial path flattens tiers identically to the old order — fully backward-
  compatible.

- **Parallel path**: `:parallel` / `:max-parallel N` keywords on `run-all`.
  Phase 1 evaluates all lambdas on main thread (Rc-safe).  Phase 2 spawns
  `std::thread` for each container job in the tier; results collected,
  sorted to declaration order, merged into resolved map and alist output.

- **6 new unit tests**: keyword parsing, zero-max-parallel rejection, unknown
  keyword rejection, Transform futures in parallel mode.

- **`docs/REML_EXECUTOR_MODEL.md`**: executor table updated, `run-all` API
  section expanded, parallel execution order diagram added, roadmap updated.

**241 lib tests pass; clippy clean; fmt clean.**

---

## Previous: Dual Executor Model Complete (Feb 25, 2026) ✅

### Context

The previous session (Feb 25, 2026) completed the Future/Executor model for
declarative container orchestration. The session ended mid-design-discussion
about how `then` should behave when its lambda returns a Future (monadic bind
style, a.k.a. Promise chaining).

The open question: **static graph** (full graph declared upfront, topo-sortable
before any execution) vs **dynamic graph** (lazy unfolding — `then`'s lambda
is not called until its upstream resolves, so returned Futures are discovered at
runtime).

The user confirmed interest in the dynamic/monadic approach:
> "yes, I believe 'then's lambda would return a Future' describes it.
>  Let's play with that"
> "I have questions around dynamic resolution vs static upfront evaluation"

---

### What was completed this session (Feb 25, 2026)

All work is on `main`. No tag yet — waiting for design to stabilise.

#### stdlib quality-of-life macros

- `unless` — `(unless test body...)` → `(when (not test) body...)`
- `zero?` — `(zero? x)` → `(= x 0)`
- `logf` — `(logf fmt arg...)` → `(log (format fmt arg...))` (reduces `(log (format ...))` noise)
- `errorf` — `(errorf fmt arg...)` → `(error (format fmt arg...))` (same for errors)
- Updated all usages in stdlib and example files

#### Result type (stdlib.lisp)

Tagged list ADT, like Rust's `Result<T,E>`:

```lisp
(define (ok  v) (list 'ok  v))
(define (err r) (list 'err r))
(define (ok?  r) (and (pair? r) (eq? (car r) 'ok)))
(define (err? r) (and (pair? r) (eq? (car r) 'err)))
(define (ok-value  r) (cadr r))
(define (err-reason r) (cadr r))
```

#### `with-cleanup` updated signature

Cleanup lambda now receives a `Result` (not a zero-arg thunk):

```lisp
(defmacro with-cleanup (cleanup . body)
  `(guard (exn (#t (,cleanup (err exn)) (error exn)))
     (let ((result (begin ,@body)))
       (,cleanup (ok result))
       result)))
```

#### Self-evaluating keywords (eval.rs)

Symbols starting with `:` now evaluate to themselves (like Clojure keywords).
This was required for `:after`, `:inject`, `:port`, `:timeout` to work in
`container-start-async` / `await` calls without being looked up in the env.

```rust
// in eval.rs, atom eval branch:
if s.starts_with(':') {
    return Ok(Step::Done(Value::Symbol(s.clone())));
}
```

#### `Value::Future` and `FutureKind` (value.rs)

```rust
pub enum FutureKind {
    Container {
        spec:   Box<crate::compose::ServiceSpec>,
        inject: Option<Box<Value>>,          // Boxed to break recursive cycle
    },
    Transform {
        upstream_id: u64,
        transform:   Box<Value>,             // Boxed to break recursive cycle
    },
}

// Value::Future variant:
Future {
    id:    u64,
    name:  String,
    kind:  FutureKind,
    after: Vec<u64>,
}
```

#### `container-start-async`, `then`, `run-all`, `await` (runtime.rs)

- `container-start-async svc [:after list] [:inject lambda]` → `Value::Future`
- `then future lambda` → `Value::Future { kind: Transform }`, auto `:after` upstream
- `run-all (list fut ...)` → alist of `(name . resolved-value)`, Kahn topo-sort
- `await future [:port P] [:timeout T]` → `ContainerHandle`, errors on Transform futures

#### `result-ref` and `assoc` (stdlib.lisp)

- `assoc` — standard alist lookup
- `result-ref` — `(result-ref results "name")` extracts from `run-all` alist; errors if missing

#### examples/compose/imperative/compose.reml

Full graph model:
```lisp
(define db-url-fut
  (then db-fut
    (lambda (db)
      (format "postgres://app:secret@~a/appdb" (container-ip db)))))

(define app-fut
  (container-start-async svc-app
    :after  (list db-url-fut cache-url-fut)
    :inject (lambda (db-url cache-url)
              (list (cons "DATABASE_URL" db-url)
                    (cons "CACHE_URL"    cache-url)))))

(define results
  (run-all (list db-fut cache-fut db-url-fut cache-url-fut migrate-fut app-fut)))
```

#### docs/REML_EXECUTOR_MODEL.md (NEW)

Design doc covering: motivation, Futures/Executors model, π-calculus connection,
FutureKind API reference, execution order (serial vs parallel), design principles,
roadmap.

---

### Open Design Question: Static vs Dynamic `then`

**Current model (static):**
- `then`'s lambda returns a plain value (string, number, etc.)
- The entire graph is declared before `run-all` is called
- Topo-sort sees all futures upfront; cycle detection is complete
- Parallel executor could be added without changing `.reml` files

**Proposed model (dynamic/monadic):**
- `then`'s lambda returns a Future (or a plain value — the executor checks)
- When a Transform future resolves to another Future, that Future is added to
  the work queue dynamically
- Graph is discovered lazily: you don't see futures in `run-all` until their
  upstream completes

**Chain syntax the user envisions:**

```lisp
;; Monadic chain: db → migrate → app, with URL threading
(define pipeline
  (then db-fut
    (lambda (db)
      (let ((db-url (format "postgres://...@~a/appdb" (container-ip db))))
        (then (container-start-async svc-migrate
                :env (list (cons "DATABASE_URL" db-url)))
          (lambda (_)
            (container-start-async svc-app
              :env (list (cons "DATABASE_URL" db-url)))))))))
```

**Key trade-offs to discuss:**

| | Static graph | Dynamic (monadic) |
|---|---|---|
| Upfront cycle detection | ✅ yes | ❌ no |
| Parallel dispatch (known tiers) | ✅ yes | ⚠️ harder |
| `then-all` join (multi-upstream) | ✅ trivial | ⚠️ needs design |
| Chain syntax | ❌ no (names needed) | ✅ yes |
| Graph introspection | ✅ yes | ❌ not upfront |
| Incremental disclosure | ❌ no | ✅ yes |

**Proposed resolution:**
Support both — a `resolve` entry point that executes a single chain dynamically,
plus `run-all` for static graphs. The two can coexist: `run-all` remains the
preferred form for complex multi-service graphs where upfront analysis is valuable;
monadic `then` enables simple pipelines without requiring explicit `run-all`.

---

### What was completed (Feb 25, 2026)

Both executors now implemented and documented:

**Data model change:**
- `after: Vec<u64>` → `after: Vec<Value>` in `Value::Future` — futures now store
  their upstream futures as values, enabling recursive graph traversal without a
  registry
- `upstream_id: u64` → `upstream: Box<Value>` in `FutureKind::Transform` — same
  reason; allows `resolve` to walk chains recursively
- Added `Value::future_id() -> Option<u64>` helper for extracting IDs
- `run-all` topo-sort updated to extract IDs via `filter_map(Value::future_id)`

**`resolve` builtin added (runtime.rs):**
- Free function `resolve_dynamic` implements recursive depth-first execution
- Container futures: spawns container, returns `ContainerHandle`
- Transform futures: resolves upstream first, calls lambda; if result is a `Future`,
  resolves that too (monadic flatten)
- Deduplication map prevents re-executing shared upstreams

**New example:** `examples/compose/imperative/compose-chain.reml`
- Same 3-service stack as compose.reml but using monadic chain style
- Annotated to explain when to use `resolve` vs `run-all`

**Docs:** `docs/REML_EXECUTOR_MODEL.md` updated
- Comparison table: static vs dynamic
- `(resolve ...)` API reference with chain example
- "Choosing an Executor" section with decision guide

**Tests:** 232 passing, clippy clean, fmt clean

### Next steps

- Tag v0.13.0
- Add `then-all` join operator (future): `(then-all (list f1 f2) (lambda (v1 v2) ...))`
  for multi-upstream joins in the monadic style
- Developer stack examples: `node-dev/` and `forgejo/` (can now use imperative style)

---

## Previous Session: Imperative Runtime Builtins (Feb 25, 2026) ✅

### Completed

**Phase 1 — Language additions** ✅
- `(format fmt arg...)` — `~a` / `~s` formatting → string
- `(sleep secs)` — thread sleep; int or float → `()`
- `(guard (var clause...) body...)` — SRFI-34 error handling
- `(with-cleanup cleanup-thunk body...)` — try/finally stdlib macro

**Phase 2 — `Value::ContainerHandle`** ✅
- `ContainerHandle { name, pid, ip }` in `value.rs`

**Phase 3 — `src/lisp/runtime.rs`** ✅
- `container-start`, `container-stop`, `container-wait`, `container-run`,
  `container-ip`, `container-status`, `await-port`

**Phase 4 — `Interpreter::new_with_runtime(project, compose_dir)`** ✅
- `container_registry` field; `Drop` impl sends SIGTERM on interpreter drop

**Phase 5 — CLI update** ✅
- `cmd_compose_up_reml` uses `new_with_runtime`

**Phase 6 — Tests + example** ✅
- 8 new unit tests; `examples/compose/imperative/compose.reml`

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
