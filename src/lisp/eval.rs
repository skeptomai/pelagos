//! Core Lisp evaluator with tail-call optimisation (TCO).
//!
//! The evaluation strategy: `eval_step` performs one reduction step.  If the
//! result is another expression to evaluate in a new environment (a tail call),
//! it returns `Step::Tail`.  The `eval` entry point loops over these steps so
//! tail calls do not consume stack space.

use std::rc::Rc;

use crate::sexpr::SExpr;

use super::env::{Env, EnvFrame};
use super::value::{value_to_sexpr, LispError, Params, Value};

// ---------------------------------------------------------------------------
// TCO driver
// ---------------------------------------------------------------------------

enum Step {
    Done(Value),
    Tail(SExpr, Env),
}

/// Evaluate `expr` in `env`, using a TCO loop to avoid deep stacks on tail calls.
pub fn eval(expr: SExpr, env: Env) -> Result<Value, LispError> {
    let mut cur_expr = expr;
    let mut cur_env = env;
    loop {
        match eval_step(cur_expr, Rc::clone(&cur_env))? {
            Step::Done(v) => return Ok(v),
            Step::Tail(e, new_env) => {
                cur_expr = e;
                cur_env = new_env;
            }
        }
    }
}

/// Apply a callable value to already-evaluated arguments.
///
/// This is a non-TCO entry point used by builtins like `map`, `apply`, etc.
pub fn eval_apply(func: &Value, args: &[Value]) -> Result<Value, LispError> {
    match func {
        Value::Lambda { params, body, env } => {
            let frame = EnvFrame::child(env);
            bind_params(params, args, &frame)?;
            eval_body(body, frame)
        }
        Value::Native(_, f) => f(args),
        other => Err(LispError::new(format!(
            "not a procedure: {}",
            other.type_name()
        ))),
    }
}

// ---------------------------------------------------------------------------
// Single-step evaluation
// ---------------------------------------------------------------------------

fn eval_step(expr: SExpr, env: Env) -> Result<Step, LispError> {
    match expr {
        // ── Self-evaluating atoms ──────────────────────────────────────────
        SExpr::Str(s) => Ok(Step::Done(Value::Str(s))),

        SExpr::Atom(ref s) => {
            // Boolean literals
            if s == "#t" || s == "#true" {
                return Ok(Step::Done(Value::Bool(true)));
            }
            if s == "#f" || s == "#false" {
                return Ok(Step::Done(Value::Bool(false)));
            }
            // Integer literals
            if let Ok(n) = s.parse::<i64>() {
                return Ok(Step::Done(Value::Int(n)));
            }
            // Float literals (must contain '.' or 'e'/'E' to be unambiguous)
            if s.contains('.') || s.contains('e') || s.contains('E') {
                if let Ok(f) = s.parse::<f64>() {
                    return Ok(Step::Done(Value::Float(f)));
                }
            }
            // Symbol lookup
            let val = env.borrow().lookup(s)?;
            Ok(Step::Done(val))
        }

        // ── Compound expressions ───────────────────────────────────────────
        SExpr::List(ref items) if items.is_empty() => Ok(Step::Done(Value::Nil)),

        SExpr::List(items) => eval_list(items, env),

        // Dotted lists are data, not callable forms.
        SExpr::DottedList(_, _) => Err(LispError::new("cannot evaluate dotted list as expression")),
    }
}

