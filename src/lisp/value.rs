//! Lisp value type: the runtime representation of all Lisp objects.

use crate::sexpr::SExpr;
use std::fmt;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// Channel receiver used by [`Value::PendingContainer`].
///
/// The inner `Option` allows `container-join` to `take()` the receiver,
/// turning a second join into a detectable error rather than a deadlock.
pub type PendingRx =
    Arc<Mutex<Option<std::sync::mpsc::Receiver<Result<(String, i32, Option<String>), String>>>>>;

use super::env::Env;

/// A boxed native function: `(args) -> Result<Value>`.
pub type NativeFn = Rc<dyn Fn(&[Value]) -> Result<Value, LispError>>;

/// Every value that can be produced or consumed by the Lisp interpreter.
///
/// All variants implement `Clone` — clones of `Pair`, `Lambda`, and `Native`
/// are cheap (they bump an `Rc` reference count).
#[derive(Clone)]
pub enum Value {
    /// The empty list / boolean false proxy / unit — written `()`.
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Symbol(String),
    /// A cons cell; proper lists are chains terminated by `Nil`.
    Pair(Rc<(Value, Value)>),
    Lambda {
        params: Params,
        body: Vec<SExpr>,
        env: Env,
    },
    /// A user-defined macro created by `defmacro`.  Like a `Lambda` but receives
    /// its arguments unevaluated; the body is evaluated and its result is then
    /// evaluated in the caller's environment (macro expansion).
    Macro {
        name: String,
        params: Params,
        body: Vec<SExpr>,
        env: Env,
    },
    Native(String, NativeFn),
    // ── Pelagos domain values ──────────────────────────────────────────────
    ServiceSpec(Box<crate::compose::ServiceSpec>),
    NetworkSpec(Box<crate::compose::NetworkSpec>),
    VolumeSpec(String),
    ComposeSpec(Box<crate::compose::ComposeFile>),
    /// A running container spawned via `container-start`.
    ContainerHandle {
        /// Scoped container name, e.g. `"proj-postgres"`.
        name: String,
        /// PID of the container process.
        pid: i32,
        /// Primary bridge IP assigned to the container, if any.
        ip: Option<String>,
        /// Container handles to stop after this one exits, in reverse
        /// topological order.  Populated by `run` for terminal Container
        /// futures; empty for handles created by `container-start` / `resolve`.
        deps: Vec<Value>,
    },
    /// A container being started in the background via `container-start-bg`.
    ///
    /// Call `(container-join handle)` to block until the container is ready
    /// and obtain a `ContainerHandle`.  Joining a second time returns an error.
    PendingContainer(PendingRx),
    /// A lazy future created by `start` or `then`.
    ///
    /// Nothing executes until `(run ...)` or `(resolve ...)` is called.
    ///
    /// - `run` — static executor: receives a list of *terminal* futures,
    ///   discovers transitive `:needs` dependencies automatically, topo-sorts,
    ///   and executes serially or in parallel tiers.
    /// - `resolve` — dynamic executor: walks the chain recursively from the tip;
    ///   if a `then` lambda returns another `Future`, it is resolved too
    ///   (monadic flatten).
    Future {
        /// Unique ID assigned at creation — used for deduplication in executors.
        id: u64,
        /// Name — for display, error messages, and the `run-all` result alist.
        name: String,
        /// What work this future performs when executed.
        kind: FutureKind,
        /// Futures that must complete before this one starts.
        ///
        /// Stored as values (not just IDs) so `resolve` can walk upstream
        /// without a separate registry.  `run-all` extracts IDs via
        /// `Value::future_id()` for topological sorting.
        after: Vec<Value>,
    },
}

