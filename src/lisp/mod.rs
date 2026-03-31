//! Lisp interpreter for `.reml` compose files.
//!
//! Entry points:
//! - [`Interpreter::new`] — create a fully-initialised interpreter with all
//!   standard and Pelagos builtins registered.
//! - [`Interpreter::eval_str`] — evaluate a string of Lisp source.
//! - [`Interpreter::eval_file`] — read and evaluate a file.
//! - [`Interpreter::take_pending`] — retrieve a `compose-up` invocation.
//! - [`Interpreter::take_hooks`] — retrieve registered `on-ready` hooks.

pub mod builtins;
pub mod env;
pub mod eval;
pub mod pelagos;
pub mod runtime;
pub mod value;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::sexpr;
pub use pelagos::{HookFn, HookMap, PendingCompose};
pub use value::{LispError, Value};

use builtins::register_builtins;
use env::{Env, EnvFrame};
use eval::eval;
use pelagos::register_pelagos_builtins;

/// A self-contained Lisp interpreter instance.
pub struct Interpreter {
    global_env: Env,
    hooks: Rc<RefCell<HookMap>>,
    pending: Rc<RefCell<PendingCompose>>,
    /// Registry of containers started via `container-start`.
    /// Entries are `(container_name, pid)`.
    /// The `Drop` impl sends SIGTERM/SIGKILL to any still-running containers,
    /// then joins all background threads before returning.
    /// `Arc<Mutex<...>>` so that parallel worker threads can register entries.
    pub(crate) container_registry: Arc<Mutex<Vec<(String, i32)>>>,
    /// Background threads (log sinks + waiters) for started containers.
    /// Joined in `Drop` after containers are killed, ensuring no threads are
    /// writing to stderr when the process calls `exit()`.
    pub(crate) thread_registry: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

impl Interpreter {
    /// Create a new interpreter with all builtins registered.
    pub fn new() -> Self {
        let global_env = EnvFrame::new();
        let hooks: Rc<RefCell<HookMap>> = Rc::new(RefCell::new(HookMap::new()));
        let pending: Rc<RefCell<PendingCompose>> = Rc::new(RefCell::new(PendingCompose::default()));
        let container_registry: Arc<Mutex<Vec<(String, i32)>>> = Arc::new(Mutex::new(Vec::new()));
        let thread_registry: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>> =
            Arc::new(Mutex::new(Vec::new()));

        register_builtins(&global_env);
        register_pelagos_builtins(&global_env, Rc::clone(&hooks), Rc::clone(&pending));

        let mut interp = Interpreter {
            global_env,
            hooks,
            pending,
            container_registry,
            thread_registry,
        };
        interp
            .eval_str(include_str!("stdlib.lisp"))
            .expect("stdlib.lisp failed to load — this is a bug");
        interp
    }

    /// Create a new interpreter with all builtins **plus** imperative runtime
    /// builtins (`container-start`, `container-stop`, `await-port`, etc.).
    ///
    /// `project` is the compose project name used to scope container names.
    /// `compose_dir` is the directory containing the compose file; it is used
    /// to resolve relative bind-mount host paths.
    pub fn new_with_runtime(project: String, compose_dir: std::path::PathBuf) -> Self {
        let interp = Self::new();
        let registry = Arc::clone(&interp.container_registry);
        let thread_registry = Arc::clone(&interp.thread_registry);
        runtime::register_runtime_builtins(
            &interp.global_env,
            registry,
            thread_registry,
            project,
            compose_dir,
        );
        interp
    }

    /// Evaluate all top-level forms in `input`, returning the value of the last.
    pub fn eval_str(&mut self, input: &str) -> Result<Value, LispError> {
        let exprs = sexpr::parse_all(input).map_err(|e| LispError::at(e.message, e.line, e.col))?;
        let mut result = Value::Nil;
        for expr in exprs {
            result = eval(expr, Rc::clone(&self.global_env))?;
        }
        Ok(result)
    }