fn eval_list(items: Vec<SExpr>, env: Env) -> Result<Step, LispError> {
    // Every list form starts with the head.
    let head = items[0].clone();
    let tail = &items[1..];

    // Check for special forms (head must be a bare-word symbol).
    if let SExpr::Atom(ref name) = head {
        match name.as_str() {
            "quote" => return eval_quote(tail),
            "quasiquote" => {
                if tail.len() != 1 {
                    return Err(LispError::new("quasiquote: expected 1 argument"));
                }
                return Ok(Step::Done(eval_quasiquote(&tail[0], &env)?));
            }
            "if" => return eval_if(tail, env),
            "cond" => return eval_cond(tail, env),
            "when" => return eval_when(tail, env, false),
            "unless" => return eval_when(tail, env, true),
            "and" => return eval_and(tail, env),
            "or" => return eval_or(tail, env),
            "begin" => return eval_begin(tail, env),
            "define" => return eval_define(tail, env),
            "set!" => return eval_set(tail, env),
            "lambda" => return eval_lambda(tail, env),
            "defmacro" => return eval_defmacro(tail, env),
            "let" => return eval_let(tail, env),
            "let*" => return eval_let_star(tail, env),
            "letrec" => return eval_letrec(tail, env),
            "do" => return eval_do(tail, env),
            _ => {}
        }
    }

    // Function application: evaluate the head, then check for macro before args.
    let func = eval(head, Rc::clone(&env))?;

    // Macro expansion: pass args unevaluated; eval the expansion in caller env.
    if let Value::Macro {
        params,
        body,
        env: mac_env,
        ..
    } = func.clone()
    {
        let raw_args: Vec<Value> = tail.iter().map(sexpr_to_datum).collect();
        let mac_frame = EnvFrame::child(&mac_env);
        bind_params(&params, &raw_args, &mac_frame)?;
        let expansion = eval_body(&body, mac_frame)?;
        let expansion_sexpr = value_to_sexpr(expansion)
            .map_err(|e| LispError::new(format!("macro expansion error: {}", e.message)))?;
        return Ok(Step::Tail(expansion_sexpr, env));
    }

    let args: Vec<Value> = tail
        .iter()
        .map(|e| eval(e.clone(), Rc::clone(&env)))
        .collect::<Result<_, _>>()?;

    // TCO: tail-call a lambda without adding a stack frame.
    match &func {
        Value::Lambda {
            params,
            body,
            env: closure_env,
        } => {
            let frame = EnvFrame::child(closure_env);
            bind_params(params, &args, &frame)?;
            let n = body.len();
            for expr in &body[..n.saturating_sub(1)] {
                eval(expr.clone(), Rc::clone(&frame))?;
            }
            if n == 0 {
                Ok(Step::Done(Value::Nil))
            } else {
                Ok(Step::Tail(body[n - 1].clone(), frame))
            }
        }
        Value::Native(_, f) => Ok(Step::Done(f(&args)?)),
        other => Err(LispError::new(format!(
            "not a procedure: {}",
            other.type_name()
        ))),
    }
}

// ---------------------------------------------------------------------------
// Special forms
// ---------------------------------------------------------------------------

fn eval_quote(tail: &[SExpr]) -> Result<Step, LispError> {
    if tail.len() != 1 {
        return Err(LispError::new("quote: expected 1 argument"));
    }
    Ok(Step::Done(sexpr_to_datum(&tail[0])))
}

/// Convert an SExpr to a Lisp datum (used by `quote`).
pub fn sexpr_to_datum(expr: &SExpr) -> Value {
    match expr {
        SExpr::Atom(s) => {
            if s == "#t" || s == "#true" {
                return Value::Bool(true);
            }
            if s == "#f" || s == "#false" {
                return Value::Bool(false);
            }
            if let Ok(n) = s.parse::<i64>() {
                return Value::Int(n);
            }
            if s.contains('.') || s.contains('e') || s.contains('E') {
                if let Ok(f) = s.parse::<f64>() {
                    return Value::Float(f);
                }
            }
            Value::Symbol(s.clone())
        }
        SExpr::Str(s) => Value::Str(s.clone()),
        SExpr::List(items) => Value::list(items.iter().map(sexpr_to_datum)),
        SExpr::DottedList(items, tail) => {
            // Build an improper list: (a b . c) → Pair(a, Pair(b, c))
            let tail_val = sexpr_to_datum(tail);
            items.iter().rev().fold(tail_val, |acc, item| {
                Value::Pair(Rc::new((sexpr_to_datum(item), acc)))
            })
        }
    }
}