/// The two kinds of work a [`Value::Future`] can perform.
#[derive(Clone)]
pub enum FutureKind {
    /// Spawn a container.  Optional `:inject` lambda receives the resolved
    /// values of all `:after` dependencies (in order) and returns a list of
    /// `(key . value)` pairs merged into the service env before spawning.
    Container {
        spec: Box<crate::compose::ServiceSpec>,
        /// Boxed to break the `Value → FutureKind → Value` recursive cycle.
        inject: Option<Box<Value>>,
    },
    /// Apply a pure transform to a single upstream future's resolved value.
    /// Created by `(then upstream-future (lambda (v) ...))`.
    ///
    /// The upstream future is stored as a `Value` (not just its ID) so that
    /// `resolve` can walk the chain recursively without a separate registry.
    /// If the transform lambda returns another `Value::Future`, the dynamic
    /// executor (`resolve`) flattens it automatically (monadic bind).
    Transform {
        /// The upstream future whose resolved value is passed to `transform`.
        /// Boxed to break the `Value → FutureKind → Value` recursive cycle.
        upstream: Box<Value>,
        /// Boxed to break the `Value → FutureKind → Value` recursive cycle.
        transform: Box<Value>,
    },
    /// Wait for multiple upstream futures and pass all their resolved values
    /// to a lambda.  Created by `(then-all (list f1 f2 ...) (lambda (v1 v2 ...) ...))`.
    ///
    /// The upstreams are stored in `Value::Future { after: Vec<Value> }` — no
    /// duplication in `kind` is needed.  The lambda receives the resolved
    /// values of all `after` futures in declaration order.
    ///
    /// If the lambda returns a `Future`, it is flattened automatically by both
    /// executors (same rule as `Transform`).
    Join {
        /// Boxed to break the `Value → FutureKind → Value` recursive cycle.
        transform: Box<Value>,
    },
}

/// Lambda parameter specification.
#[derive(Debug, Clone)]
pub enum Params {
    /// `(lambda (a b c) ...)` — exact arity.
    Fixed(Vec<String>),
    /// `(lambda (a b . rest) ...)` — at least N args, rest in a list.
    Variadic(Vec<String>, String),
    /// `(lambda args ...)` — all args as a single list.
    Rest(String),
}

/// A Lisp evaluation error with optional source position.
#[derive(Debug, Clone)]
pub struct LispError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl LispError {
    pub fn new(message: impl Into<String>) -> Self {
        LispError {
            message: message.into(),
            line: 0,
            col: 0,
        }
    }

    pub fn at(message: impl Into<String>, line: usize, col: usize) -> Self {
        LispError {
            message: message.into(),
            line,
            col,
        }
    }
}

impl fmt::Display for LispError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(f, "{}:{}: {}", self.line, self.col, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for LispError {}

impl Value {
    /// Build a proper list from an iterator of values.
    pub fn list(items: impl DoubleEndedIterator<Item = Value>) -> Value {
        let mut result = Value::Nil;
        for item in items.rev() {
            result = Value::Pair(Rc::new((item, result)));
        }
        result
    }

    /// Collect a proper list into a `Vec`, or return an error if not a proper list.
    pub fn to_vec(&self) -> Result<Vec<Value>, LispError> {
        let mut result = Vec::new();
        let mut cur = self.clone();
        loop {
            match cur {
                Value::Nil => return Ok(result),
                Value::Pair(p) => {
                    result.push(p.0.clone());
                    cur = p.1.clone();
                }
                _ => return Err(LispError::new("not a proper list")),
            }
        }
    }

    /// Truthiness: everything is truthy except `#f`.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Bool(false))
    }

    /// Return a human-readable type name for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::Pair(_) => "pair",
            Value::Lambda { .. } => "lambda",
            Value::Macro { .. } => "macro",
            Value::Native(_, _) => "procedure",
            Value::ServiceSpec(_) => "service-spec",
            Value::NetworkSpec(_) => "network-spec",
            Value::VolumeSpec(_) => "volume-spec",
            Value::ComposeSpec(_) => "compose-spec",
            Value::ContainerHandle { .. } => "container",
            Value::PendingContainer(_) => "pending-container",
            Value::Future { .. } => "future",
        }
    }

    /// Return the `id` of this future, or `None` if not a `Future`.
    ///
    /// Used by executors to extract dependency IDs from `after: Vec<Value>`.
    pub fn future_id(&self) -> Option<u64> {
        match self {
            Value::Future { id, .. } => Some(*id),
            _ => None,
        }
    }

    /// True if this is a proper list (nil-terminated chain of pairs).
    pub fn is_list(&self) -> bool {
        let mut cur = self;
        loop {
            match cur {
                Value::Nil => return true,
                Value::Pair(p) => cur = &p.1,
                _ => return false,
            }
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "()"),
            Value::Bool(true) => write!(f, "#t"),
            Value::Bool(false) => write!(f, "#f"),
            Value::Int(n) => write!(f, "{}", n),
            Value::Float(n) => {
                if n.fract() == 0.0 && n.is_finite() {
                    write!(f, "{}.0", n)
                } else {
                    write!(f, "{}", n)
                }
            }
            Value::Str(s) => {
                write!(f, "\"")?;
                for c in s.chars() {
                    match c {
                        '"' => write!(f, "\\\"")?,
                        '\\' => write!(f, "\\\\")?,
                        '\n' => write!(f, "\\n")?,
                        '\t' => write!(f, "\\t")?,
                        c => write!(f, "{}", c)?,
                    }
                }
                write!(f, "\"")
            }
            Value::Symbol(s) => write!(f, "{}", s),
            Value::Pair(_) => {
                write!(f, "(")?;
                // Clone once to get an owned cursor we can advance.
                let mut cur = self.clone();
                let mut first = true;
                loop {
                    match cur {
                        Value::Nil => break,
                        Value::Pair(p) => {
                            if !first {
                                write!(f, " ")?;
                            }
                            first = false;
                            write!(f, "{}", p.0)?;
                            cur = p.1.clone();
                        }
                        other => {
                            write!(f, " . {}", other)?;
                            break;
                        }
                    }
                }
                write!(f, ")")
            }
            Value::Lambda { .. } => write!(f, "#<lambda>"),
            Value::Macro { name, .. } => write!(f, "#<macro:{}>", name),
            Value::Native(name, _) => write!(f, "#<procedure:{}>", name),
            Value::ServiceSpec(s) => write!(f, "#<service:{}>", s.name),
            Value::NetworkSpec(n) => write!(f, "#<network:{}>", n.name),
            Value::VolumeSpec(v) => write!(f, "#<volume:{}>", v),
            Value::ComposeSpec(_) => write!(f, "#<compose-spec>"),
            Value::ContainerHandle { name, .. } => write!(f, "#<container {}>", name),
            Value::PendingContainer(_) => write!(f, "#<pending-container>"),
            Value::Future { name, after, .. } => {
                if after.is_empty() {
                    write!(f, "#<future:{}>", name)
                } else {
                    write!(f, "#<future:{} after:{}>", name, after.len())
                }
            }
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self)
    }
}

