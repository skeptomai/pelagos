# Remora Lisp Executor Model

**Status:** Implemented (v0.13.x+)
**Location:** `src/lisp/runtime.rs`, `src/lisp/stdlib.lisp`

---

## Motivation

Static compose formats express *what* containers to run but not *how* they
relate at runtime.  Connection strings derived from assigned IPs, migration
steps that must complete before an app starts, health checks that gate
dependent services — none of these can be expressed declaratively.  They
require a programming language.

Remora's `.reml` format is that language.  This document describes the
executor model: how dependency graphs are declared, how data flows between
nodes, and how the two executors differ.

---

## Core Idea: Futures and Executors

The model separates two concerns:

- **Futures** — pure descriptions of work.  Nothing happens when a future is
  created.  A future is a first-class `Value` that can be passed to functions,
  stored in lists, and composed with `then`.

- **Executors** — policies for *when* and *how* to run the work.

```
.reml declaration         Value::Future in Lisp heap
      │                         │
      ▼                         ▼
(start svc)             { id, name, kind: Container, after: [] }
(define-then x f ...)   { id, name: "x", kind: Transform, after: [f] }
      │                         │
      ▼                         ▼
(run  (list ...))       static executor  — topo-sort, cycle detection, :parallel
(resolve future)        dynamic executor — recursive walk, monadic flatten
```

---

## The `define-*` Macro Family

These macros are the primary user-facing API.  They are defined in
`src/lisp/stdlib.lisp` and available in every `.reml` file without import.

### `(define-service var "name" opts...)`

Declares a service specification.  No container starts; this is purely a
data declaration.

```lisp
(define-service svc-db "db"
  :image   "postgres:16"
  :network "app-net"
  :env     ("POSTGRES_PASSWORD" . "secret")
           ("POSTGRES_DB"       . "appdb")
  :port    (5432 . 5432))
```

Options: `:image`, `:network`, `:env`, `:port`, `:bind`, `:bind-rw`,
`:tmpfs`, `:command`, `:memory`.

Multiple values under one keyword are written as dotted pairs (key–value)
or bare values (port numbers, memory strings).

### `(define-nodes (var svc) ...)`

Declares multiple lazy `start` nodes in one form.  Each `(var svc)` pair
expands to `(define var (start svc))`.  Nothing executes.

```lisp
(define-nodes
  (db    svc-db)
  (cache svc-cache))
```

Use `define-nodes` for independent services with no `:needs` or `:env`.
Services that require options use `(define name (start svc :needs ... :env
...))` directly.

### `(define-then name upstream (param) body...)`

Defines `name` as a Transform future whose value is computed from
`upstream`'s resolved value, with the result bound to `param`.

```lisp
(define-then db-url db (h)
  (format "postgres://app:secret@~a/appdb" (container-ip h)))
```

The future is named `"db-url"` (the binding name, not an auto-generated
string), so error messages reference `db-url` directly.

Expands to:
```lisp
(define db-url
  (then db (lambda (h) (format ...)) :name "db-url"))
```

### `(define-run [keywords...] (binding-name future-var) ...)`

Executes the graph **and** binds the results in one form.  Derives each
result key from `(symbol->string future-var)`, so no string literals are
needed.  Keywords (`:parallel`, `:max-parallel N`) are any non-list
arguments before the first binding pair.

```lisp
(define-run :parallel
  (db-handle    db)
  (cache-handle cache)
  (app-handle   app))
```

Expands to:
```lisp
(begin
  (define _run_result_ (run (list db cache app) :parallel))
  (define db-handle    (result-ref _run_result_ "db"))
  (define cache-handle (result-ref _run_result_ "cache"))
  (define app-handle   (result-ref _run_result_ "app")))
```

**Convention:** the future variable name must match the service name
(`db` → service `"db"`).  This is always true when using `define-nodes`.
If the names differ, use `run` + `define-results` directly.

