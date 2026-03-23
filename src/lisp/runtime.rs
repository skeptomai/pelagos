//! Imperative runtime builtins for the Lisp interpreter.
//!
//! Registered only when the interpreter is created via
//! [`super::Interpreter::new_with_runtime`].  Not available in plain `Interpreter::new()`.
//!
//! # Available functions
//!
//! | Function | Signature | Description |
//! |----------|-----------|-------------|
//! | `container-start`    | `(svc-spec [:env list])` → ContainerHandle | Spawn a container immediately |
//! | `container-start-bg` | `(svc-spec [:env list])` → PendingContainer | Spawn in background; returns immediately |
//! | `container-join`     | `(pending)` → ContainerHandle | Block until background container is ready |
//! | `start`            | `(svc-spec [:needs list] [:env lambda])` → Future | Declare a lazy container start; nothing runs |
//! | `then`           | `(future lambda)` → Future | Compute a value from a future's resolved result |
//! | `then-all`       | `((list fut...) lambda)` → Future | Join multiple futures, then compute |
//! | `run`              | `((list fut...) [:parallel] [:max-parallel N])` → alist | Execute graph; serial or tier-parallel |
//! | `resolve`          | `(future)` → value | Execute a monadic chain depth-first |
//! | `await`            | `(future [:port P] [:timeout T])` → ContainerHandle | Await a single Container future |
//! | `container-stop`   | `(handle)` → `()` | Send SIGTERM to a container |
//! | `container-wait`   | `(handle)` → Int | Wait for a container to exit |
//! | `container-run`    | `(svc-spec)` → Int | Start + wait; returns exit code |
//! | `container-ip`     | `(handle)` → Str\|Nil | Primary IP of container |
//! | `container-status` | `(handle)` → Str | `"running"` or `"exited"` |
//! | `await-port`       | `(host port [timeout-secs])` → Bool | TCP connect loop |
//!
//! ## Executor model
//!
//! `start` returns a [`Value::Future`] — a pure description of work (the
//! service spec) with no side effects.  Two executors are provided:
//!
//! - **`run`** — static graph executor.  Accepts a list of *terminal* futures
//!   (the ones whose results you care about); transitive `:needs` dependencies
//!   are discovered automatically and executed in the correct order.  Pass
//!   `:parallel` to run independent futures within each tier concurrently.
//!   Use `:max-parallel N` to cap threads per tier.  Returns an alist of
//!   `(name . resolved-value)` pairs for the explicitly listed futures only.
//!
//! - **`resolve`** — dynamic (monadic) executor.  Executes a single future
//!   depth-first: resolves all upstreams recursively before calling transforms.
//!   If a `then` lambda returns a new Future, that Future is resolved too
//!   (monadic flatten).  Use this for chains where the next step is only known
//!   after the previous one resolves.

use std::io::Read;
use std::net::TcpStream;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static FUTURE_ID: AtomicU64 = AtomicU64::new(1);

fn next_future_id() -> u64 {
    FUTURE_ID.fetch_add(1, Ordering::Relaxed)
}

use crate::compose::ServiceSpec;
use crate::container::{Command, Namespace, Stdio, Volume};
use crate::image;
use crate::network::NetworkMode;