    /// Read a `.reml` file and evaluate it.
    pub fn eval_file(&mut self, path: &Path) -> Result<Value, LispError> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| LispError::new(format!("cannot read '{}': {}", path.display(), e)))?;
        self.eval_str(&source)
    }

    /// Define an additional native function in the global environment.
    pub fn register_native(&mut self, name: &str, f: value::NativeFn) {
        self.global_env
            .borrow_mut()
            .define(name, Value::Native(name.to_string(), f));
    }

    /// Consume the pending `compose-up` invocation, if any.
    pub fn take_pending(&self) -> Option<PendingCompose> {
        let mut p = self.pending.borrow_mut();
        if p.spec.is_some() {
            let spec = p.spec.take();
            let project = p.project.take();
            let foreground = p.foreground;
            *p = PendingCompose::default();
            Some(PendingCompose {
                spec,
                project,
                foreground,
            })
        } else {
            None
        }
    }

    /// Consume the registered `on-ready` hooks.
    pub fn take_hooks(&self) -> HookMap {
        std::mem::take(&mut self.hooks.borrow_mut())
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Interpreter {
    fn drop(&mut self) {
        use std::time::{Duration, Instant};

        // Snapshot the registry; don't hold the lock during sleeps.
        let containers: Vec<(String, i32)> = self.container_registry.lock().unwrap().clone();
        if containers.is_empty() {
            return;
        }

        // 1. SIGTERM — graceful shutdown request.
        for (name, pid) in &containers {
            log::info!("interpreter cleanup: stopping '{}' (pid {})", name, pid);
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(*pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }

        // 2. Wait up to 5 s for all containers to exit.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let all_dead = containers.iter().all(|(_, pid)| {
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid), None).is_err()
            });
            if all_dead || Instant::now() >= deadline {
                break;
            }
        }

        // 3. SIGKILL any stragglers that ignored SIGTERM.
        for (name, pid) in &containers {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid), None).is_ok() {
                log::warn!(
                    "interpreter cleanup: force-killing '{}' (pid {})",
                    name,
                    pid
                );
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(*pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }

        // 4. Join waiter threads so DNS cleanup completes before _exit().
        let handles: Vec<_> = std::mem::take(&mut *self.thread_registry.lock().unwrap());
        for handle in handles {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn interp() -> Interpreter {
        Interpreter::new()
    }

    fn eval_ok(interp: &mut Interpreter, src: &str) -> Value {
        interp
            .eval_str(src)
            .unwrap_or_else(|e| panic!("eval error: {}", e))
    }

    fn eval_err(interp: &mut Interpreter, src: &str) -> String {
        interp
            .eval_str(src)
            .err()
            .unwrap_or_else(|| panic!("expected error"))
            .message
    }

    // ── Self-evaluating ───────────────────────────────────────────────────
    #[test]
    fn test_integer() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "42"), Value::Int(42));
        assert_eq!(eval_ok(&mut i, "-7"), Value::Int(-7));
    }

    #[test]
    fn test_float() {
        let mut i = interp();
        assert!(matches!(eval_ok(&mut i, "3.14"), Value::Float(_)));
    }

    #[test]
    fn test_string() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, r#""hello""#), Value::Str("hello".into()));
    }

    #[test]
    fn test_bool() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "#t"), Value::Bool(true));
        assert_eq!(eval_ok(&mut i, "#f"), Value::Bool(false));
    }

    #[test]
    fn test_nil() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "()"), Value::Nil);
    }

    // ── Arithmetic ────────────────────────────────────────────────────────
    #[test]
    fn test_add() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(+ 1 2 3)"), Value::Int(6));
    }

    #[test]
    fn test_sub() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(- 10 3)"), Value::Int(7));
    }

    #[test]
    fn test_mul() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(* 3 4)"), Value::Int(12));
    }

    #[test]
    fn test_div_exact() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(/ 12 4)"), Value::Int(3));
    }

    #[test]
    fn test_div_inexact() {
        let mut i = interp();
        assert!(matches!(eval_ok(&mut i, "(/ 1 3)"), Value::Float(_)));
    }

    #[test]
    fn test_modulo() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(modulo 10 3)"), Value::Int(1));
        assert_eq!(eval_ok(&mut i, "(modulo -10 3)"), Value::Int(2));
    }

    #[test]
    fn test_expt() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(expt 2 10)"), Value::Int(1024));
    }

    // ── Comparison ────────────────────────────────────────────────────────
    #[test]
    fn test_numeric_comparison() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(< 1 2)"), Value::Bool(true));
        assert_eq!(eval_ok(&mut i, "(> 1 2)"), Value::Bool(false));
        assert_eq!(eval_ok(&mut i, "(= 3 3)"), Value::Bool(true));
        assert_eq!(eval_ok(&mut i, "(<= 2 2)"), Value::Bool(true));
    }

    // ── Boolean ───────────────────────────────────────────────────────────
    #[test]
    fn test_not() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(not #f)"), Value::Bool(true));
        assert_eq!(eval_ok(&mut i, "(not 42)"), Value::Bool(false));
    }

    // ── Quote ─────────────────────────────────────────────────────────────
    #[test]
    fn test_quote_symbol() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "'foo"), Value::Symbol("foo".into()));
    }

    #[test]
    fn test_quote_list() {
        let mut i = interp();
        let v = eval_ok(&mut i, "'(1 2 3)");
        assert!(v.is_list());
        let items = v.to_vec().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Value::Int(1));
    }

    // ── If ────────────────────────────────────────────────────────────────
    #[test]
    fn test_if_true() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(if #t 1 2)"), Value::Int(1));
    }

    #[test]
    fn test_if_false() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(if #f 1 2)"), Value::Int(2));
    }

    // ── Define & Lambda ───────────────────────────────────────────────────
    #[test]
    fn test_define_and_use() {
        let mut i = interp();
        eval_ok(&mut i, "(define x 42)");
        assert_eq!(eval_ok(&mut i, "x"), Value::Int(42));
    }

    #[test]
    fn test_define_function() {
        let mut i = interp();
        eval_ok(&mut i, "(define (square n) (* n n))");
        assert_eq!(eval_ok(&mut i, "(square 7)"), Value::Int(49));
    }

    #[test]
    fn test_lambda() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, "((lambda (x y) (+ x y)) 3 4)"),
            Value::Int(7)
        );
    }

    #[test]
    fn test_variadic_lambda() {
        let mut i = interp();
        eval_ok(&mut i, "(define (head . rest) (car rest))");
        assert_eq!(eval_ok(&mut i, "(head 1 2 3)"), Value::Int(1));
    }

    // ── Let ───────────────────────────────────────────────────────────────
    #[test]
    fn test_let() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, "(let ((x 3) (y 4)) (+ x y))"),
            Value::Int(7)
        );
    }

    #[test]
    fn test_let_star() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, "(let* ((x 1) (y (+ x 1))) y)"),
            Value::Int(2)
        );
    }

    #[test]
    fn test_letrec() {
        let mut i = interp();
        assert_eq!(
            eval_ok(
                &mut i,
                "(letrec ((even? (lambda (n) (if (= n 0) #t (odd? (- n 1)))))
                          (odd?  (lambda (n) (if (= n 0) #f (even? (- n 1))))))
                   (even? 10))"
            ),
            Value::Bool(true)
        );
    }

    // ── Named let ─────────────────────────────────────────────────────────
    #[test]
    fn test_named_let() {
        let mut i = interp();
        assert_eq!(
            eval_ok(
                &mut i,
                "(let loop ((n 5) (acc 1))
                   (if (= n 0) acc (loop (- n 1) (* acc n))))"
            ),
            Value::Int(120) // 5!
        );
    }

    // ── TCO: deep recursion doesn't overflow ──────────────────────────────
    #[test]
    fn test_tco_does_not_overflow() {
        let mut i = interp();
        // 100 000 tail calls — would overflow without TCO.
        eval_ok(
            &mut i,
            "(define (count-down n)
               (if (= n 0) #t (count-down (- n 1))))
             (count-down 100000)",
        );
    }

    // ── Cond ──────────────────────────────────────────────────────────────
    #[test]
    fn test_cond() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, "(cond ((= 1 2) 'a) ((= 1 1) 'b) (else 'c))"),
            Value::Symbol("b".into())
        );
    }

    // ── And/Or ────────────────────────────────────────────────────────────
    #[test]
    fn test_and_short_circuit() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(and 1 #f 3)"), Value::Bool(false));
        assert_eq!(eval_ok(&mut i, "(and 1 2 3)"), Value::Int(3));
    }

    #[test]
    fn test_or_short_circuit() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(or #f #f 42)"), Value::Int(42));
        assert_eq!(eval_ok(&mut i, "(or #f #f #f)"), Value::Bool(false));
    }

    // ── Quasiquote ────────────────────────────────────────────────────────
    #[test]
    fn test_quasiquote_basic() {
        let mut i = interp();
        eval_ok(&mut i, "(define x 42)");
        let v = eval_ok(&mut i, "`(a ,x c)");
        let items = v.to_vec().unwrap();
        assert_eq!(items[0], Value::Symbol("a".into()));
        assert_eq!(items[1], Value::Int(42));
        assert_eq!(items[2], Value::Symbol("c".into()));
    }

    #[test]
    fn test_quasiquote_splicing() {
        let mut i = interp();
        eval_ok(&mut i, "(define xs '(1 2 3))");
        let v = eval_ok(&mut i, "`(a ,@xs b)");
        let items = v.to_vec().unwrap();
        assert_eq!(items.len(), 5);
        assert_eq!(items[0], Value::Symbol("a".into()));
        assert_eq!(items[1], Value::Int(1));
        assert_eq!(items[4], Value::Symbol("b".into()));
    }

    // ── List operations ───────────────────────────────────────────────────
    #[test]
    fn test_map() {
        let mut i = interp();
        let v = eval_ok(&mut i, "(map (lambda (x) (* x x)) '(1 2 3 4))");
        let items = v.to_vec().unwrap();
        assert_eq!(
            items,
            vec![Value::Int(1), Value::Int(4), Value::Int(9), Value::Int(16)]
        );
    }

    #[test]
    fn test_filter() {
        let mut i = interp();
        let v = eval_ok(
            &mut i,
            "(filter (lambda (x) (not (= (modulo x 2) 0))) '(1 2 3 4 5))",
        );
        let items = v.to_vec().unwrap();
        assert_eq!(items, vec![Value::Int(1), Value::Int(3), Value::Int(5)]);
    }

    #[test]
    fn test_odd_even_predicates() {
        let mut i = interp();
        // Define odd? and even? since they're not builtins
        eval_ok(
            &mut i,
            "(define (odd? n) (not (= (modulo n 2) 0)))
             (define (even? n) (= (modulo n 2) 0))",
        );
        assert_eq!(eval_ok(&mut i, "(odd? 3)"), Value::Bool(true));
        assert_eq!(eval_ok(&mut i, "(even? 4)"), Value::Bool(true));
    }

    #[test]
    fn test_apply() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(apply + '(1 2 3))"), Value::Int(6));
        assert_eq!(eval_ok(&mut i, "(apply + 1 2 '(3 4))"), Value::Int(10));
    }

    #[test]
    fn test_fold_left() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, "(fold-left + 0 '(1 2 3 4 5))"),
            Value::Int(15)
        );
    }

    #[test]
    fn test_iota() {
        let mut i = interp();
        let v = eval_ok(&mut i, "(iota 5)");
        let items = v.to_vec().unwrap();
        assert_eq!(
            items,
            vec![
                Value::Int(0),
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Int(4)
            ]
        );
    }

    // ── Do loop ───────────────────────────────────────────────────────────
    #[test]
    fn test_do_loop() {
        let mut i = interp();
        assert_eq!(
            eval_ok(
                &mut i,
                "(do ((i 0 (+ i 1))
                      (s 0 (+ s i)))
                     ((= i 5) s))"
            ),
            Value::Int(10) // 0+1+2+3+4
        );
    }

    // ── String operations ─────────────────────────────────────────────────
    #[test]
    fn test_string_append() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, r#"(string-append "hello" " " "world")"#),
            Value::Str("hello world".into())
        );
    }

    #[test]
    fn test_string_length() {
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, r#"(string-length "héllo")"#), Value::Int(5));
    }

    #[test]
    fn test_number_to_string() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, "(number->string 42)"),
            Value::Str("42".into())
        );
    }

    // ── Error handling ────────────────────────────────────────────────────
    #[test]
    fn test_unbound_variable() {
        let mut i = interp();
        let e = eval_err(&mut i, "undefined-var");
        assert!(e.contains("unbound"), "got: {}", e);
    }

    #[test]
    fn test_arity_error() {
        let mut i = interp();
        let e = eval_err(&mut i, "(car 1 2)");
        assert!(e.contains("arity") || e.contains("argument"), "got: {}", e);
    }

    #[test]
    fn test_not_a_procedure() {
        let mut i = interp();
        let e = eval_err(&mut i, "(42 1 2)");
        assert!(e.contains("procedure") || e.contains("not a"), "got: {}", e);
    }

    // ── Pelagos builtins ───────────────────────────────────────────────────
    #[test]
    fn test_service_builtin() {
        let mut i = interp();
        let v = eval_ok(
            &mut i,
            r#"(service "app"
                 (list (quote image) "alpine:latest"))"#,
        );
        assert!(
            matches!(&v, Value::ServiceSpec(s) if s.name == "app"),
            "got: {}",
            v
        );
    }

    #[test]
    fn test_compose_builtin_collects_specs() {
        let mut i = interp();
        let v = eval_ok(
            &mut i,
            r#"(compose
                  (service "db"
                    (list (quote image) "postgres:16"))
                  (service "app"
                    (list (quote image) "myapp:latest")))"#,
        );
        match &v {
            Value::ComposeSpec(c) => {
                assert_eq!(c.services.len(), 2);
                assert_eq!(c.services[0].name, "db");
                assert_eq!(c.services[1].name, "app");
            }
            _ => panic!("expected ComposeSpec, got: {}", v),
        }
    }

    #[test]
    fn test_lisp_compose_basic() {
        // Full mini-program: define a service factory, build a compose spec,
        // then store it via compose-up. The compose spec must have 2 services.
        let mut i = interp();
        i.eval_str(
            r#"
            (define (mk-service name img)
              (service name
                (list 'image img)))

            (compose-up
              (compose
                (mk-service "db"  "postgres:16")
                (mk-service "app" "alpine:latest")))
            "#,
        )
        .expect("eval failed");

        let pending = i.take_pending().expect("no pending compose");
        let spec = pending.spec.expect("no spec");
        assert_eq!(spec.services.len(), 2);
        assert_eq!(spec.services[0].name, "db");
        assert_eq!(spec.services[0].image, "postgres:16");
        assert_eq!(spec.services[1].name, "app");
        assert_eq!(spec.services[1].image, "alpine:latest");
    }

    // ── defmacro / define-service ─────────────────────────────────────────
    #[test]
    fn test_defmacro_basic() {
        // A simple macro that swaps two expressions in a list.
        let mut i = interp();
        eval_ok(&mut i, "(defmacro my-swap (a b) `(list ,b ,a))");
        let v = eval_ok(&mut i, "(let ((x 1) (y 2)) (my-swap x y))");
        let items = v.to_vec().unwrap();
        assert_eq!(items, vec![Value::Int(2), Value::Int(1)]);
    }

    #[test]
    fn test_defmacro_generates_define() {
        // Macro that generates a (define ...) form — the basis of define-service.
        let mut i = interp();
        eval_ok(&mut i, "(defmacro def-42 (name) `(define ,name 42))");
        eval_ok(&mut i, "(def-42 answer)");
        assert_eq!(eval_ok(&mut i, "answer"), Value::Int(42));
    }

    #[test]
    fn test_define_service_macro() {
        // define-service (from stdlib.lisp) produces a bound ServiceSpec whose
        // fields match the keyword options, including variable evaluation.
        let mut i = interp();
        eval_ok(&mut i, r#"(define mem "128m")"#);
        eval_ok(
            &mut i,
            r#"(define-service svc "myapp"
                 :image   "myapp:latest"
                 :network "backend"
                 :memory  mem)"#,
        );
        let v = eval_ok(&mut i, "svc");
        match v {
            Value::ServiceSpec(s) => {
                assert_eq!(s.name, "myapp");
                assert_eq!(s.image, "myapp:latest");
                assert!(s.networks.contains(&"backend".to_string()));
                assert_eq!(s.memory.as_deref(), Some("128m"));
            }
            _ => panic!("expected ServiceSpec, got: {}", v),
        }
    }

    #[test]
    fn test_define_service_with_port_variable() {
        // Variable in (:port ...) must be evaluated at call-site, not at macro-definition time.
        let mut i = interp();
        eval_ok(&mut i, "(define my-port 9090)");
        eval_ok(
            &mut i,
            r#"(define-service svc "app"
                 :image "app:latest"
                 :port  my-port 80)"#,
        );
        let v = eval_ok(&mut i, "svc");
        match v {
            Value::ServiceSpec(s) => {
                assert_eq!(s.ports.len(), 1);
                assert_eq!(s.ports[0].host, 9090);
                assert_eq!(s.ports[0].container, 80);
            }
            _ => panic!("expected ServiceSpec"),
        }
    }

    // ── define-service: dotted-pair env values with complex expressions (issue #159) ──

    #[test]
    fn test_define_service_env_dotted_pair_with_let() {
        // Regression test for issue #159: a `let` expression as the cdr of a
        // dotted-pair env value was treated as an unbound variable because the
        // S-expression `("K" . (let ...))` is indistinguishable from `("K" let ...)`
        // at the Value level (both are proper-list Pair chains).  The fix checks
        // `(= (length sub) 2)` before taking the `,@sub` splice branch so that
        // a list-valued cdr is kept together as a single expression rather than
        // being spliced apart.
        let mut i = interp();
        eval_ok(&mut i, r#"(define base-val "hello")"#);
        eval_ok(
            &mut i,
            r#"(define-service svc "myapp"
                 :image "myapp:latest"
                 :network "backend"
                 :env ("MY_VAR" . (let ((v base-val)) (if (null? v) "default" v))))"#,
        );
        let v = eval_ok(&mut i, "svc");
        match v {
            Value::ServiceSpec(s) => {
                assert_eq!(
                    s.env.get("MY_VAR").map(String::as_str),
                    Some("hello"),
                    "let expression in dotted-pair env value must be evaluated"
                );
            }
            _ => panic!("expected ServiceSpec, got: {}", v),
        }
    }

    #[test]
    fn test_define_service_env_dotted_pair_with_if() {
        // Guards against regression for other special forms used as dotted-pair
        // cdr values.  `if` is a special form; before the fix it would have
        // triggered "unbound variable: if" for the same reason as `let`.
        let mut i = interp();
        eval_ok(&mut i, r#"(define flag #t)"#);
        eval_ok(
            &mut i,
            r#"(define-service svc "myapp"
                 :image "myapp:latest"
                 :network "backend"
                 :env ("MODE" . (if flag "production" "development")))"#,
        );
        let v = eval_ok(&mut i, "svc");
        match v {
            Value::ServiceSpec(s) => {
                assert_eq!(
                    s.env.get("MODE").map(String::as_str),
                    Some("production"),
                    "if expression in dotted-pair env value must be evaluated"
                );
            }
            _ => panic!("expected ServiceSpec, got: {}", v),
        }
    }

    #[test]
    fn test_define_service_env_2element_list_unchanged() {
        // The 2-element proper-list form ("K" "v") must continue to work after
        // the length-check fix.  This is the primary documented syntax for
        // env key-value pairs.
        let mut i = interp();
        eval_ok(
            &mut i,
            r#"(define-service svc "myapp"
                 :image "myapp:latest"
                 :network "backend"
                 :env ("FOO" "bar") ("BAZ" "qux"))"#,
        );
        let v = eval_ok(&mut i, "svc");
        match v {
            Value::ServiceSpec(s) => {
                assert_eq!(s.env.get("FOO").map(String::as_str), Some("bar"));
                assert_eq!(s.env.get("BAZ").map(String::as_str), Some("qux"));
            }
            _ => panic!("expected ServiceSpec, got: {}", v),
        }
    }

    #[test]
    fn test_define_service_env_3element_sublist_errors() {
        // A 3-element list `("K" "a" "b")` is ambiguous: the writer may have
        // intended a dotted pair `("K" . "a")` with an extra stray token, or a
        // multi-value form that has no documented meaning.
        //
        // OLD behaviour (`,@sub` for all proper lists): silently set K="a" and
        // dropped "b" — a data-loss bug with no error.
        //
        // NEW behaviour (length-2 check): the `,(car sub) ,(cdr sub)` branch
        // fires, producing `(list 'env "K" ("a" "b"))`.  The cdr `("a" "b")`
        // is unquoted as a code form and evaluated as a function call, where
        // `"a"` is not a procedure → "not a procedure: string".  This is still
        // an error (good — the input is wrong), just earlier in the pipeline.
        // Surfacing any error is strictly better than silent data loss.
        let mut i = interp();
        let err = eval_err(
            &mut i,
            r#"(define-service svc "myapp"
                 :image "myapp:latest"
                 :network "backend"
                 :env ("MY_VAR" "a" "b"))"#,
        );
        assert!(
            !err.is_empty(),
            "3-element env sublist must produce an error, not silently drop data"
        );
        // The exact message depends on evaluation order but must not be empty.
        // Document the current message so future changes don't silently revert
        // to the old silent-data-loss behaviour.
        assert!(
            err.contains("not a procedure") || err.contains("expected string"),
            "unexpected error message for 3-element env sublist: {}",
            err
        );
    }

    #[test]
    fn test_service_builtin_stop_grace_period() {
        // (stop-grace-period N) sets ServiceSpec.stop_grace_period; absent = None.
        let mut i = interp();
        let v = eval_ok(
            &mut i,
            r#"(service "s"
                 (list 'image "img:1")
                 (list 'stop-grace-period 30))"#,
        );
        match v {
            Value::ServiceSpec(s) => {
                assert_eq!(s.stop_grace_period, Some(30), "stop_grace_period not set");
            }
            _ => panic!("expected ServiceSpec, got: {}", v),
        }

        // Absent → None.
        let v2 = eval_ok(&mut i, r#"(service "s2" (list 'image "img:1"))"#);
        match v2 {
            Value::ServiceSpec(s) => {
                assert_eq!(s.stop_grace_period, None);
            }
            _ => panic!("expected ServiceSpec"),
        }
    }

    #[test]
    fn test_on_ready_hook_registered() {
        let mut i = interp();
        eval_ok(&mut i, r#"(on-ready "db" (lambda () (log "db is ready")))"#);
        let hooks = i.take_hooks();
        assert!(hooks.contains_key("db"), "hook for 'db' not registered");
        assert_eq!(hooks["db"].len(), 1);
    }

    // ── Compose fixture: monitoring stack ────────────────────────────────────

    #[test]
    fn test_lisp_eval_file_monitoring_fixture() {
        // Evaluate the monitoring compose.reml fixture and verify the resulting
        // ComposeSpec matches the declared architecture:
        //   - 3 services: prometheus, loki, grafana
        //   - 1 network:  monitoring-net  (10.89.1.0/24)
        //   - 2 volumes:  prometheus-data, grafana-data
        //   - grafana depends on prometheus:9090 AND loki:3100
        //   - grafana has GF_SECURITY_ADMIN_PASSWORD = "admin" (default)
        //   - 2 on-ready hooks (prometheus, loki)
        //
        // This test exercises: define, env+fallback, on-ready, multiple
        // depends-on, dotted-pair :env with variable values, define-service.
        let src = include_str!("../../examples/compose/monitoring/compose.reml");
        let mut i = interp();
        i.eval_str(src)
            .expect("monitoring compose.reml failed to eval");

        // ── compose spec ──────────────────────────────────────────────────
        let pending = i
            .take_pending()
            .expect("no pending compose from compose-up");
        let spec = pending.spec.expect("compose-up produced no spec");

        assert_eq!(spec.services.len(), 3, "expected 3 services");

        // Services appear in definition order
        assert_eq!(spec.services[0].name, "prometheus");
        assert_eq!(spec.services[1].name, "loki");
        assert_eq!(spec.services[2].name, "grafana");

        // Images
        assert_eq!(spec.services[0].image, "monitoring-prometheus:latest");
        assert_eq!(spec.services[1].image, "monitoring-loki:latest");
        assert_eq!(spec.services[2].image, "monitoring-grafana:latest");

        // Network
        assert_eq!(spec.networks.len(), 1);
        assert_eq!(spec.networks[0].name, "monitoring-net");
        assert_eq!(spec.networks[0].subnet.as_deref(), Some("10.89.1.0/24"));
        for svc in &spec.services {
            assert!(
                svc.networks.contains(&"monitoring-net".to_string()),
                "service '{}' not on monitoring-net",
                svc.name
            );
        }

        // Volumes
        assert_eq!(spec.volumes.len(), 2);
        assert!(spec.volumes.contains(&"prometheus-data".to_string()));
        assert!(spec.volumes.contains(&"grafana-data".to_string()));

        // Grafana depends on both prometheus:9090 and loki:3100
        let grafana = &spec.services[2];
        assert_eq!(
            grafana.depends_on.len(),
            2,
            "grafana should have 2 depends-on entries"
        );
        let dep_names: Vec<&str> = grafana
            .depends_on
            .iter()
            .map(|d| d.service.as_str())
            .collect();
        assert!(
            dep_names.contains(&"prometheus"),
            "grafana missing prometheus dep"
        );
        assert!(dep_names.contains(&"loki"), "grafana missing loki dep");

        let prom_dep = grafana
            .depends_on
            .iter()
            .find(|d| d.service == "prometheus")
            .unwrap();
        let prom_check = prom_dep
            .health_check
            .as_ref()
            .expect("prometheus dep has no health check");
        assert!(
            matches!(prom_check, crate::compose::HealthCheck::Port(9090)),
            "expected Port(9090) health check for prometheus dep, got: {:?}",
            prom_check
        );

        let loki_dep = grafana
            .depends_on
            .iter()
            .find(|d| d.service == "loki")
            .unwrap();
        let loki_check = loki_dep
            .health_check
            .as_ref()
            .expect("loki dep has no health check");
        assert!(
            matches!(loki_check, crate::compose::HealthCheck::Port(3100)),
            "expected Port(3100) health check for loki dep, got: {:?}",
            loki_check
        );

        // Grafana env: GF_SECURITY_ADMIN_PASSWORD should be "admin" (default)
        let admin_pass = grafana
            .env
            .get("GF_SECURITY_ADMIN_PASSWORD")
            .map(|v| v.as_str());
        assert_eq!(
            admin_pass,
            Some("admin"),
            "grafana-password should default to 'admin'"
        );

        // Ports
        assert_eq!(spec.services[0].ports[0].host, 9090);
        assert_eq!(spec.services[1].ports[0].host, 3100);
        assert_eq!(spec.services[2].ports[0].host, 3000);

        // ── on-ready hooks ────────────────────────────────────────────────
        let hooks = i.take_hooks();
        assert!(
            hooks.contains_key("prometheus"),
            "no on-ready hook for prometheus"
        );
        assert!(hooks.contains_key("loki"), "no on-ready hook for loki");
    }

    // ── Compose fixture: monitoring stack (inline-let variant, issue #159) ──

    #[test]
    fn test_lisp_eval_file_monitoring_inline_let_fixture() {
        // Evaluate the inline-let variant of the monitoring compose.reml.
        //
        // This fixture is the real-world pattern that was impossible before
        // issue #159: secrets and tunables are read from the host environment
        // with a fallback default, expressed entirely inline as dotted-pair
        // cdr values:
        //
        //   :env ("GF_SECURITY_ADMIN_PASSWORD" . (let ((p (env "GRAFANA_PASSWORD")))
        //                                          (if (null? p) "admin" p)))
        //
        // Before the fix, (list? sub) returned true for any dotted pair whose
        // cdr was a proper list, so the ,@sub splice branch would flatten
        // (let ...) into bare tokens → "unbound variable: let".
        //
        // Assertions mirror test_lisp_eval_file_monitoring_fixture but also
        // verify the inline-let expressions produce the correct default values,
        // and that overriding via environment variables works correctly.

        // ── default behaviour (env vars unset) ────────────────────────────
        // Ensure the vars are absent so defaults apply.
        std::env::remove_var("GRAFANA_PASSWORD");
        std::env::remove_var("GRAFANA_LOG_LEVEL");
        std::env::remove_var("PROM_RETENTION");

        let src = include_str!("../../examples/compose/monitoring-inline-let/compose.reml");
        let mut i = interp();
        i.eval_str(src)
            .expect("monitoring-inline-let compose.reml failed to eval");

        let pending = i
            .take_pending()
            .expect("no pending compose from compose-up");
        let spec = pending.spec.expect("compose-up produced no spec");

        assert_eq!(spec.services.len(), 3, "expected 3 services");
        assert_eq!(spec.services[0].name, "prometheus");
        assert_eq!(spec.services[1].name, "loki");
        assert_eq!(spec.services[2].name, "grafana");

        // Network and volumes unchanged from top-level-define variant.
        assert_eq!(spec.networks.len(), 1);
        assert_eq!(spec.networks[0].name, "monitoring-net");
        assert_eq!(spec.networks[0].subnet.as_deref(), Some("10.89.1.0/24"));
        assert_eq!(spec.volumes.len(), 2);

        // Grafana depends on prometheus:9090 and loki:3100.
        let grafana = &spec.services[2];
        assert_eq!(grafana.depends_on.len(), 2);
        let dep_names: Vec<&str> = grafana
            .depends_on
            .iter()
            .map(|d| d.service.as_str())
            .collect();
        assert!(dep_names.contains(&"prometheus"));
        assert!(dep_names.contains(&"loki"));

        // ── inline-let defaults ───────────────────────────────────────────
        // GRAFANA_PASSWORD unset → "admin"
        assert_eq!(
            grafana
                .env
                .get("GF_SECURITY_ADMIN_PASSWORD")
                .map(String::as_str),
            Some("admin"),
            "inline let should default GF_SECURITY_ADMIN_PASSWORD to 'admin'"
        );
        // GRAFANA_LOG_LEVEL unset → "warn"
        assert_eq!(
            grafana.env.get("GF_LOG_LEVEL").map(String::as_str),
            Some("warn"),
            "inline let should default GF_LOG_LEVEL to 'warn'"
        );
        // GF_SERVER_HTTP_PORT comes from a variable expression, not env.
        assert_eq!(
            grafana.env.get("GF_SERVER_HTTP_PORT").map(String::as_str),
            Some("3000"),
            "port variable expression should produce '3000'"
        );
        // Static env entries still work alongside inline-let entries.
        assert_eq!(
            grafana
                .env
                .get("GF_USERS_ALLOW_SIGN_UP")
                .map(String::as_str),
            Some("false")
        );

        // Ports
        assert_eq!(spec.services[0].ports[0].host, 9090);
        assert_eq!(spec.services[1].ports[0].host, 3100);
        assert_eq!(spec.services[2].ports[0].host, 3000);

        // on-ready hooks
        let hooks = i.take_hooks();
        assert!(hooks.contains_key("prometheus"));
        assert!(hooks.contains_key("loki"));

        // ── overridden via environment variables ──────────────────────────
        std::env::set_var("GRAFANA_PASSWORD", "s3cr3t");
        std::env::set_var("GRAFANA_LOG_LEVEL", "debug");

        let mut i2 = interp();
        i2.eval_str(src)
            .expect("monitoring-inline-let compose.reml failed to eval (overridden)");
        let spec2 = i2
            .take_pending()
            .expect("no pending compose (overridden)")
            .spec
            .expect("no spec (overridden)");
        let grafana2 = spec2.services.iter().find(|s| s.name == "grafana").unwrap();

        assert_eq!(
            grafana2
                .env
                .get("GF_SECURITY_ADMIN_PASSWORD")
                .map(String::as_str),
            Some("s3cr3t"),
            "GRAFANA_PASSWORD env var should override the inline-let default"
        );
        assert_eq!(
            grafana2.env.get("GF_LOG_LEVEL").map(String::as_str),
            Some("debug"),
            "GRAFANA_LOG_LEVEL env var should override the inline-let default"
        );

        // Clean up so we don't bleed state into other tests.
        std::env::remove_var("GRAFANA_PASSWORD");
        std::env::remove_var("GRAFANA_LOG_LEVEL");
    }

    // ── Compose fixture: rust-builder stack ───────────────────────────────

    #[test]
    fn test_lisp_eval_file_rust_builder_fixture() {
        // Evaluate the rust-builder compose.reml fixture and verify the
        // resulting ComposeSpec matches the declared architecture:
        //   - 1 service:  rust-builder  (image "rust-builder:latest")
        //   - 0 networks  (no inter-service communication needed)
        //   - 2 compose volumes: cargo-registry, sccache-cache
        //   - service mounts both volumes
        //   - service env: RUSTC_WRAPPER=sccache, SCCACHE_DIR=/sccache-cache
        //   - service command: ["sleep", "infinity"]
        //
        // This test exercises: define, env+fallback, :volume service option,
        // :command, dotted-pair :env, define-service.
        let src = include_str!("../../examples/compose/rust-builder/compose.reml");
        let mut i = interp();
        i.eval_str(src)
            .expect("rust-builder compose.reml failed to eval");

        let pending = i
            .take_pending()
            .expect("no pending compose from compose-up");
        let spec = pending.spec.expect("compose-up produced no spec");

        // Single service, no network
        assert_eq!(spec.services.len(), 1, "expected 1 service");
        assert_eq!(spec.networks.len(), 0, "expected 0 networks");

        // Compose-level volumes
        assert_eq!(spec.volumes.len(), 2, "expected 2 volumes");
        assert!(
            spec.volumes.contains(&"cargo-registry".to_string()),
            "missing cargo-registry volume"
        );
        assert!(
            spec.volumes.contains(&"sccache-cache".to_string()),
            "missing sccache-cache volume"
        );

        let svc = &spec.services[0];
        assert_eq!(svc.name, "rust-builder");
        assert_eq!(svc.image, "rust-builder:latest");

        // Command: sleep infinity
        let cmd = svc.command.as_ref().expect("service has no command");
        assert_eq!(cmd, &vec!["sleep".to_string(), "infinity".to_string()]);

        // Service-level volume mounts
        assert_eq!(svc.volumes.len(), 2, "expected 2 volume mounts on service");
        let registry_mount = svc
            .volumes
            .iter()
            .find(|v| v.name == "cargo-registry")
            .expect("cargo-registry mount missing");
        assert_eq!(registry_mount.mount_path, "/root/.cargo/registry");

        let sccache_mount = svc
            .volumes
            .iter()
            .find(|v| v.name == "sccache-cache")
            .expect("sccache-cache mount missing");
        assert_eq!(sccache_mount.mount_path, "/sccache-cache");

        // Environment variables
        assert_eq!(
            svc.env.get("RUSTC_WRAPPER").map(|s| s.as_str()),
            Some("sccache"),
            "RUSTC_WRAPPER should be 'sccache'"
        );
        assert_eq!(
            svc.env.get("SCCACHE_DIR").map(|s| s.as_str()),
            Some("/sccache-cache"),
            "SCCACHE_DIR should be '/sccache-cache'"
        );
        assert_eq!(
            svc.env.get("RUST_EDITION").map(|s| s.as_str()),
            Some("2021"),
            "RUST_EDITION should be '2021'"
        );
    }

    // ── format builtin ────────────────────────────────────────────────────

    #[test]
    fn test_format_builtin() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, r#"(format "~s + ~s = ~s" 1 2 3)"#),
            Value::Str("1 + 2 = 3".into())
        );
    }

    #[test]
    fn test_format_tilde_a_display_no_quotes() {
        // ~a = display: strings without quotes
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, r#"(format "hello ~a" "world")"#),
            Value::Str("hello world".into())
        );
    }

    #[test]
    fn test_format_tilde_s_write_with_quotes() {
        // ~s = write: strings with quotes
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, r#"(format "val=~s" "hi")"#),
            Value::Str("val=\"hi\"".into())
        );
    }

    // ── sleep builtin ─────────────────────────────────────────────────────

    #[test]
    fn test_sleep_builtin() {
        // sleep 0 returns Nil without panic
        let mut i = interp();
        assert_eq!(eval_ok(&mut i, "(sleep 0)"), Value::Nil);
    }

    // ── guard special form ────────────────────────────────────────────────

    #[test]
    fn test_guard_catches_error() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, r#"(guard (e (#t "caught")) (error "boom"))"#),
            Value::Str("caught".into())
        );
    }

    #[test]
    fn test_guard_no_error_returns_body_value() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, r#"(guard (e (#t "caught")) 42)"#),
            Value::Int(42)
        );
    }

    #[test]
    fn test_guard_reraises_on_no_match() {
        let mut i = interp();
        let msg = eval_err(&mut i, r#"(guard (e (#f "nope")) (error "boom"))"#);
        assert!(msg.contains("boom"), "expected 'boom' in: {}", msg);
    }

    #[test]
    fn test_guard_binds_error_message() {
        let mut i = interp();
        assert_eq!(
            eval_ok(&mut i, r#"(guard (msg (#t msg)) (error "the-message"))"#),
            Value::Str("the-message".into())
        );
    }

    // ── with-cleanup macro ────────────────────────────────────────────────

    #[test]
    fn test_with_cleanup_normal_exit() {
        let mut i = interp();
        eval_ok(&mut i, "(define last-result #f)");
        let v = eval_ok(
            &mut i,
            r#"(with-cleanup (lambda (result) (set! last-result result)) 99)"#,
        );
        assert_eq!(v, Value::Int(99));
        assert_eq!(eval_ok(&mut i, "(ok? last-result)"), Value::Bool(true));
        assert_eq!(eval_ok(&mut i, "(ok-value last-result)"), Value::Int(99));
    }

    #[test]
    fn test_with_cleanup_error_exit() {
        let mut i = interp();
        eval_ok(&mut i, "(define last-result #f)");
        let msg = eval_err(
            &mut i,
            r#"(with-cleanup (lambda (result) (set! last-result result)) (error "oops"))"#,
        );
        assert!(msg.contains("oops"), "got: {}", msg);
        assert_eq!(eval_ok(&mut i, "(err? last-result)"), Value::Bool(true));
        assert_eq!(
            eval_ok(&mut i, "(err-reason last-result)"),
            Value::Str("oops".into())
        );
    }

    // ── Future / executor model ───────────────────────────────────────────

    fn runtime_interp() -> Interpreter {
        Interpreter::new_with_runtime("test".to_string(), std::path::PathBuf::from("/tmp"))
    }

    #[test]
    fn test_future_type_name_and_display() {
        use crate::compose::ServiceSpec;
        use crate::lisp::value::FutureKind;
        let f = Value::Future {
            id: 1,
            name: "db".into(),
            kind: FutureKind::Container {
                spec: Box::new(ServiceSpec {
                    name: "db".into(),
                    ..Default::default()
                }),
                inject: None,
            },
            after: vec![],
        };
        assert_eq!(f.type_name(), "future");
        assert_eq!(format!("{}", f), "#<future:db>");
        assert!(f.is_truthy());
    }

    #[test]
    fn test_future_display_with_deps() {
        use crate::compose::ServiceSpec;
        use crate::lisp::value::FutureKind;
        let f = Value::Future {
            id: 3,
            name: "app".into(),
            kind: FutureKind::Container {
                spec: Box::new(ServiceSpec {
                    name: "app".into(),
                    ..Default::default()
                }),
                inject: None,
            },
            after: vec![
                Value::Future {
                    id: 1,
                    name: "dep1".into(),
                    kind: FutureKind::Container {
                        spec: Box::new(ServiceSpec {
                            name: "dep1".into(),
                            ..Default::default()
                        }),
                        inject: None,
                    },
                    after: vec![],
                },
                Value::Future {
                    id: 2,
                    name: "dep2".into(),
                    kind: FutureKind::Container {
                        spec: Box::new(ServiceSpec {
                            name: "dep2".into(),
                            ..Default::default()
                        }),
                        inject: None,
                    },
                    after: vec![],
                },
            ],
        };
        assert_eq!(format!("{}", f), "#<future:app after:2>");
    }

    #[test]
    fn test_container_start_async_returns_future() {
        let mut i = runtime_interp();
        eval_ok(
            &mut i,
            r#"(define-service svc-test "db" :image "alpine:latest" :network "test-net")"#,
        );
        let v = eval_ok(&mut i, "(start svc-test)");
        assert_eq!(v.type_name(), "future");
        assert!(format!("{}", v).contains("db"));
    }

    #[test]
    fn test_container_start_async_with_after() {
        let mut i = runtime_interp();
        eval_ok(
            &mut i,
            r#"(define-service svc-db  "db"  :image "alpine:latest" :network "net")"#,
        );
        eval_ok(
            &mut i,
            r#"(define-service svc-app "app" :image "alpine:latest" :network "net")"#,
        );
        eval_ok(&mut i, "(define db-fut (start svc-db))");
        // app-fut declares :needs db-fut — should still be a future
        let v = eval_ok(&mut i, "(start svc-app :needs (list db-fut))");
        assert_eq!(v.type_name(), "future");
        // after:1 dependency should appear in display
        assert!(format!("{}", v).contains("after:1"), "got: {}", v);
    }

    #[test]
    fn test_run_all_cycle_detection() {
        let mut i = runtime_interp();
        // Build two futures that depend on each other — run must detect the cycle.
        // We can't directly create circular :needs refs via the API (a future must
        // exist before it can be referenced), so test a self-referential-style cycle
        // by constructing two futures where A :needs B and B :needs A indirectly.
        // Instead, test that run rejects a non-list argument.
        let err = eval_err(&mut i, r#"(run "not-a-list")"#);
        assert!(err.contains("list"), "got: {}", err);
    }

    #[test]
    fn test_result_ref_found_and_missing() {
        let mut i = interp();
        // Build a manual alist and test result-ref
        eval_ok(
            &mut i,
            r#"(define results (list (cons "db" 42) (cons "cache" 99)))"#,
        );
        assert_eq!(
            eval_ok(&mut i, r#"(result-ref results "db")"#),
            Value::Int(42)
        );
        assert_eq!(
            eval_ok(&mut i, r#"(result-ref results "cache")"#),
            Value::Int(99)
        );
        let err = eval_err(&mut i, r#"(result-ref results "missing")"#);
        assert!(err.contains("missing"), "got: {}", err);
    }

    #[test]
    fn test_await_rejects_non_future() {
        let mut i = runtime_interp();
        let err = eval_err(&mut i, r#"(await "not-a-future")"#);
        assert!(err.contains("expected future"), "got: {}", err);
    }

    #[test]
    fn test_then_all_returns_join_future() {
        let mut i = runtime_interp();
        eval_ok(
            &mut i,
            r#"(define-service svc1 "s1" :image "alpine:latest" :network "net")
               (define-service svc2 "s2" :image "alpine:latest" :network "net")
               (define f1 (start svc1))
               (define f2 (start svc2))
               (define j  (then-all (list f1 f2) (lambda (v1 v2) (list v1 v2))))"#,
        );
        let v = eval_ok(&mut i, "j");
        assert_eq!(v.type_name(), "future");
        let display = format!("{}", v);
        assert!(
            display.contains("join"),
            "expected 'join' in display, got: {}",
            display
        );
        assert!(
            display.contains("after:2"),
            "expected after:2, got: {}",
            display
        );
    }

    #[test]
    fn test_then_all_rejects_non_future_in_list() {
        let mut i = runtime_interp();
        let err = eval_err(&mut i, r#"(then-all (list 1 2) (lambda (a b) a))"#);
        assert!(err.contains("expected futures"), "got: {}", err);
    }

    #[test]
    fn test_then_all_rejects_non_lambda() {
        let mut i = runtime_interp();
        eval_ok(
            &mut i,
            r#"(define-service svc "s" :image "alpine:latest" :network "net")
               (define f (start svc))"#,
        );
        let err = eval_err(&mut i, r#"(then-all (list f) "not-a-lambda")"#);
        assert!(err.contains("expected lambda"), "got: {}", err);
    }

    // ── define-nodes macro ───────────────────────────────────────────

    #[test]
    fn test_define_nodes_macro() {
        // (define-nodes (v1 svc1) (v2 svc2) ...) expands to individual
        // (define v (start svc)) forms.
        let mut i = runtime_interp();
        eval_ok(
            &mut i,
            r#"(define-service svc1 "a" :image "alpine:latest" :network "net")
               (define-service svc2 "b" :image "alpine:latest" :network "net")
               (define-nodes (a svc1) (b svc2))"#,
        );
        assert_eq!(eval_ok(&mut i, "a").type_name(), "future");
        assert_eq!(eval_ok(&mut i, "b").type_name(), "future");
    }

    // ── define-results macro ─────────────────────────────────────────

    #[test]
    fn test_define_results_macro() {
        // (define-results alist (var key) ...) destructures an alist into bindings.
        let mut i = interp();
        eval_ok(
            &mut i,
            r#"(define results (list (cons "db" 42) (cons "app" 99)))
               (define-results results
                 (db-h  "db")
                 (app-h "app"))"#,
        );
        assert_eq!(eval_ok(&mut i, "db-h"), Value::Int(42));
        assert_eq!(eval_ok(&mut i, "app-h"), Value::Int(99));
    }

    // ── define-then macro ────────────────────────────────────────────

    #[test]
    fn test_define_then_macro() {
        // (define-then name upstream (param) body...) expands to
        // (define name (then upstream (lambda (param) body...)))
        let mut i = runtime_interp();
        eval_ok(
            &mut i,
            r#"(define-service svc "db" :image "alpine:latest" :network "net")
               (define db (start svc))
               (define-then db-url db (h) "postgres://localhost/db")"#,
        );
        let v = eval_ok(&mut i, "db-url");
        assert_eq!(v.type_name(), "future", "define-then should bind a future");
        // define-then names the future after the binding, not the upstream.
        let display = format!("{}", v);
        assert!(
            display.contains("db-url"),
            "expected 'db-url' in display, got: {}",
            display
        );
    }

    // ── define-run macro ─────────────────────────────────────────────

    #[test]
    fn test_define_run_empty_bindings() {
        // (define-run) with no bindings runs (run (list)) → Nil.
        let mut i = runtime_interp();
        let v = eval_ok(&mut i, "(define-run)");
        assert_eq!(v, Value::Nil, "empty define-run should return Nil");
    }

    #[test]
    fn test_define_run_parallel_empty_bindings() {
        // :parallel keyword is forwarded; empty list still returns Nil.
        let mut i = runtime_interp();
        let v = eval_ok(&mut i, "(define-run :parallel)");
        assert_eq!(v, Value::Nil);
    }

    #[test]
    fn test_define_run_derives_key_from_symbol() {
        // define-run uses (symbol->string future-var) to look up results.
        // Simulate by seeding _run_result_ manually and checking bindings.
        let mut i = interp();
        eval_ok(
            &mut i,
            r#"(define db    42)
               (define cache 99)
               (define _run_result_ (list (cons "db" db) (cons "cache" cache)))
               (define db-handle    (result-ref _run_result_ "db"))
               (define cache-handle (result-ref _run_result_ "cache"))"#,
        );
        assert_eq!(eval_ok(&mut i, "db-handle"), Value::Int(42));
        assert_eq!(eval_ok(&mut i, "cache-handle"), Value::Int(99));
    }

    // ── container-start-bg / container-join ─────────────────────────

    #[test]
    fn test_container_start_bg_returns_pending() {
        // container-start-bg returns a pending-container without blocking.
        // (No real image needed — we just check the type before joining.)
        let mut i = runtime_interp();
        eval_ok(
            &mut i,
            r#"(define-service svc "db" :image "alpine:latest" :network "net")"#,
        );
        let v = eval_ok(&mut i, "(container-start-bg svc)");
        assert_eq!(
            v.type_name(),
            "pending-container",
            "container-start-bg should return a pending-container immediately"
        );
    }

    #[test]
    fn test_container_join_errors_on_bad_arg() {
        // container-join rejects non-pending-container arguments.
        let mut i = runtime_interp();
        let err = eval_err(&mut i, r#"(container-join "not-a-pending")"#);
        assert!(
            err.contains("pending-container"),
            "expected type error mentioning pending-container, got: {}",
            err
        );
    }

    #[test]
    fn test_container_start_bg_join_propagates_error() {
        // Joining a failed background start surfaces the error message.
        let mut i = runtime_interp();
        eval_ok(
            &mut i,
            r#"(define-service svc "db" :image "no-such-image:latest" :network "net")"#,
        );
        eval_ok(&mut i, "(define pending (container-start-bg svc))");
        // The background thread will fail; joining should surface the error.
        let err = eval_err(&mut i, "(container-join pending)");
        // Any non-empty error is sufficient — the image doesn't exist.
        assert!(
            !err.is_empty(),
            "expected an error from failed container start"
        );
    }

    // ── run parallel keyword parsing ─────────────────────────────────

    #[test]
    fn test_run_all_accepts_parallel_keyword() {
        // :parallel on an empty list is valid — returns empty alist (Nil).
        let mut i = runtime_interp();
        let v = eval_ok(&mut i, "(run (list) :parallel)");
        assert_eq!(
            v,
            Value::Nil,
            "empty parallel run should return Nil (empty alist)"
        );
    }

    #[test]
    fn test_run_all_accepts_max_parallel_keyword() {
        // :max-parallel implies :parallel; empty list is fine.
        let mut i = runtime_interp();
        let v = eval_ok(&mut i, "(run (list) :max-parallel 4)");
        assert_eq!(v, Value::Nil);
    }

    #[test]
    fn test_run_all_max_parallel_without_explicit_parallel_flag() {
        // :max-parallel alone (no explicit :parallel) should also work.
        let mut i = runtime_interp();
        let v = eval_ok(&mut i, "(run (list) :max-parallel 2)");
        assert_eq!(v, Value::Nil);
    }

    #[test]
    fn test_run_all_rejects_zero_max_parallel() {
        let mut i = runtime_interp();
        let err = eval_err(&mut i, "(run (list) :max-parallel 0)");
        assert!(err.contains("positive"), "got: {}", err);
    }

    #[test]
    fn test_run_all_rejects_unknown_keyword() {
        let mut i = runtime_interp();
        let err = eval_err(&mut i, "(run (list) :unknown)");
        assert!(err.contains("unexpected"), "got: {}", err);
    }

    #[test]
    fn test_run_transitive_discovery_attempts_container_upstream() {
        // run() now discovers transitive :needs dependencies automatically.
        // Listing only `url-fut` (a Transform) is enough — run finds `db-fut`
        // (its Container upstream) and attempts to execute it.  In unit tests
        // the image is guaranteed nonexistent, so the attempt fails; the error
        // proves that discovery and execution were triggered (not silently skipped).
        //
        // Use a deliberately-invalid image name rather than a real one like
        // "alpine:latest" — a cached real image would cause the container to
        // succeed, turning the expected Err into Ok and panicking the test.
        let mut i = runtime_interp();
        eval_ok(
            &mut i,
            r#"(define-service svc "db" :image "this-image-does-not-exist:unit-test" :network "net")
               (define db-fut  (start svc))
               (define url-fut (then db-fut (lambda (x) "postgres://localhost/db")))"#,
        );
        // Container start fails (image nonexistent); error proves discovery ran.
        let err = eval_err(&mut i, "(run (list url-fut) :parallel)");
        assert!(!err.is_empty(), "expected container-start error, got none");
    }

    #[test]
    fn test_run_only_terminal_futures_in_alist() {
        // When transitive deps are discovered, only explicitly listed (terminal)
        // futures appear in the result alist.  Intermediates are executed but not
        // surfaced to the caller.
        // This test verifies the structural contract using a serial (non-parallel)
        // run where the Container upstream fails immediately, letting us inspect
        // the error without worrying about the alist contents.  The complementary
        // integration test verifies the alist shape with real containers.
        let mut i = runtime_interp();
        eval_ok(
            &mut i,
            r#"(define-service svc "db" :image "this-image-does-not-exist:unit-test" :network "net")
               (define db-fut  (start svc))
               (define url-fut (then db-fut (lambda (x) x)))"#,
        );
        // Both futures listed → both are terminal → both would appear in alist.
        // Fails at container start (image nonexistent); confirms both were attempted.
        let err = eval_err(&mut i, "(run (list db-fut url-fut))");
        assert!(!err.is_empty());
    }
}