### `(define-results results-var (var "key") ...)`

Destructures the alist returned by `run` into named bindings.  Use this
when you need custom binding names or a subset of results; otherwise prefer
`define-run`.

```lisp
(define-results results
  (db-handle    "db")
  (cache-handle "cache")
  (app-handle   "app"))
```

Expands to individual `(define var (result-ref results-var key))` forms.

---

## Primitive Functions

### `(start svc [:needs list] [:env lambda])`

Returns a `Future` — nothing starts.

- `:needs (list fut ...)` — futures that must resolve before this one
  starts; their values are passed positionally to the `:env` lambda
- `:env (lambda (dep1 dep2 ...) ...)` — called at execution time with
  resolved `:needs` values; must return a list of `("KEY" . value)` pairs
  merged into the container environment

```lisp
(define app (start svc-app
  :needs (list db-url cache-url)
  :env   (lambda (db-url cache-url)
           `(("DATABASE_URL" . ,db-url)
             ("CACHE_URL"    . ,cache-url)))))
```

The `:env` lambda receives the *resolved values* of its `:needs` futures
(strings, numbers, etc.) — not the futures themselves.

### `(then future lambda [:name "label"])`

Returns a Transform future that applies `lambda` to the resolved value of
`future`.  Automatically declares `:needs` the upstream.

The optional `:name "label"` overrides the auto-generated name
(`"<upstream>-then"`).  `define-then` uses this automatically; manual
`then` calls use the default.

```lisp
;; Raw form — future named "db-then"
(define db-url (then db (lambda (h) (format "..." (container-ip h)))))

;; Via define-then — future named "db-url"
(define-then db-url db (h)
  (format "..." (container-ip h)))
```

### `(then-all (list fut ...) lambda)`

Returns a Join future.  Waits for all listed futures, then calls `lambda`
with their resolved values in declaration order.  If the lambda returns a
`Future`, it is resolved automatically (same monadic flatten as `then`).

```lisp
(define both
  (then-all (list db-url cache-url)
    (lambda (db cache)
      (format "db=~a cache=~a" db cache))))
```

### `(run (list fut ...) [:parallel] [:max-parallel N])`

Static graph executor.

**The list is a *terminal* futures list** — the containers whose handles
you need after execution.  `run` walks each future's `:needs` transitively
to discover the full dependency graph automatically.  You do not need to
enumerate intermediate computed values.

Returns an alist of `("name" . resolved-value)` pairs for the explicitly
listed futures only.  Transitive dependencies are executed but not
surfaced.

```lisp
;; db-url and cache-url are discovered automatically from app's :needs.
(define results (run (list db cache app) :parallel))
```

Raises an error if a dependency cycle is detected (Kahn's algorithm).

**Modes:**
```lisp
(run (list db cache app))                    ; serial
(run (list db cache app) :parallel)          ; tier-parallel
(run (list db cache app) :parallel           ; capped at 4 simultaneous
     :max-parallel 4)
```

`:max-parallel N` implies `:parallel`.

**Threading model:** Transform and Join futures always run on the main
thread (their lambdas capture `Rc` values that are `!Send`).  Only raw
container spawns run in worker threads via `std::thread::spawn`.

### `(resolve future)`

Dynamic (monadic) executor.  Resolves `future` depth-first:

1. **Container future** — spawns the container; returns `ContainerHandle`.
2. **Transform future** — resolves the upstream, calls the lambda.  If
   the lambda returns another `Future`, resolves that too (monadic
   flatten).
3. **Plain value** — returned as-is.

A deduplication map prevents re-executing shared upstreams.

Does not perform upfront cycle detection and does not support `:parallel`.
Use for linear pipelines where each step's future is determined by the
previous step's value.

### `(result-ref results "name")`

Extracts a resolved value from a `run` alist by service name.  Raises an
error if the name is not found.

---

## Transitive Dependency Discovery

`run` automatically walks `:needs` recursively before executing.  You list
only the futures whose results you need; everything upstream is discovered
and executed in the correct order.

Given this graph:

```
db ──→ db-url ──→ migrate
db ──→ db-url ──→ app
cache ──→ cache-url ──→ app
```

You only list the containers you need handles for:

```lisp
(define results (run (list db cache app) :parallel))
```

`migrate` ran (it's a transitive dependency of `app` via `db-url`), but
its handle is not needed so it is not listed.  `db-url` and `cache-url`
are intermediate computations — they execute as needed but are not in the
output alist.

This keeps the terminal list as a statement of *intent* ("I need these
handles") rather than a manual transcription of the dependency graph.

---

## Execution Order

Given:

```lisp
(define-nodes
  (db    svc-db)
  (cache svc-cache))