fn eval_quasiquote(template: &SExpr, env: &Env) -> Result<Value, LispError> {
    match template {
        SExpr::Atom(s) => Ok(sexpr_to_datum(&SExpr::Atom(s.clone()))),
        SExpr::Str(s) => Ok(Value::Str(s.clone())),
        SExpr::List(items) => {
            if items.is_empty() {
                return Ok(Value::Nil);
            }
            // (unquote e) → evaluate e
            if let SExpr::Atom(head) = &items[0] {
                if head == "unquote" {
                    if items.len() != 2 {
                        return Err(LispError::new("unquote: expected 1 argument"));
                    }
                    return eval(items[1].clone(), Rc::clone(env));
                }
            }
            // Build the list right-to-left so we can handle splicing.
            let mut result = Value::Nil;
            for item in items.iter().rev() {
                // (unquote-splicing e) → splice a list
                if let SExpr::List(inner) = item {
                    if inner.len() == 2 {
                        if let SExpr::Atom(head) = &inner[0] {
                            if head == "unquote-splicing" {
                                let spliced = eval(inner[1].clone(), Rc::clone(env))?;
                                result = list_append(spliced, result)?;
                                continue;
                            }
                        }
                    }
                }
                let val = eval_quasiquote(item, env)?;
                result = Value::Pair(Rc::new((val, result)));
            }
            Ok(result)
        }
        SExpr::DottedList(items, tail) => {
            // Build an improper quasiquoted list: `(a b . ,c) → Pair(a, Pair(b, eval(c)))
            let tail_val = eval_quasiquote(tail, env)?;
            let mut result = tail_val;
            for item in items.iter().rev() {
                // (unquote-splicing e) is allowed in head position
                if let SExpr::List(inner) = item {
                    if inner.len() == 2 {
                        if let SExpr::Atom(head) = &inner[0] {
                            if head == "unquote-splicing" {
                                let spliced = eval(inner[1].clone(), Rc::clone(env))?;
                                result = list_append(spliced, result)?;
                                continue;
                            }
                        }
                    }
                }
                let val = eval_quasiquote(item, env)?;
                result = Value::Pair(Rc::new((val, result)));
            }
            Ok(result)
        }
    }
}

/// Append two proper lists, returning a new list.
fn list_append(list: Value, tail: Value) -> Result<Value, LispError> {
    match list {
        Value::Nil => Ok(tail),
        Value::Pair(p) => {
            let new_tail = list_append(p.1.clone(), tail)?;
            Ok(Value::Pair(Rc::new((p.0.clone(), new_tail))))
        }
        _ => Err(LispError::new("unquote-splicing: expected a list")),
    }
}

fn eval_if(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.len() < 2 || tail.len() > 3 {
        return Err(LispError::new("if: expected 2 or 3 arguments"));
    }
    let cond_val = eval(tail[0].clone(), Rc::clone(&env))?;
    if cond_val.is_truthy() {
        Ok(Step::Tail(tail[1].clone(), env))
    } else if tail.len() == 3 {
        Ok(Step::Tail(tail[2].clone(), env))
    } else {
        Ok(Step::Done(Value::Nil))
    }
}

fn eval_cond(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    for clause in tail {
        let items = clause
            .as_list()
            .ok_or_else(|| LispError::new("cond: clause must be a list"))?;
        if items.is_empty() {
            return Err(LispError::new("cond: empty clause"));
        }
        // (else expr...) — always matches
        let is_else = matches!(&items[0], SExpr::Atom(s) if s == "else");
        if is_else || eval(items[0].clone(), Rc::clone(&env))?.is_truthy() {
            return eval_begin(&items[1..], env);
        }
    }
    Ok(Step::Done(Value::Nil))
}

/// `when negate=false`: `(when cond body...)`
/// `when negate=true`: `(unless cond body...)`
fn eval_when(tail: &[SExpr], env: Env, negate: bool) -> Result<Step, LispError> {
    if tail.is_empty() {
        return Err(LispError::new("when/unless: expected condition"));
    }
    let cond_val = eval(tail[0].clone(), Rc::clone(&env))?;
    let matches = if negate {
        !cond_val.is_truthy()
    } else {
        cond_val.is_truthy()
    };
    if matches {
        eval_begin(&tail[1..], env)
    } else {
        Ok(Step::Done(Value::Nil))
    }
}

fn eval_and(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.is_empty() {
        return Ok(Step::Done(Value::Bool(true)));
    }
    for expr in &tail[..tail.len() - 1] {
        let v = eval(expr.clone(), Rc::clone(&env))?;
        if !v.is_truthy() {
            return Ok(Step::Done(Value::Bool(false)));
        }
    }
    Ok(Step::Tail(tail[tail.len() - 1].clone(), env))
}