/// Convert a Lisp `Value` back to an `SExpr` for macro expansion.
///
/// Only pure-data values can be serialised.  Procedural values (`Lambda`,
/// `Macro`, `Native`) and Pelagos domain values (`ServiceSpec`, etc.) cannot
/// appear in a macro expansion — return an error if they do.
///
/// Note: `SExpr` has no numeric variants; numbers and booleans are stored as
/// `Atom` strings and re-parsed by the evaluator on the next pass.
pub fn value_to_sexpr(v: Value) -> Result<SExpr, LispError> {
    match v {
        Value::Nil => Ok(SExpr::List(vec![])),
        Value::Bool(true) => Ok(SExpr::Atom("#t".into())),
        Value::Bool(false) => Ok(SExpr::Atom("#f".into())),
        Value::Int(n) => Ok(SExpr::Atom(n.to_string())),
        Value::Float(f) => Ok(SExpr::Atom(f.to_string())),
        Value::Str(s) => Ok(SExpr::Str(s)),
        Value::Symbol(s) => Ok(SExpr::Atom(s)),
        Value::Pair(_) => {
            // Walk the chain; produce DottedList if it terminates non-nil.
            let mut head_items = Vec::new();
            let mut cur = v;
            loop {
                match cur {
                    Value::Nil => return Ok(SExpr::List(head_items)),
                    Value::Pair(p) => {
                        head_items.push(value_to_sexpr(p.0.clone())?);
                        cur = p.1.clone();
                    }
                    other => {
                        return Ok(SExpr::DottedList(
                            head_items,
                            Box::new(value_to_sexpr(other)?),
                        ));
                    }
                }
            }
        }
        other => Err(LispError::new(format!(
            "macro expansion produced non-serialisable value: {}",
            other.type_name()
        ))),
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
            (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Symbol(a), Value::Symbol(b)) => a == b,
            (Value::Pair(a), Value::Pair(b)) => a.0 == b.0 && a.1 == b.1,
            // Functions are never equal (standard Lisp semantics).
            _ => false,
        }
    }
}