(define-then db-url    db    (h) (format "postgres://...@~a/db"   (container-ip h)))
(define-then cache-url cache (h) (format "redis://~a:6379"        (container-ip h)))

(define migrate (start svc-migrate
  :needs (list db-url)
  :env   (lambda (url) `(("DATABASE_URL" . ,url)))))

(define app (start svc-app
  :needs (list db-url cache-url)
  :env   (lambda (db-url cache-url)
           `(("DATABASE_URL" . ,db-url)
             ("CACHE_URL"    . ,cache-url)))))

(define results (run (list db cache app) :parallel))
```

Serial execution order (one valid topological sort):

```
db → cache → db-url → cache-url → migrate → app
```

Parallel execution order:

```
Tier 1:  db ∥ cache                    (no dependencies)
Tier 2:  db-url ∥ cache-url            (unblocked after tier 1)
Tier 3:  migrate                        (unblocked after db-url)
Tier 4:  app                            (unblocked after migrate + cache-url)
```

No changes to the graph declaration are needed to switch between serial
and parallel.  `:parallel` is an execution-policy keyword, not a
structural one.

---

## Choosing an Executor

| Situation | Use |
|-----------|-----|
| Multiple independent services (db ∥ cache) | `run` |
| Upfront cycle detection | `run` |
| Parallel dispatch across tiers | `run :parallel` |
| Linear chain; next step depends on previous value | `resolve` |
| Short pipeline, no need to name intermediate futures | `resolve` |
| Static topology + one conditional branch | `run` — see below |
| Step-by-step with immediate results; no future graph | `container-start` |
| Parallel start without declarative graph | `container-start-bg` + `container-join` |

### Mixing static and conditional execution

A `then` lambda can return a `Future` instead of a plain value.  Both
executors detect this and resolve the returned future automatically
(**monadic flatten**).  This is the bridge between the static graph and
conditional branches: include the decision step in the `run` list like any
other future; its lambda chooses which container to start at runtime.

```lisp
(define migration-gate
  (then db-url
    (lambda (url)
      (if (need-migrations? url)
          (start svc-migrate :needs (list db-url)
                             :env   (lambda (u) `(("DATABASE_URL" . ,u))))
          (start svc-noop)))))

(define results (run (list db migration-gate app) :parallel))
```

Cycle detection covers the static portion.  The dynamic tail (the future
returned by the lambda) is structurally acyclic — a lambda cannot close
over a future that does not yet exist.

---

## Eager (Imperative) Execution

The graph model (`start`/`run`) is declarative: you describe *what* depends on
*what*, and the executor decides *when* to run each node.  The eager model is
imperative: you start containers by calling functions and receive handles
immediately, writing ordinary sequential or parallel Lisp code.

### `(container-start svc [:env list])`

Spawns a container synchronously and returns a `ContainerHandle`.  The optional
`:env` argument is a list of `(KEY . value)` pairs merged into the service env
before spawning — values are already-resolved strings or numbers (not lambdas).

```lisp
(define db  (container-start svc-db))
(define url (format "postgres://...@~a/db" (container-ip db)))
(define app (container-start svc-app :env (list (cons "DATABASE_URL" url))))
```

### `(container-start-bg svc [:env list])`

Spawns a container in a background thread and returns a `PendingContainer`
**immediately** — the calling thread is not blocked.  The same optional `:env`
argument is supported.

### `(container-join pending)`

Blocks until the background container finishes starting and returns a
`ContainerHandle`.  Raises an error if the start failed.  Calling `join` a
second time on the same pending handle raises `"already joined"`.

#### Sequential vs parallel eager

```lisp
;; Sequential — start db, wait, derive url, start app.
(define db  (container-start svc-db))
(define url (format "postgres://...@~a/db" (container-ip db)))
(define app (container-start svc-app :env (list (cons "DATABASE_URL" url))))

;; Parallel — kick off db and cache simultaneously.
(define db-p    (container-start-bg svc-db))
(define cache-p (container-start-bg svc-cache))
;; Both are starting.  Do other work here if desired.
(define db    (container-join db-p))
(define cache (container-join cache-p))
```

The parallel eager model restores the original intent of the async contract:
put work in motion, do other things, collect results later.  It does not
provide upfront cycle detection — that is a property of the graph model.

---

## Error Messages and Debugability

Future names appear in error messages.  The naming rules are:

| Form | Future name |
|------|-------------|
| `(define-service svc-db "db" ...)` + `(start svc-db)` | `"db"` |
| `(define-then db-url db ...)` | `"db-url"` (the binding name) |
| `(then upstream lambda)` (raw) | `"<upstream>-then"` |
| `(then upstream lambda :name "label")` | `"label"` |

`define-then` passes `(symbol->string name)` as `:name` at macro expansion
time, so the future's internal name matches the Lisp binding.  Errors
reference `"db-url"`, not `"db-then"`.

---

## Design Principles

**Futures are values.** A future is a first-class `Value::Future` in the
Lisp heap.  It can be passed to functions, stored in lists, and composed
with `then`.  There is no special syntax.

**The terminal list states intent.** `run` receives the futures whose
results you need, not the full graph.  Transitive discovery closes the gap
between "what I declared" and "what the executor needs to run."

**Data flow is typed.** `then` transforms produce typed values (strings,
numbers) rather than requiring callers to destructure raw handles.  `:env`
receives those typed values and wires them into containers.
`ContainerHandle` leaks only where needed (`container-ip`, `container-stop`).

**Executor policy is separate from graph structure.** The `.reml` file
declares what depends on what.  The executor decides when and how to run
it.  Swapping executors does not require changing the graph.

**Binding names surface in errors.** `define-then` names futures after
their Lisp bindings.  Error messages reference the user's vocabulary, not
generated internal names.

---

## Connection to π-Calculus

The model is a simplified π-calculus.  Processes communicate by sending
and receiving values on named channels; the dependency structure *is* the
communication pattern.

| π-calculus concept | Remora equivalent |
|--------------------|-------------------|
| Process | `start` future |
| Channel | Lisp name binding (`define`) |
| Send | Future resolving to a value |
| Receive / block | `:needs` dependency + `:env` |
| Value on channel | `ContainerHandle`, URL string, etc. |

`db-url` is not just a string — it is the value communicated between the
database future and the downstream futures that consume it.

---

## Roadmap

1. **Parallel `run`** — ✅ Implemented.  `:parallel` and `:max-parallel N`
   dispatch independent tiers concurrently via `std::thread`.
   Transform/Join futures remain on the main thread (they capture `Rc`);
   only container spawns run in workers.

2. **Streaming results** — expose a channel that futures push results onto
   as they complete, enabling reactive patterns (log a container as soon as
   it starts without waiting for the full graph).

3. **Cancellation** — if any future in a `run` call fails, SIGTERM all
   running containers from that execution set.  The interpreter `Drop` impl
   already handles this for the global registry; a per-`run` registry would
   scope it correctly.