fn eval_or(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.is_empty() {
        return Ok(Step::Done(Value::Bool(false)));
    }
    for expr in &tail[..tail.len() - 1] {
        let v = eval(expr.clone(), Rc::clone(&env))?;
        if v.is_truthy() {
            return Ok(Step::Done(v));
        }
    }
    Ok(Step::Tail(tail[tail.len() - 1].clone(), env))
}

fn eval_begin(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.is_empty() {
        return Ok(Step::Done(Value::Nil));
    }
    for expr in &tail[..tail.len() - 1] {
        eval(expr.clone(), Rc::clone(&env))?;
    }
    Ok(Step::Tail(tail[tail.len() - 1].clone(), env))
}

fn eval_define(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.is_empty() {
        return Err(LispError::new("define: expected name"));
    }
    match &tail[0] {
        // (define name val)
        SExpr::Atom(name) | SExpr::Str(name) => {
            let name = name.clone();
            let val = if tail.len() >= 2 {
                eval(tail[1].clone(), Rc::clone(&env))?
            } else {
                Value::Nil
            };
            env.borrow_mut().define(&name, val);
            Ok(Step::Done(Value::Nil))
        }
        // (define (name params...) body...) — shorthand for lambda
        SExpr::List(sig) => {
            if sig.is_empty() {
                return Err(LispError::new("define: empty function signature"));
            }
            let name = sig[0]
                .as_atom()
                .ok_or_else(|| LispError::new("define: function name must be a symbol"))?
                .to_string();
            let params = parse_params_list(&sig[1..])?;
            let body = tail[1..].to_vec();
            let lambda = Value::Lambda {
                params,
                body,
                env: Rc::clone(&env),
            };
            env.borrow_mut().define(&name, lambda);
            Ok(Step::Done(Value::Nil))
        }
        // (define (name fixed... . rest) body...) — variadic function shorthand.
        SExpr::DottedList(sig, rest_param) => {
            if sig.is_empty() {
                return Err(LispError::new("define: empty function signature"));
            }
            let name = sig[0]
                .as_atom()
                .ok_or_else(|| LispError::new("define: function name must be a symbol"))?
                .to_string();
            let fixed: Result<Vec<_>, _> = sig[1..]
                .iter()
                .map(|e| {
                    e.as_atom()
                        .ok_or_else(|| LispError::new("lambda: parameter must be a symbol"))
                        .map(|s| s.to_string())
                })
                .collect();
            let rest = rest_param
                .as_atom()
                .ok_or_else(|| LispError::new("lambda: rest parameter must be a symbol"))?
                .to_string();
            let params = Params::Variadic(fixed?, rest);
            let body = tail[1..].to_vec();
            let lambda = Value::Lambda {
                params,
                body,
                env: Rc::clone(&env),
            };
            env.borrow_mut().define(&name, lambda);
            Ok(Step::Done(Value::Nil))
        }
    }
}

fn eval_set(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.len() != 2 {
        return Err(LispError::new("set!: expected 2 arguments"));
    }
    let name = tail[0]
        .as_atom()
        .ok_or_else(|| LispError::new("set!: name must be a symbol"))?
        .to_string();
    let val = eval(tail[1].clone(), Rc::clone(&env))?;
    env.borrow_mut().set(&name, val)?;
    Ok(Step::Done(Value::Nil))
}

fn eval_lambda(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.is_empty() {
        return Err(LispError::new("lambda: expected parameter list"));
    }
    let params = parse_params(&tail[0])?;
    let body = tail[1..].to_vec();
    Ok(Step::Done(Value::Lambda { params, body, env }))
}