use super::env::Env;
use super::value::{LispError, Value};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Register all imperative runtime builtins into `env`.
///
/// Called by [`super::Interpreter::new_with_runtime`].
pub fn register_runtime_builtins(
    env: &Env,
    registry: Arc<Mutex<Vec<(String, i32)>>>,
    thread_registry: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
    project: String,
    compose_dir: PathBuf,
) {
    let project = Rc::new(project);
    let compose_dir = Rc::new(compose_dir);

    // ── container-start ────────────────────────────────────────────────────
    // Immediately spawns a container and returns a ContainerHandle.
    // Optional :env applies a list of (KEY . value) pairs to the service env.
    {
        let registry = Arc::clone(&registry);
        let thread_registry = Arc::clone(&thread_registry);
        let project = Rc::clone(&project);
        let compose_dir = Rc::clone(&compose_dir);
        native(env, "container-start", move |args| {
            if args.is_empty() {
                return Err(LispError::new(
                    "container-start: expected (service-spec [:env list])",
                ));
            }
            let mut svc = extract_service_spec("container-start", &args[0])?;
            let mut i = 1;
            while i < args.len() {
                match &args[i] {
                    Value::Symbol(s) if s == ":env" => {
                        i += 1;
                        let env_list = args
                            .get(i)
                            .ok_or_else(|| LispError::new("container-start: :env requires a list"))?
                            .clone();
                        apply_inject_env(&mut svc, env_list, "container-start")?;
                        i += 1;
                    }
                    other => {
                        return Err(LispError::new(format!(
                            "container-start: unexpected argument: {}",
                            other
                        )))
                    }
                }
            }
            do_container_start(svc, &project, &compose_dir, &registry, &thread_registry)
        });
    }

    // ── container-start-bg ─────────────────────────────────────────────────
    // Spawns a container in a background thread and returns a PendingContainer
    // immediately.  Call (container-join pending) to block and get a handle.
    // Optional :env applies (KEY . value) pairs before spawning.
    {
        let registry = Arc::clone(&registry);
        let thread_registry = Arc::clone(&thread_registry);
        let project = Rc::clone(&project);
        let compose_dir = Rc::clone(&compose_dir);
        native(env, "container-start-bg", move |args| {
            if args.is_empty() {
                return Err(LispError::new(
                    "container-start-bg: expected (service-spec [:env list])",
                ));
            }
            let mut svc = extract_service_spec("container-start-bg", &args[0])?;
            let mut i = 1;
            while i < args.len() {
                match &args[i] {
                    Value::Symbol(s) if s == ":env" => {
                        i += 1;
                        let env_list = args
                            .get(i)
                            .ok_or_else(|| {
                                LispError::new("container-start-bg: :env requires a list")
                            })?
                            .clone();
                        apply_inject_env(&mut svc, env_list, "container-start-bg")?;
                        i += 1;
                    }
                    other => {
                        return Err(LispError::new(format!(
                            "container-start-bg: unexpected argument: {}",
                            other
                        )))
                    }
                }
            }
            let registry2 = Arc::clone(&registry);
            let thread_registry2 = Arc::clone(&thread_registry);
            let project_str = (*project).clone();
            let compose_dir_path = compose_dir.to_path_buf();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = do_container_start_inner(
                    svc,
                    &project_str,
                    &compose_dir_path,
                    &registry2,
                    &thread_registry2,
                )
                .map(|r| (r.name, r.pid, r.ip))
                .map_err(|e| e.message);
                let _ = tx.send(result);
            });
            use crate::lisp::value::{PendingRx, Value as V};
            let pending: PendingRx = std::sync::Arc::new(std::sync::Mutex::new(Some(rx)));
            Ok(V::PendingContainer(pending))
        });
    }

    // ── container-join ─────────────────────────────────────────────────────
    // Blocks until a background container (from container-start-bg) finishes
    // starting and returns a ContainerHandle.  Errors if already joined.
    native(env, "container-join", |args| {
        if args.len() != 1 {
            return Err(LispError::new(
                "container-join: expected 1 argument (pending-container)",
            ));
        }
        match &args[0] {
            Value::PendingContainer(arc) => {
                let rx = arc
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| LispError::new("container-join: already joined"))?;
                let (name, pid, ip) = rx
                    .recv()
                    .map_err(|_| LispError::new("container-join: background thread panicked"))?
                    .map_err(LispError::new)?;
                Ok(Value::ContainerHandle {
                    name,
                    pid,
                    ip,
                    deps: vec![],
                })
            }
            other => Err(LispError::new(format!(
                "container-join: expected pending-container, got {}",
                other.type_name()
            ))),
        }
    });

    // ── start ─────────────────────────────────────────────
    // Returns a Future — nothing starts.  Keywords:
    //   :needs  (list fut...)   ordering dependencies
    //   :env    (lambda ...)    called with resolved :needs values; returns
    //                           a list of (key . value) env pairs to merge
    native(env, "start", |args| {
        if args.is_empty() {
            return Err(LispError::new(
                "start: expected (svc [:needs list] [:env lambda])",
            ));
        }
        let svc = extract_service_spec("start", &args[0])?;
        let mut after: Vec<Value> = Vec::new();
        let mut inject: Option<Box<Value>> = None;
        let mut i = 1;
        while i < args.len() {
            match &args[i] {
                Value::Symbol(s) if s == ":needs" => {
                    i += 1;
                    let deps = args
                        .get(i)
                        .ok_or_else(|| LispError::new("start: :needs requires a list"))?
                        .to_vec()
                        .map_err(|_| LispError::new("start: :needs requires a list"))?;
                    for dep in deps {
                        match &dep {
                            Value::Future { .. } => after.push(dep),
                            other => {
                                return Err(LispError::new(format!(
                                    "start: :needs requires futures, got {}",
                                    other.type_name()
                                )))
                            }
                        }
                    }
                    i += 1;
                }
                Value::Symbol(s) if s == ":env" => {
                    i += 1;
                    let f = args
                        .get(i)
                        .ok_or_else(|| LispError::new("start: :env requires a lambda"))?
                        .clone();
                    match &f {
                        Value::Lambda { .. } | Value::Native(_, _) => {}
                        other => {
                            return Err(LispError::new(format!(
                                "start: :env requires a lambda, got {}",
                                other.type_name()
                            )))
                        }
                    }
                    inject = Some(Box::new(f));
                    i += 1;
                }
                other => {
                    return Err(LispError::new(format!(
                        "start: unexpected argument: {}",
                        other
                    )))
                }
            }
        }
        use crate::lisp::value::FutureKind;
        Ok(Value::Future {
            id: next_future_id(),
            name: svc.name.clone(),
            kind: FutureKind::Container {
                spec: Box::new(svc),
                inject,
            },
            after,
        })
    });

    // ── then ───────────────────────────────────────────────────────────────
    // (then upstream-future (lambda (v) computed-value) [:name "label"])
    // Creates a Transform future whose resolved value is the result of
    // applying the lambda to the upstream future's resolved value.
    // The new future declares :needs the upstream automatically.
    //
    // The optional :name "label" argument overrides the auto-generated name
    // ("<upstream>-then").  define-then uses this to name the future after
    // the Lisp binding, so error messages reference the user's variable name.
    native(env, "then", |args| {
        if args.len() < 2 {
            return Err(LispError::new(
                "then: expected (future transform-lambda [:name string])",
            ));
        }
        let upstream_name = match &args[0] {
            Value::Future { name, .. } => name.clone(),
            other => {
                return Err(LispError::new(format!(
                    "then: expected future, got {}",
                    other.type_name()
                )))
            }
        };
        match &args[1] {
            Value::Lambda { .. } | Value::Native(_, _) => {}
            other => {
                return Err(LispError::new(format!(
                    "then: expected lambda, got {}",
                    other.type_name()
                )))
            }
        }
        // Optional :name override.
        let name = if args.len() >= 4 {
            match (&args[2], &args[3]) {
                (Value::Symbol(k), Value::Str(n)) if k == ":name" => n.clone(),
                _ => format!("{}-then", upstream_name),
            }
        } else {
            format!("{}-then", upstream_name)
        };
        use crate::lisp::value::FutureKind;
        let upstream = args[0].clone();
        Ok(Value::Future {
            id: next_future_id(),
            name,
            kind: FutureKind::Transform {
                upstream: Box::new(upstream.clone()),
                transform: Box::new(args[1].clone()),
            },
            after: vec![upstream],
        })
    });

    // ── then-all ───────────────────────────────────────────────────────────
    // (then-all (list f1 f2 ...) (lambda (v1 v2 ...) result))
    // Creates a Join future: waits for all listed futures, then calls the
    // lambda with all their resolved values in order.  If the lambda returns
    // a Future it is flattened automatically (same rule as then).
    native(env, "then-all", |args| {
        if args.len() != 2 {
            return Err(LispError::new(
                "then-all: expected (list-of-futures lambda)",
            ));
        }
        let futures_list = args[0]
            .to_vec()
            .map_err(|_| LispError::new("then-all: first argument must be a list of futures"))?;
        let mut upstreams: Vec<Value> = Vec::new();
        let mut name_parts: Vec<String> = Vec::new();
        for f in futures_list {
            match &f {
                Value::Future { name, .. } => {
                    name_parts.push(name.clone());
                    upstreams.push(f);
                }
                other => {
                    return Err(LispError::new(format!(
                        "then-all: expected futures in list, got {}",
                        other.type_name()
                    )))
                }
            }
        }
        match &args[1] {
            Value::Lambda { .. } | Value::Native(_, _) => {}
            other => {
                return Err(LispError::new(format!(
                    "then-all: expected lambda, got {}",
                    other.type_name()
                )))
            }
        }
        use crate::lisp::value::FutureKind;
        Ok(Value::Future {
            id: next_future_id(),
            name: format!("join({})", name_parts.join(",")),
            kind: FutureKind::Join {
                transform: Box::new(args[1].clone()),
            },
            after: upstreams,
        })
    });

    // ── run ────────────────────────────────────────────────────────────
    // Graph-aware executor.  Topologically sorts futures by :needs, produces
    // tiers of independent futures, then executes serially or (with :parallel)
    // spawns threads for Container futures within each tier.
    //
    // Syntax:
    //   (run futures-list)                    ; serial (default)
    //   (run futures-list :parallel)           ; parallel tiers
    //   (run futures-list :parallel :max-parallel N) ; parallel, ≤N at once
    //   (run futures-list :max-parallel N)    ; :max-parallel implies :parallel
    //
    // Transform/Join futures always execute on the main thread (lambdas capture
    // Rc values which are !Send).  Only the raw container-spawn step runs in
    // worker threads; their results are converted to ContainerHandles on return.
    //
    // Deps not in the list are treated as already satisfied (resolved externally).
    {
        let registry = Arc::clone(&registry);
        let thread_registry = Arc::clone(&thread_registry);
        let project = Rc::clone(&project);
        let compose_dir = Rc::clone(&compose_dir);
        native(env, "run", move |args| {
            if args.is_empty() {
                return Err(LispError::new(
                    "run: expected (futures-list [:parallel] [:max-parallel N])",
                ));
            }
            let future_list = args[0]
                .to_vec()
                .map_err(|_| LispError::new("run: argument must be a list of futures"))?;

            // Parse optional keywords.
            let mut parallel = false;
            let mut max_parallel: Option<usize> = None;
            let mut ki = 1;
            while ki < args.len() {
                match &args[ki] {
                    Value::Symbol(s) if s == ":parallel" => {
                        parallel = true;
                        ki += 1;
                    }
                    Value::Symbol(s) if s == ":max-parallel" => {
                        ki += 1;
                        match args.get(ki) {
                            Some(Value::Int(n)) if *n > 0 => {
                                max_parallel = Some(*n as usize);
                                parallel = true;
                            }
                            _ => {
                                return Err(LispError::new(
                                    "run: :max-parallel requires a positive integer",
                                ))
                            }
                        }
                        ki += 1;
                    }
                    other => {
                        return Err(LispError::new(format!(
                            "run: unexpected argument: {}",
                            other
                        )))
                    }
                }
            }

            use super::eval::eval_apply;
            use crate::lisp::value::FutureKind;

            struct Entry {
                id: u64,
                kind: FutureKind,
                /// Dependency IDs extracted from the `after: Vec<Value>` field.
                after_ids: Vec<u64>,
            }

            // Validate the explicitly listed items up front.
            for v in &future_list {
                if !matches!(v, Value::Future { .. }) {
                    return Err(LispError::new(format!(
                        "run: expected futures, got {}",
                        v.type_name()
                    )));
                }
            }

            // Walk :needs transitively so the caller only needs to list terminal
            // futures; the full dependency graph is discovered automatically.
            fn collect_transitive(
                v: &Value,
                seen: &mut std::collections::HashSet<u64>,
                all: &mut Vec<Value>,
            ) {
                if let Value::Future { id, after, .. } = v {
                    if seen.insert(*id) {
                        for dep in after {
                            collect_transitive(dep, seen, all);
                        }
                        all.push(v.clone());
                    }
                }
            }

            let mut all_futures: Vec<Value> = Vec::new();
            let mut seen_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
            for v in &future_list {
                collect_transitive(v, &mut seen_ids, &mut all_futures);
            }

            let mut entries: Vec<Entry> = Vec::new();
            for v in all_futures {
                if let Value::Future {
                    id,
                    name,
                    kind,
                    after,
                } = v
                {
                    let after_ids = after.iter().filter_map(Value::future_id).collect();
                    entries.push(Entry {
                        id,
                        kind,
                        after_ids,
                    });
                    let _ = name; // name lives on the Value::Future; not needed in Entry
                }
            }

            // Tier-aware Kahn's topological sort.
            // Each tier contains futures that are independent of each other and
            // depend only on futures in earlier tiers — exactly the set that can
            // be dispatched in parallel.
            let n = entries.len();
            let id_to_idx: std::collections::HashMap<u64, usize> =
                entries.iter().enumerate().map(|(i, e)| (e.id, i)).collect();
            let mut in_degree = vec![0usize; n];
            let mut dependents: Vec<Vec<usize>> = vec![vec![]; n];
            for (i, e) in entries.iter().enumerate() {
                for dep_id in &e.after_ids {
                    if let Some(&dep_idx) = id_to_idx.get(dep_id) {
                        in_degree[i] += 1;
                        dependents[dep_idx].push(i);
                    }
                }
            }
            let mut ready: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
            let mut tiers: Vec<Vec<usize>> = Vec::new();
            while !ready.is_empty() {
                let tier = std::mem::take(&mut ready);
                for &i in &tier {
                    for &j in &dependents[i] {
                        in_degree[j] -= 1;
                        if in_degree[j] == 0 {
                            ready.push(j);
                        }
                    }
                }
                tiers.push(tier);
            }
            if tiers.iter().map(|t| t.len()).sum::<usize>() != n {
                return Err(LispError::new("run: dependency cycle detected"));
            }

            // Execution position of each future (tier * n + slot) — used to
            // compute reverse topological order for dep teardown lists.
            let exec_pos: std::collections::HashMap<u64, usize> = tiers
                .iter()
                .flatten()
                .enumerate()
                .map(|(pos, &idx)| (entries[idx].id, pos))
                .collect();

            // Execute tiers; track resolved values for inject/transform.
            let mut resolved: std::collections::HashMap<u64, Value> =
                std::collections::HashMap::new();

            if !parallel {
                // ── Serial path ──────────────────────────────────────────────
                // Identical to the previous implementation; tiers flatten to the
                // same deterministic topo order.
                for tier in &tiers {
                    for &idx in tier {
                        let e = &entries[idx];
                        let result = match &e.kind {
                            FutureKind::Container { spec, inject } => {
                                let mut spec = *spec.clone();
                                if let Some(inject_fn) = inject {
                                    let dep_vals: Vec<Value> = e
                                        .after_ids
                                        .iter()
                                        .map(|id| resolved.get(id).cloned().unwrap_or(Value::Nil))
                                        .collect();
                                    let env_list = eval_apply(inject_fn, &dep_vals)?;
                                    apply_inject_env(&mut spec, env_list, "run")?;
                                }
                                do_container_start(
                                    spec,
                                    &project,
                                    &compose_dir,
                                    &registry,
                                    &thread_registry,
                                )?
                            }
                            FutureKind::Transform {
                                upstream,
                                transform,
                            } => {
                                let upstream_id = upstream.future_id().unwrap_or(0);
                                let upstream_val =
                                    resolved.get(&upstream_id).cloned().unwrap_or(Value::Nil);
                                let result = eval_apply(transform, &[upstream_val])?;
                                // Monadic flatten: if the lambda returns a Future (conditional
                                // branch), resolve it dynamically so already-computed values
                                // are shared and not re-executed.  This is the bridge between
                                // static and dynamic execution.
                                match result {
                                    Value::Future { .. } => resolve_dynamic(
                                        result,
                                        &mut resolved,
                                        &project,
                                        &compose_dir,
                                        &registry,
                                        &thread_registry,
                                    )?,
                                    other => other,
                                }
                            }
                            FutureKind::Join { transform } => {
                                let upstream_vals: Vec<Value> = e
                                    .after_ids
                                    .iter()
                                    .map(|id| resolved.get(id).cloned().unwrap_or(Value::Nil))
                                    .collect();
                                let result = eval_apply(transform, &upstream_vals)?;
                                match result {
                                    Value::Future { .. } => resolve_dynamic(
                                        result,
                                        &mut resolved,
                                        &project,
                                        &compose_dir,
                                        &registry,
                                        &thread_registry,
                                    )?,
                                    other => other,
                                }
                            }
                        };
                        resolved.insert(e.id, result);
                    }
                }
            } else {
                // ── Parallel path ────────────────────────────────────────────
                // For each tier:
                //   Phase 1 (main thread): evaluate all lambdas (inject, transform,
                //     join).  Lambdas capture Rc values so they must stay on the
                //     main thread.  Container futures with inject produce a prepared
                //     ServiceSpec; Transform/Join futures produce their result directly.
                //   Phase 2 (worker threads): spawn prepared Container futures in
                //     parallel, at most max_parallel at a time.  Each thread gets
                //     owned data (ServiceSpec, String, PathBuf, Arc<Mutex<...>>).
                //   Results are merged in declaration order for a deterministic alist.

                let chunk_size = max_parallel.unwrap_or(0); // 0 = all at once

                for tier in &tiers {
                    let mut tier_results: Vec<(usize, Value)> = Vec::new();
                    let mut container_jobs: Vec<(usize, ServiceSpec)> = Vec::new();

                    // Phase 1: evaluate lambdas on main thread.
                    for &idx in tier {
                        let e = &entries[idx];
                        match &e.kind {
                            FutureKind::Container { spec, inject } => {
                                let mut spec = *spec.clone();
                                if let Some(inject_fn) = inject {
                                    let dep_vals: Vec<Value> = e
                                        .after_ids
                                        .iter()
                                        .map(|id| resolved.get(id).cloned().unwrap_or(Value::Nil))
                                        .collect();
                                    let env_list = eval_apply(inject_fn, &dep_vals)?;
                                    apply_inject_env(&mut spec, env_list, "run")?;
                                }
                                container_jobs.push((idx, spec));
                            }
                            FutureKind::Transform {
                                upstream,
                                transform,
                            } => {
                                let upstream_id = upstream.future_id().unwrap_or(0);
                                let upstream_val =
                                    resolved.get(&upstream_id).cloned().unwrap_or(Value::Nil);
                                let result = eval_apply(transform, &[upstream_val])?;
                                let result = match result {
                                    Value::Future { .. } => resolve_dynamic(
                                        result,
                                        &mut resolved,
                                        &project,
                                        &compose_dir,
                                        &registry,
                                        &thread_registry,
                                    )?,
                                    other => other,
                                };
                                tier_results.push((idx, result));
                            }
                            FutureKind::Join { transform } => {
                                let upstream_vals: Vec<Value> = e
                                    .after_ids
                                    .iter()
                                    .map(|id| resolved.get(id).cloned().unwrap_or(Value::Nil))
                                    .collect();
                                let result = eval_apply(transform, &upstream_vals)?;
                                let result = match result {
                                    Value::Future { .. } => resolve_dynamic(
                                        result,
                                        &mut resolved,
                                        &project,
                                        &compose_dir,
                                        &registry,
                                        &thread_registry,
                                    )?,
                                    other => other,
                                };
                                tier_results.push((idx, result));
                            }
                        }
                    }

                    // Phase 2: spawn container threads.
                    // Use one big chunk (fully parallel) unless max_parallel is set.
                    let effective_chunk = if chunk_size == 0 {
                        container_jobs.len().max(1)
                    } else {
                        chunk_size
                    };
                    for chunk in container_jobs.chunks(effective_chunk) {
                        let mut handles: Vec<(
                            usize,
                            std::thread::JoinHandle<Result<SpawnResult, LispError>>,
                        )> = Vec::new();

                        for (idx, spec) in chunk {
                            let idx = *idx;
                            let spec = spec.clone();
                            let project_owned = (*project).clone();
                            let compose_dir_owned = (*compose_dir).clone();
                            let registry_arc = Arc::clone(&registry);
                            let thread_registry_arc = Arc::clone(&thread_registry);
                            let handle = std::thread::spawn(move || {
                                do_container_start_inner(
                                    spec,
                                    &project_owned,
                                    &compose_dir_owned,
                                    &registry_arc,
                                    &thread_registry_arc,
                                )
                            });
                            handles.push((idx, handle));
                        }

                        for (idx, handle) in handles {
                            match handle.join() {
                                Ok(Ok(r)) => {
                                    let val = Value::ContainerHandle {
                                        name: r.name,
                                        pid: r.pid,
                                        ip: r.ip,
                                        deps: vec![],
                                    };
                                    tier_results.push((idx, val));
                                }
                                Ok(Err(e)) => return Err(e),
                                Err(_) => {
                                    return Err(LispError::new("run: a worker thread panicked"))
                                }
                            }
                        }
                    }

                    // Merge tier results in declaration order (deterministic alist).
                    tier_results.sort_by_key(|(idx, _)| *idx);
                    for (idx, val) in tier_results {
                        resolved.insert(entries[idx].id, val);
                    }
                }
            }

            // ── Post-execution: build output alist with deps ──────────────
            //
            // For each terminal Container future, collect its transitive
            // container dependencies in reverse execution order and attach
            // them as `deps` on the handle.  Transform/Join futures resolve
            // to plain values and carry no deps.
            //
            // "Transitive container deps" means: walk :needs recursively,
            // keep only nodes whose kind is Container, sort in reverse
            // execution order (latest-started first → stopped first).

            fn container_deps(
                id: u64,
                entries: &[Entry],
                id_to_idx: &std::collections::HashMap<u64, usize>,
                exec_pos: &std::collections::HashMap<u64, usize>,
                resolved: &std::collections::HashMap<u64, Value>,
            ) -> Vec<Value> {
                let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
                let mut stack = vec![id];
                let mut dep_ids: Vec<u64> = Vec::new();
                while let Some(cur) = stack.pop() {
                    if !visited.insert(cur) {
                        continue;
                    }
                    if let Some(&idx) = id_to_idx.get(&cur) {
                        for &dep_id in &entries[idx].after_ids {
                            stack.push(dep_id);
                        }
                        // Collect Container futures only (not self, not Transform/Join).
                        if cur != id {
                            if let Some(e) = id_to_idx
                                .get(&cur)
                                .map(|&i| &entries[i])
                                .filter(|e| matches!(e.kind, FutureKind::Container { .. }))
                            {
                                if resolved.contains_key(&e.id) {
                                    dep_ids.push(e.id);
                                }
                            }
                        }
                    }
                }
                // Reverse execution order: latest started is stopped first.
                dep_ids
                    .sort_by_key(|did| std::cmp::Reverse(exec_pos.get(did).copied().unwrap_or(0)));
                dep_ids
                    .into_iter()
                    .filter_map(|did| resolved.get(&did).cloned())
                    .collect()
            }

            let mut pairs: Vec<Value> = Vec::new();
            for v in &future_list {
                if let Value::Future { id, name, kind, .. } = v {
                    if let Some(resolved_val) = resolved.get(id) {
                        let final_val = if matches!(kind, FutureKind::Container { .. }) {
                            let deps =
                                container_deps(*id, &entries, &id_to_idx, &exec_pos, &resolved);
                            // Rebuild the handle with deps populated.
                            match resolved_val {
                                Value::ContainerHandle { name, pid, ip, .. } => {
                                    Value::ContainerHandle {
                                        name: name.clone(),
                                        pid: *pid,
                                        ip: ip.clone(),
                                        deps,
                                    }
                                }
                                other => other.clone(),
                            }
                        } else {
                            resolved_val.clone()
                        };
                        pairs.push(Value::Pair(Rc::new((Value::Str(name.clone()), final_val))));
                    }
                }
            }

            Ok(Value::list(pairs.into_iter()))
        });
    }

    // ── resolve ────────────────────────────────────────────────────────────
    // Dynamic (monadic) executor.  Resolves a single future chain recursively:
    //
    //   1. Container future   → spawn the container, return ContainerHandle.
    //   2. Transform future   → resolve the upstream first, then call the
    //      lambda.  If the lambda returns *another* Future, resolve that too
    //      (monadic flatten / Promise chaining).  Repeat until a non-Future
    //      value is produced.
    //
    // Unlike run-all, the full graph need not be declared upfront: the graph
    // emerges as lambdas execute.  Trade-off: no upfront cycle detection and
    // no tier-based parallel dispatch.  Use run via run when the full graph is
    // known; use resolve for linear pipelines or when the next step depends
    // on the runtime value of the previous one.
    {
        let registry = Arc::clone(&registry);
        let thread_registry = Arc::clone(&thread_registry);
        let project = Rc::clone(&project);
        let compose_dir = Rc::clone(&compose_dir);
        native(env, "resolve", move |args| {
            if args.len() != 1 {
                return Err(LispError::new("resolve: expected (future)"));
            }
            match &args[0] {
                Value::Future { .. } => {}
                other => {
                    return Err(LispError::new(format!(
                        "resolve: expected future, got {}",
                        other.type_name()
                    )))
                }
            }
            let mut resolved = std::collections::HashMap::new();
            resolve_dynamic(
                args[0].clone(),
                &mut resolved,
                &project,
                &compose_dir,
                &registry,
                &thread_registry,
            )
        });
    }

    // ── await ──────────────────────────────────────────────────────────────
    // Single-future serial executor.  Runs a Container future to completion,
    // optionally waiting for a TCP port.  Keywords: :port <int>, :timeout <num>.
    // Transform futures are not supported by await (use run-all or resolve).
    {
        let registry = Arc::clone(&registry);
        let thread_registry = Arc::clone(&thread_registry);
        let project = Rc::clone(&project);
        let compose_dir = Rc::clone(&compose_dir);
        native(env, "await", move |args| {
            if args.is_empty() {
                return Err(LispError::new(
                    "await: expected (future [:port P] [:timeout T])",
                ));
            }
            use crate::lisp::value::FutureKind;
            let svc = match &args[0] {
                Value::Future {
                    kind: FutureKind::Container { spec, .. },
                    ..
                } => *spec.clone(),
                Value::Future {
                    kind: FutureKind::Transform { .. } | FutureKind::Join { .. },
                    ..
                } => {
                    return Err(LispError::new(
                        "await: Transform and Join futures must be executed via run or resolve",
                    ))
                }
                other => {
                    return Err(LispError::new(format!(
                        "await: expected future, got {}",
                        other.type_name()
                    )))
                }
            };

            let mut port: Option<u16> = None;
            let mut timeout_secs = 60.0f64;
            let mut i = 1;
            while i < args.len() {
                match &args[i] {
                    Value::Symbol(s) if s == ":port" => {
                        i += 1;
                        port = Some(match args.get(i) {
                            Some(Value::Int(n)) => *n as u16,
                            _ => return Err(LispError::new("await: :port requires an integer")),
                        });
                        i += 1;
                    }
                    Value::Symbol(s) if s == ":timeout" => {
                        i += 1;
                        timeout_secs = match args.get(i) {
                            Some(Value::Int(n)) => *n as f64,
                            Some(Value::Float(f)) => *f,
                            _ => return Err(LispError::new("await: :timeout requires a number")),
                        };
                        i += 1;
                    }
                    other => {
                        return Err(LispError::new(format!(
                            "await: unexpected argument: {}",
                            other
                        )))
                    }
                }
            }

            let handle =
                do_container_start(svc, &project, &compose_dir, &registry, &thread_registry)?;

            if let Some(p) = port {
                let ip = match &handle {
                    Value::ContainerHandle { ip: Some(ip), .. } => ip.clone(),
                    _ => "127.0.0.1".to_string(),
                };
                let container_name = match &handle {
                    Value::ContainerHandle { name, .. } => name.clone(),
                    _ => "unknown".to_string(),
                };
                let addr = format!("{}:{}", ip, p);
                let deadline = Instant::now() + Duration::from_secs_f64(timeout_secs);
                loop {
                    if TcpStream::connect_timeout(
                        &addr.parse().map_err(|e| {
                            LispError::new(format!("await: invalid address '{}': {}", addr, e))
                        })?,
                        Duration::from_millis(250),
                    )
                    .is_ok()
                    {
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Err(LispError::new(format!(
                            "await: '{}' port {} did not open within {}s",
                            container_name, p, timeout_secs
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }

            Ok(handle)
        });
    }

    // ── container-stop ─────────────────────────────────────────────────────
    // Stops the container and cascades through its deps in reverse topo order.
    {
        let registry = Arc::clone(&registry);
        native(env, "container-stop", move |args| {
            if args.len() != 1 {
                return Err(LispError::new(
                    "container-stop: expected 1 argument (container-handle)",
                ));
            }
            stop_cascade(&args[0], &registry)?;
            Ok(Value::Nil)
        });
    }

    // ── container-wait ─────────────────────────────────────────────────────
    // Polls kill(pid, 0) until the process is gone, returns exit code (0),
    // then cascades container-stop through deps in reverse topo order.
    {
        let registry = Arc::clone(&registry);
        native(env, "container-wait", move |args| {
            if args.len() != 1 {
                return Err(LispError::new(
                    "container-wait: expected 1 argument (container-handle)",
                ));
            }
            let (_, pid) = extract_handle("container-wait", &args[0])?;
            loop {
                match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                    Err(nix::errno::Errno::ESRCH) => break,
                    Err(_) => break,
                    Ok(()) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            // Deregister the waited container (it exited naturally) and cascade
            // stop through its deps in reverse topo order.
            if let Value::ContainerHandle { name, deps, .. } = &args[0] {
                registry.lock().unwrap().retain(|(n, _)| n != name);
                for dep in deps {
                    stop_cascade(dep, &registry)?;
                }
            }
            Ok(Value::Int(0))
        });
    }

    // ── container-run ──────────────────────────────────────────────────────
    {
        let registry = Arc::clone(&registry);
        let thread_registry = Arc::clone(&thread_registry);
        let project = Rc::clone(&project);
        let compose_dir = Rc::clone(&compose_dir);
        native(env, "container-run", move |args| {
            if args.len() != 1 {
                return Err(LispError::new(
                    "container-run: expected 1 argument (service-spec)",
                ));
            }
            let svc = extract_service_spec("container-run", &args[0])?;
            let handle =
                do_container_start(svc, &project, &compose_dir, &registry, &thread_registry)?;
            let (name, pid) = extract_handle("container-run", &handle)?;
            // Wait for process to exit.
            loop {
                match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                    Err(nix::errno::Errno::ESRCH) => break,
                    _ => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            registry.lock().unwrap().retain(|(n, _)| n != &name);
            Ok(Value::Int(0))
        });
    }

    // ── container-ip ───────────────────────────────────────────────────────
    native(env, "container-ip", |args| {
        if args.len() != 1 {
            return Err(LispError::new(
                "container-ip: expected 1 argument (container-handle)",
            ));
        }
        match &args[0] {
            Value::ContainerHandle { ip, .. } => match ip {
                Some(s) => Ok(Value::Str(s.clone())),
                None => Ok(Value::Nil),
            },
            a => Err(LispError::new(format!(
                "container-ip: expected container, got {}",
                a.type_name()
            ))),
        }
    });

    // ── container-status ───────────────────────────────────────────────────
    native(env, "container-status", |args| {
        if args.len() != 1 {
            return Err(LispError::new(
                "container-status: expected 1 argument (container-handle)",
            ));
        }
        let (_, pid) = extract_handle("container-status", &args[0])?;
        let alive = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok();
        Ok(Value::Str(if alive { "running" } else { "exited" }.into()))
    });

    // ── await-port ─────────────────────────────────────────────────────────
    native(env, "await-port", |args| {
        if args.len() < 2 || args.len() > 3 {
            return Err(LispError::new(
                "await-port: expected (host port [timeout-secs])",
            ));
        }
        let host = match &args[0] {
            Value::Str(s) => s.clone(),
            a => {
                return Err(LispError::new(format!(
                    "await-port: expected string host, got {}",
                    a.type_name()
                )))
            }
        };
        let port = match &args[1] {
            Value::Int(n) => *n as u16,
            a => {
                return Err(LispError::new(format!(
                    "await-port: expected integer port, got {}",
                    a.type_name()
                )))
            }
        };
        let timeout_secs = if args.len() == 3 {
            match &args[2] {
                Value::Int(n) => *n as f64,
                Value::Float(f) => *f,
                a => {
                    return Err(LispError::new(format!(
                        "await-port: expected number timeout, got {}",
                        a.type_name()
                    )))
                }
            }
        } else {
            60.0
        };

        let addr = format!("{}:{}", host, port);
        let deadline = Instant::now() + Duration::from_secs_f64(timeout_secs);
        loop {
            if TcpStream::connect_timeout(
                &addr.parse().map_err(|e| {
                    LispError::new(format!("await-port: invalid address '{}': {}", addr, e))
                })?,
                Duration::from_millis(250),
            )
            .is_ok()
            {
                return Ok(Value::Bool(true));
            }
            if Instant::now() >= deadline {
                return Ok(Value::Bool(false));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });
}

// ---------------------------------------------------------------------------
// Dynamic executor
// ---------------------------------------------------------------------------

/// Recursively resolve `future` using a work-list deduplication map.
///
/// - Container futures spawn a container and return its [`Value::ContainerHandle`].
/// - Transform futures resolve their upstream first, then apply the lambda.
///   If the lambda returns another `Future`, that future is resolved too
///   (monadic flatten): this is what enables Promise-style chain syntax where
///   `(then ...)` lambdas return further `(container-start-async ...)` calls.
/// - Plain (non-Future) values are returned as-is.
///
/// The `resolved` map acts as a memo table: a future whose ID is already in
/// the map is not executed again, enabling shared upstreams without redundant
/// work.
fn resolve_dynamic(
    future: Value,
    resolved: &mut std::collections::HashMap<u64, Value>,
    project: &str,
    compose_dir: &std::path::Path,
    registry: &Arc<Mutex<Vec<(String, i32)>>>,
    thread_registry: &Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
) -> Result<Value, LispError> {
    use super::eval::eval_apply;
    use crate::lisp::value::FutureKind;

    match future {
        Value::Future {
            id, kind, after, ..
        } => {
            // Deduplication: if already resolved, return cached value.
            if let Some(cached) = resolved.get(&id) {
                return Ok(cached.clone());
            }

            // Resolve :needs deps first (needed for :env).
            let mut after_vals: Vec<Value> = Vec::new();
            for dep_fut in after {
                let val = resolve_dynamic(
                    dep_fut,
                    resolved,
                    project,
                    compose_dir,
                    registry,
                    thread_registry,
                )?;
                after_vals.push(val);
            }

            let result = match kind {
                FutureKind::Container { spec, inject } => {
                    let mut spec = *spec;
                    if let Some(inj) = inject {
                        let env_list = eval_apply(&inj, &after_vals)?;
                        apply_inject_env(&mut spec, env_list, "resolve")?;
                    }
                    do_container_start(spec, project, compose_dir, registry, thread_registry)?
                }
                FutureKind::Transform {
                    upstream,
                    transform,
                } => {
                    let upstream_val = resolve_dynamic(
                        *upstream,
                        resolved,
                        project,
                        compose_dir,
                        registry,
                        thread_registry,
                    )?;
                    let result = eval_apply(&transform, &[upstream_val])?;
                    // Monadic flatten: lambda may return a Future — resolve it.
                    match result {
                        Value::Future { .. } => resolve_dynamic(
                            result,
                            resolved,
                            project,
                            compose_dir,
                            registry,
                            thread_registry,
                        )?,
                        other => other,
                    }
                }
                FutureKind::Join { transform } => {
                    // after_vals holds resolved values for all upstreams in order.
                    let result = eval_apply(&transform, &after_vals)?;
                    match result {
                        Value::Future { .. } => resolve_dynamic(
                            result,
                            resolved,
                            project,
                            compose_dir,
                            registry,
                            thread_registry,
                        )?,
                        other => other,
                    }
                }
            };

            resolved.insert(id, result.clone());
            Ok(result)
        }
        // Plain value — already resolved, return as-is.
        other => Ok(other),
    }
}

// ---------------------------------------------------------------------------
// Core container-start logic
// ---------------------------------------------------------------------------

/// Thread-safe result of spawning a container.
///
/// Unlike [`Value::ContainerHandle`], this struct is `Send` (no `Rc` fields),
/// so it can be returned from worker threads in the parallel executor.
struct SpawnResult {
    name: String,
    pid: i32,
    ip: Option<String>,
}

/// Spawn a container and return a [`Value::ContainerHandle`].
///
/// This is a thin wrapper around [`do_container_start_inner`] for callers on
/// the main thread that need a `Value` directly.
fn do_container_start(
    svc: ServiceSpec,
    project: &str,
    compose_dir: &std::path::Path,
    registry: &Arc<Mutex<Vec<(String, i32)>>>,
    thread_registry: &Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
) -> Result<Value, LispError> {
    let r = do_container_start_inner(svc, project, compose_dir, registry, thread_registry)?;
    Ok(Value::ContainerHandle {
        name: r.name,
        pid: r.pid,
        ip: r.ip,
        deps: vec![],
    })
}

/// Core container spawn logic.  Returns a [`SpawnResult`] that is `Send`,
/// enabling use from worker threads in the parallel executor.
fn do_container_start_inner(
    svc: ServiceSpec,
    project: &str,
    compose_dir: &std::path::Path,
    registry: &Arc<Mutex<Vec<(String, i32)>>>,
    thread_registry: &Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
) -> Result<SpawnResult, LispError> {
    // Resolve image.
    let image_ref = &svc.image;
    let (_, manifest) = resolve_image(image_ref)?;
    let layers = image::layer_dirs(&manifest);
    if layers.is_empty() {
        return Err(LispError::new(format!(
            "container-start: service '{}': image has no layers",
            svc.name
        )));
    }
    let layer_dirs = layers.clone();

    // Determine command.
    let exe_and_args = if let Some(ref cmd) = svc.command {
        cmd.clone()
    } else {
        let mut cmd_vec = manifest.config.entrypoint.clone();
        cmd_vec.extend(manifest.config.cmd.clone());
        if cmd_vec.is_empty() {
            vec!["/bin/sh".to_string()]
        } else {
            cmd_vec
        }
    };
    let exe = &exe_and_args[0];
    let rest = &exe_and_args[1..];
    let container_name = format!("{}-{}", project, svc.name);

    let mut cmd = Command::new(exe).args(rest).with_image_layers(layers);

    // Apply image config env.
    for env_str in &manifest.config.env {
        if let Some((k, v)) = env_str.split_once('=') {
            cmd = cmd.env(k, v);
        }
    }

    // Apply image config workdir.
    if !manifest.config.working_dir.is_empty() && svc.workdir.is_none() {
        cmd = cmd.with_cwd(&manifest.config.working_dir);
    }

    // Apply image config user as default.
    if svc.user.is_none() && !manifest.config.user.is_empty() {
        if let Ok((uid, gid)) = parse_user_in_layers(&manifest.config.user, &layer_dirs) {
            cmd = cmd.with_uid(uid);
            if let Some(g) = gid {
                cmd = cmd.with_gid(g);
            }
        }
    }

    // Networks: service declares them; scope to project.
    let svc_network_names: Vec<String> = svc
        .networks
        .iter()
        .map(|n| scoped_network_name(project, n))
        .collect();

    // Ensure each network exists — create on demand, same as volumes.
    for net_name in &svc_network_names {
        crate::network::ensure_network(net_name).map_err(|e| {
            LispError::new(format!(
                "container-start: failed to ensure network '{}': {}",
                net_name, e
            ))
        })?;
    }

    if let Some(primary) = svc_network_names.first() {
        cmd = cmd.with_network(NetworkMode::BridgeNamed(primary.clone()));
    }
    for additional in svc_network_names.iter().skip(1) {
        cmd = cmd.with_additional_network(additional);
    }

    // NAT for internet access.
    if !svc_network_names.is_empty() {
        cmd = cmd.with_nat();
    }

    // Volumes.
    for vol in &svc.volumes {
        let scoped = format!("{}-{}", project, vol.name);
        let v = Volume::open(&scoped)
            .or_else(|_| Volume::create(&scoped))
            .map_err(|e| LispError::new(format!("container-start: volume '{}': {}", scoped, e)))?;
        cmd = cmd.with_volume(&v, &vol.mount_path);
    }

    // Bind mounts.
    for bm in &svc.bind_mounts {
        let host = if std::path::Path::new(&bm.host_path).is_relative() {
            compose_dir
                .join(&bm.host_path)
                .canonicalize()
                .map_err(|e| {
                    LispError::new(format!(
                        "container-start: bind-mount host path '{}': {}",
                        bm.host_path, e
                    ))
                })?
                .to_string_lossy()
                .into_owned()
        } else {
            bm.host_path.clone()
        };
        if bm.read_only {
            cmd = cmd.with_bind_mount_ro(&host, &bm.container_path);
        } else {
            cmd = cmd.with_bind_mount(&host, &bm.container_path);
        }
    }

    // tmpfs mounts.
    for path in &svc.tmpfs_mounts {
        cmd = cmd.with_tmpfs(path, "");
    }

    // Environment variables.
    for (k, v) in &svc.env {
        cmd = cmd.env(k, v);
    }
    let image_sets_path = manifest.config.env.iter().any(|e| e.starts_with("PATH="));
    if !image_sets_path {
        cmd = cmd.env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        );
    }

    // Port forwards.
    for port in &svc.ports {
        cmd = cmd.with_port_forward(port.host, port.container);
    }

    // Resource limits.
    if let Some(ref mem) = svc.memory {
        if let Ok(bytes) = parse_memory(mem) {
            cmd = cmd.with_cgroup_memory(bytes);
        }
    }
    if let Some(ref cpus) = svc.cpus {
        if let Ok((quota, period)) = parse_cpus(cpus) {
            cmd = cmd.with_cgroup_cpu_quota(quota, period);
        }
    }

    // User override.
    if let Some(ref u) = svc.user {
        if let Ok((uid, gid)) = parse_user_in_layers(u, &layer_dirs) {
            cmd = cmd.with_uid(uid);
            if let Some(g) = gid {
                cmd = cmd.with_gid(g);
            }
        }
    }

    // Workdir override.
    if let Some(ref w) = svc.workdir {
        cmd = cmd.with_cwd(w);
    }

    // ── Container hardening ──────────────────────────────────────────────────
    //
    // Applied last so we OR with any namespace flags already accumulated (e.g.
    // MOUNT from with_image_layers, NET from Loopback mode).

    // Namespaces: PID isolates the process tree so orphaned sub-processes
    // (e.g. postgres workers) are killed by the kernel when PID 1 exits.
    // UTS gives each container its own hostname; IPC isolates SysV/POSIX IPC.
    let ns = cmd.namespaces();
    cmd = cmd
        .with_namespaces(ns | Namespace::PID | Namespace::UTS | Namespace::IPC)
        .with_hostname(&container_name);

    // Security: seccomp + capabilities + no-new-privileges + masked paths.
    //
    // Start from DEFAULT_CAPS, apply (cap-drop ...) then (cap-add ...).
    // (cap-drop "ALL") zeros the baseline before any cap-add is applied.
    {
        use crate::container::Capability;
        let drop_all = svc.cap_drop.iter().any(|c| c.eq_ignore_ascii_case("ALL"));
        let mut effective = if drop_all {
            Capability::empty()
        } else {
            Capability::DEFAULT_CAPS
        };
        if !drop_all {
            for name in &svc.cap_drop {
                let n = name.to_uppercase().replace('-', "_");
                let n = n.trim_start_matches("CAP_");
                match Capability::from_name(n) {
                    Some(cap) => effective &= !cap,
                    None => log::warn!("cap-drop: unknown capability '{}' — skipping", name),
                }
            }
        }
        for name in &svc.cap_add {
            let n = name.to_uppercase().replace('-', "_");
            let n = n.trim_start_matches("CAP_");
            match Capability::from_name(n) {
                Some(cap) => effective |= cap,
                None => log::warn!("cap-add: unknown capability '{}' — skipping", name),
            }
        }
        cmd = cmd
            .with_seccomp_default()
            .with_capabilities(effective)
            .with_no_new_privileges(true)
            .with_masked_paths_default();
    }

    // Spawn with log capture.
    cmd = cmd
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped);

    let mut child = cmd.spawn().map_err(|e| {
        LispError::new(format!(
            "container-start: spawn '{}' failed: {}",
            svc.name, e
        ))
    })?;

    let pid = child.pid();
    let ip = child.container_ip();

    // Register DNS entries.
    let all_ips: Vec<(String, String)> = child
        .container_ips()
        .into_iter()
        .map(|(name, ip)| (name.to_string(), ip))
        .collect();

    for (net_name, ip_str) in &all_ips {
        let ip_addr: std::net::Ipv4Addr = match ip_str.parse() {
            Ok(ip) => ip,
            Err(_) => continue,
        };
        let net_def = match crate::network::load_network_def(net_name) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if let Err(e) = crate::dns::dns_add_entry(
            net_name,
            &svc.name,
            ip_addr,
            net_def.gateway,
            &["8.8.8.8".to_string(), "1.1.1.1".to_string()],
        ) {
            log::warn!(
                "container-start: dns: failed to register '{}' on {}: {}",
                svc.name,
                net_name,
                e
            );
        }
    }

    // Spawn log sink threads (discard output — no log files in imperative mode).
    let mut stdout_handle = child.take_stdout();
    let mut stderr_handle = child.take_stderr();
    let svc_name_log = svc.name.clone();

    let t_stdout = std::thread::spawn(move || {
        if let Some(mut src) = stdout_handle.take() {
            let mut buf = [0u8; 4096];
            while matches!(src.read(&mut buf), Ok(n) if n > 0) {}
        }
    });

    let t_stderr = std::thread::spawn(move || {
        if let Some(mut src) = stderr_handle.take() {
            let mut buf = [0u8; 4096];
            while matches!(src.read(&mut buf), Ok(n) if n > 0) {}
        }
    });

    // Spawn waiter that cleans up DNS when the container exits.
    let all_ips_wait = all_ips.clone();
    let t_waiter = std::thread::spawn(move || {
        let _ = child.wait();
        for (net_name, _) in &all_ips_wait {
            let _ = crate::dns::dns_remove_entry(net_name, &svc_name_log);
        }
    });

    // Register the waiter thread so Drop can join it before calling exit().
    // The waiter calls dns_remove_entry (which writes log messages), so we
    // must ensure it finishes before exit() flushes stderr.
    //
    // The log-sink threads (t_stdout, t_stderr) are intentionally NOT joined:
    // they block on read() until the container's write-end of the pipe closes,
    // which may not happen if the container forked children that inherited the
    // fd (e.g. postgres autovacuum workers that outlive the postmaster after
    // SIGKILL).  Since the log sinks never write to stderr they cannot cause
    // glibc's IO-lock deadlock; glibc's own _exit() will kill them at the end.
    drop(t_stdout);
    drop(t_stderr);
    thread_registry.lock().unwrap().push(t_waiter);

    // Register in interpreter's cleanup registry.
    registry.lock().unwrap().push((container_name.clone(), pid));

    log::info!(
        "container-start: '{}' started (pid {}, ip {:?})",
        container_name,
        pid,
        ip
    );

    Ok(SpawnResult {
        name: container_name,
        pid,
        ip,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Apply an `(inject ...)` result — a list of `(key . value)` pairs — to a
/// `ServiceSpec`'s env map.  Used in both the serial and parallel executor
/// paths to avoid duplicating the pair-parsing logic.
fn apply_inject_env(
    spec: &mut ServiceSpec,
    env_list: Value,
    caller: &str,
) -> Result<(), LispError> {
    for pair in env_list.to_vec()? {
        match pair {
            Value::Pair(p) => {
                let k = match &p.0 {
                    Value::Str(s) => s.clone(),
                    Value::Symbol(s) => s.clone(),
                    other => {
                        return Err(LispError::new(format!(
                            "{}: inject env key must be string, got {}",
                            caller,
                            other.type_name()
                        )))
                    }
                };
                let v = match &p.1 {
                    Value::Str(s) => s.clone(),
                    other => format!("{}", other),
                };
                spec.env.insert(k, v);
            }
            other => {
                return Err(LispError::new(format!(
                    "{}: inject must return (key . value) pairs, got {}",
                    caller,
                    other.type_name()
                )))
            }
        }
    }
    Ok(())
}

/// Stop a container and cascade through its `deps` in order.
///
/// Used by both `container-stop` and `container-wait` to implement
/// topology-aware teardown: stopping a terminal container automatically
/// stops everything it depended on, in reverse topological order.
fn stop_cascade(
    handle: &Value,
    registry: &Arc<Mutex<Vec<(String, i32)>>>,
) -> Result<(), LispError> {
    match handle {
        Value::ContainerHandle {
            name, pid, deps, ..
        } => {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(*pid),
                nix::sys::signal::Signal::SIGTERM,
            );
            registry.lock().unwrap().retain(|(n, _)| n != name);
            for dep in deps {
                stop_cascade(dep, registry)?;
            }
            Ok(())
        }
        other => Err(LispError::new(format!(
            "container-stop: expected container handle, got {}",
            other.type_name()
        ))),
    }
}

/// Register a native function closure into `env`.
fn native<F>(env: &Env, name: &str, f: F)
where
    F: Fn(&[Value]) -> Result<Value, LispError> + 'static,
{
    use super::value::NativeFn;
    env.borrow_mut().define(
        name,
        Value::Native(name.to_string(), Rc::new(f) as NativeFn),
    );
}

fn extract_service_spec(fn_name: &str, v: &Value) -> Result<ServiceSpec, LispError> {
    match v {
        Value::ServiceSpec(s) => Ok(*s.clone()),
        other => Err(LispError::new(format!(
            "{}: expected service-spec, got {}",
            fn_name,
            other.type_name()
        ))),
    }
}

fn extract_handle(fn_name: &str, v: &Value) -> Result<(String, i32), LispError> {
    match v {
        Value::ContainerHandle { name, pid, .. } => Ok((name.clone(), *pid)),
        other => Err(LispError::new(format!(
            "{}: expected container handle, got {}",
            fn_name,
            other.type_name()
        ))),
    }
}

/// Resolve an image reference to `(normalized_ref, manifest)`.
fn resolve_image(image_ref: &str) -> Result<(String, image::ImageManifest), LispError> {
    if let Ok(m) = image::load_image(image_ref) {
        return Ok((image_ref.to_string(), m));
    }
    let normalised = normalise_image_reference(image_ref);
    let m = image::load_image(&normalised).map_err(|e| {
        LispError::new(format!(
            "image '{}' not found locally (run 'pelagos image pull {}'): {}",
            image_ref, image_ref, e
        ))
    })?;
    Ok((normalised, m))
}

fn normalise_image_reference(r: &str) -> String {
    crate::image::normalise_reference(r)
}

/// Scope a network name to a project (mirrors the binary's `scoped_network_name`).
fn scoped_network_name(project: &str, net: &str) -> String {
    let name = format!("{}-{}", project, net);
    if name.len() > 12 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        let h = hasher.finish();
        format!("{}{:04x}", &name[..8], h as u16)
    } else {
        name
    }
}

/// Parse a `"128m"` / `"1g"` memory string to bytes.
fn parse_memory(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let (num, unit) = s
        .find(|c: char| c.is_alphabetic())
        .map(|i| (&s[..i], &s[i..]))
        .unwrap_or((s, ""));
    let base: i64 = num.parse().map_err(|_| format!("invalid memory: {}", s))?;
    let mult = match unit.to_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => return Err(format!("unknown memory unit: {}", unit)),
    };
    Ok(base * mult)
}

/// Parse a `"0.5"` / `"1.5"` CPU string to `(quota_us, period_us)`.
fn parse_cpus(s: &str) -> Result<(i64, u64), String> {
    let cpus: f64 = s
        .trim()
        .parse()
        .map_err(|_| format!("invalid cpus: {}", s))?;
    let period: u64 = 100_000;
    let quota = (cpus * period as f64) as i64;
    Ok((quota, period))
}

/// Parse a `"uid[:gid]"` or `"username"` string against the image layers.
fn parse_user_in_layers(
    user: &str,
    layer_dirs: &[std::path::PathBuf],
) -> Result<(u32, Option<u32>), String> {
    // Fast path: numeric uid[:gid]
    if let Some((uid_s, gid_s)) = user.split_once(':') {
        if let (Ok(uid), Ok(gid)) = (uid_s.parse::<u32>(), gid_s.parse::<u32>()) {
            return Ok((uid, Some(gid)));
        }
    }
    if let Ok(uid) = user.parse::<u32>() {
        return Ok((uid, None));
    }
    // Username lookup: search /etc/passwd in the top-most layer.
    for layer in layer_dirs.iter().rev() {
        let passwd = layer.join("etc/passwd");
        if let Ok(contents) = std::fs::read_to_string(&passwd) {
            for line in contents.lines() {
                let fields: Vec<&str> = line.split(':').collect();
                if fields.len() >= 4 && fields[0] == user {
                    let uid = fields[2].parse::<u32>().unwrap_or(0);
                    let gid = fields[3].parse::<u32>().ok();
                    return Ok((uid, gid));
                }
            }
        }
    }
    Err(format!("user '{}' not found in image", user))
}