/// `(defmacro name (params...) body...)` — define a macro in the current environment.
///
/// Like `lambda` but stores a `Value::Macro`.  When the macro is later called,
/// its arguments are passed unevaluated; the body is evaluated to produce an
/// expansion which is then evaluated in the caller's environment.
fn eval_defmacro(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.len() < 2 {
        return Err(LispError::new(
            "defmacro: expected name, parameter list, and body",
        ));
    }
    let name = match &tail[0] {
        SExpr::Atom(s) => s.clone(),
        _ => return Err(LispError::new("defmacro: name must be a symbol")),
    };
    let params = parse_params(&tail[1])?;
    let body = tail[2..].to_vec();
    let mac = Value::Macro {
        name: name.clone(),
        params,
        body,
        env: Rc::clone(&env),
    };
    env.borrow_mut().define(&name, mac);
    Ok(Step::Done(Value::Nil))
}

fn eval_let(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.is_empty() {
        return Err(LispError::new("let: expected bindings"));
    }
    // Named let: (let name ((x e) ...) body...)
    if let SExpr::Atom(loop_name) = &tail[0] {
        return eval_named_let(loop_name.clone(), &tail[1..], env);
    }
    let bindings = tail[0]
        .as_list()
        .ok_or_else(|| LispError::new("let: bindings must be a list"))?;
    let frame = EnvFrame::child(&env);
    for binding in bindings {
        let pair = binding
            .as_list()
            .ok_or_else(|| LispError::new("let: each binding must be a list"))?;
        if pair.len() < 2 {
            return Err(LispError::new("let: binding must have name and value"));
        }
        let name = pair[0]
            .as_atom()
            .ok_or_else(|| LispError::new("let: binding name must be a symbol"))?
            .to_string();
        let val = eval(pair[1].clone(), Rc::clone(&env))?;
        frame.borrow_mut().define(&name, val);
    }
    eval_begin(&tail[1..], frame)
}

/// Named let: `(let loop ((x init) ...) body ...)` — desugars to recursive call.
fn eval_named_let(name: String, tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.is_empty() {
        return Err(LispError::new("named let: expected bindings"));
    }
    let bindings = tail[0]
        .as_list()
        .ok_or_else(|| LispError::new("named let: bindings must be a list"))?;
    let mut param_names: Vec<String> = Vec::new();
    let mut init_vals: Vec<Value> = Vec::new();
    for b in bindings {
        let pair = b
            .as_list()
            .ok_or_else(|| LispError::new("named let: each binding must be a list"))?;
        if pair.len() < 2 {
            return Err(LispError::new(
                "named let: binding must have name and value",
            ));
        }
        let pname = pair[0]
            .as_atom()
            .ok_or_else(|| LispError::new("named let: binding name must be a symbol"))?
            .to_string();
        let init = eval(pair[1].clone(), Rc::clone(&env))?;
        param_names.push(pname);
        init_vals.push(init);
    }
    let body = tail[1..].to_vec();
    let params = Params::Fixed(param_names);
    // Create a recursive environment where `name` is bound to the lambda itself.
    let frame = EnvFrame::child(&env);
    let lambda = Value::Lambda {
        params,
        body,
        env: Rc::clone(&frame),
    };
    frame.borrow_mut().define(&name, lambda.clone());
    // Apply immediately.
    let call_frame = EnvFrame::child(&frame);
    if let Value::Lambda { params, body, .. } = &lambda {
        bind_params(params, &init_vals, &call_frame)?;
        let n = body.len();
        for expr in &body[..n.saturating_sub(1)] {
            eval(expr.clone(), Rc::clone(&call_frame))?;
        }
        if n == 0 {
            Ok(Step::Done(Value::Nil))
        } else {
            Ok(Step::Tail(body[n - 1].clone(), call_frame))
        }
    } else {
        unreachable!()
    }
}

fn eval_let_star(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.is_empty() {
        return Err(LispError::new("let*: expected bindings"));
    }
    let bindings = tail[0]
        .as_list()
        .ok_or_else(|| LispError::new("let*: bindings must be a list"))?;
    let mut frame = env;
    for binding in bindings {
        let pair = binding
            .as_list()
            .ok_or_else(|| LispError::new("let*: each binding must be a list"))?;
        if pair.len() < 2 {
            return Err(LispError::new("let*: binding must have name and value"));
        }
        let name = pair[0]
            .as_atom()
            .ok_or_else(|| LispError::new("let*: binding name must be a symbol"))?
            .to_string();
        let val = eval(pair[1].clone(), Rc::clone(&frame))?;
        let child = EnvFrame::child(&frame);
        child.borrow_mut().define(&name, val);
        frame = child;
    }
    eval_begin(&tail[1..], frame)
}

fn eval_letrec(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.is_empty() {
        return Err(LispError::new("letrec: expected bindings"));
    }
    let bindings = tail[0]
        .as_list()
        .ok_or_else(|| LispError::new("letrec: bindings must be a list"))?;
    let frame = EnvFrame::child(&env);
    // First pass: bind all names to Nil (forward declarations).
    let mut names = Vec::new();
    for binding in bindings {
        let pair = binding
            .as_list()
            .ok_or_else(|| LispError::new("letrec: each binding must be a list"))?;
        let name = pair[0]
            .as_atom()
            .ok_or_else(|| LispError::new("letrec: binding name must be a symbol"))?
            .to_string();
        frame.borrow_mut().define(&name, Value::Nil);
        names.push(name);
    }
    // Second pass: evaluate and set.
    for (i, binding) in bindings.iter().enumerate() {
        let pair = binding.as_list().unwrap();
        if pair.len() < 2 {
            return Err(LispError::new("letrec: binding must have name and value"));
        }
        let val = eval(pair[1].clone(), Rc::clone(&frame))?;
        frame.borrow_mut().set(&names[i], val)?;
    }
    eval_begin(&tail[1..], frame)
}

/// `do` loop: `(do ((var init step) ...) (test result...) body ...)`
fn eval_do(tail: &[SExpr], env: Env) -> Result<Step, LispError> {
    if tail.len() < 2 {
        return Err(LispError::new("do: expected var specs and test clause"));
    }
    let var_specs = tail[0]
        .as_list()
        .ok_or_else(|| LispError::new("do: var specs must be a list"))?;
    let test_clause = tail[1]
        .as_list()
        .ok_or_else(|| LispError::new("do: test clause must be a list"))?;
    let body = &tail[2..];

    // Parse variable specs: (var init step)
    let mut vars: Vec<(String, SExpr, Option<SExpr>)> = Vec::new();
    for spec in var_specs {
        let parts = spec
            .as_list()
            .ok_or_else(|| LispError::new("do: each var spec must be a list"))?;
        if parts.len() < 2 {
            return Err(LispError::new("do: var spec needs at least (var init)"));
        }
        let name = parts[0]
            .as_atom()
            .ok_or_else(|| LispError::new("do: var name must be a symbol"))?
            .to_string();
        let init = parts[1].clone();
        let step = parts.get(2).cloned();
        vars.push((name, init, step));
    }

    // Initialise frame.
    let mut frame = EnvFrame::child(&env);
    for (name, init, _) in &vars {
        let val = eval(init.clone(), Rc::clone(&env))?;
        frame.borrow_mut().define(name, val);
    }

    // Loop.
    loop {
        // Test.
        if test_clause.is_empty() {
            return Err(LispError::new("do: test clause is empty"));
        }
        let test_val = eval(test_clause[0].clone(), Rc::clone(&frame))?;
        if test_val.is_truthy() {
            // Return result expressions.
            if test_clause.len() > 1 {
                let result_exprs = &test_clause[1..];
                for expr in &result_exprs[..result_exprs.len().saturating_sub(1)] {
                    eval(expr.clone(), Rc::clone(&frame))?;
                }
                if !result_exprs.is_empty() {
                    let last = result_exprs[result_exprs.len() - 1].clone();
                    return Ok(Step::Tail(last, frame));
                }
            }
            return Ok(Step::Done(Value::Nil));
        }
        // Execute body.
        for expr in body {
            eval(expr.clone(), Rc::clone(&frame))?;
        }
        // Compute new step values in the OLD frame.
        let new_vals: Vec<Option<Value>> = vars
            .iter()
            .map(|(_, _, step)| {
                step.as_ref()
                    .map(|s| eval(s.clone(), Rc::clone(&frame)))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Update frame.
        let new_frame = EnvFrame::child(&env);
        for (i, (name, _, _)) in vars.iter().enumerate() {
            let val = new_vals[i]
                .clone()
                .unwrap_or_else(|| frame.borrow().lookup(name).unwrap_or(Value::Nil));
            new_frame.borrow_mut().define(name, val);
        }
        frame = new_frame;
    }
}

// ---------------------------------------------------------------------------
// Parameter parsing & binding
// ---------------------------------------------------------------------------

/// Parse a lambda parameter form: an atom or a list (possibly with `.`).
pub fn parse_params(expr: &SExpr) -> Result<Params, LispError> {
    match expr {
        SExpr::Atom(s) => Ok(Params::Rest(s.clone())),
        SExpr::Str(s) => Ok(Params::Rest(s.clone())),
        SExpr::List(items) => parse_params_list(items),
        // (lambda (a b . rest) ...) — parser now produces DottedList directly.
        SExpr::DottedList(items, tail) => {
            let fixed: Result<Vec<_>, _> = items
                .iter()
                .map(|e| {
                    e.as_atom()
                        .ok_or_else(|| LispError::new("lambda: parameter must be a symbol"))
                        .map(|s| s.to_string())
                })
                .collect();
            let rest = tail
                .as_atom()
                .ok_or_else(|| LispError::new("lambda: rest parameter must be a symbol"))?
                .to_string();
            Ok(Params::Variadic(fixed?, rest))
        }
    }
}

/// Parse `[name...]` or `[name... . rest]`.
pub fn parse_params_list(items: &[SExpr]) -> Result<Params, LispError> {
    if let Some(dot_pos) = items
        .iter()
        .position(|e| matches!(e, SExpr::Atom(s) if s == "."))
    {
        let fixed: Result<Vec<_>, _> = items[..dot_pos]
            .iter()
            .map(|e| {
                e.as_atom()
                    .ok_or_else(|| LispError::new("lambda: parameter must be a symbol"))
                    .map(|s| s.to_string())
            })
            .collect();
        let rest_part = &items[dot_pos + 1..];
        if rest_part.len() != 1 {
            return Err(LispError::new(
                "lambda: dotted params must have exactly one rest symbol",
            ));
        }
        let rest = rest_part[0]
            .as_atom()
            .ok_or_else(|| LispError::new("lambda: rest parameter must be a symbol"))?
            .to_string();
        Ok(Params::Variadic(fixed?, rest))
    } else {
        let fixed: Result<Vec<_>, _> = items
            .iter()
            .map(|e| {
                e.as_atom()
                    .ok_or_else(|| LispError::new("lambda: parameter must be a symbol"))
                    .map(|s| s.to_string())
            })
            .collect();
        Ok(Params::Fixed(fixed?))
    }
}

/// Bind `params` to `args` in `frame`.
pub fn bind_params(params: &Params, args: &[Value], frame: &Env) -> Result<(), LispError> {
    match params {
        Params::Fixed(names) => {
            if args.len() != names.len() {
                return Err(LispError::new(format!(
                    "arity mismatch: expected {} arguments, got {}",
                    names.len(),
                    args.len()
                )));
            }
            for (name, val) in names.iter().zip(args.iter()) {
                frame.borrow_mut().define(name, val.clone());
            }
        }
        Params::Variadic(fixed, rest) => {
            if args.len() < fixed.len() {
                return Err(LispError::new(format!(
                    "arity mismatch: expected at least {} arguments, got {}",
                    fixed.len(),
                    args.len()
                )));
            }
            for (name, val) in fixed.iter().zip(args.iter()) {
                frame.borrow_mut().define(name, val.clone());
            }
            let rest_val = Value::list(args[fixed.len()..].iter().cloned());
            frame.borrow_mut().define(rest, rest_val);
        }
        Params::Rest(name) => {
            let rest_val = Value::list(args.iter().cloned());
            frame.borrow_mut().define(name, rest_val);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helper: eval a body, returning the value of the last expression.
// ---------------------------------------------------------------------------

/// Evaluate a sequence of expressions, returning the value of the last one.
pub fn eval_body(body: &[SExpr], env: Env) -> Result<Value, LispError> {
    if body.is_empty() {
        return Ok(Value::Nil);
    }
    for expr in &body[..body.len() - 1] {
        eval(expr.clone(), Rc::clone(&env))?;
    }
    eval(body[body.len() - 1].clone(), env)
}
