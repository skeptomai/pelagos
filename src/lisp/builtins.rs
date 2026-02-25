//! Standard Lisp primitives registered into a global environment.
//!
//! ~55 functions covering: arithmetic, comparison, boolean, pairs/lists,
//! higher-order, strings, symbols, type predicates, and I/O.

use std::rc::Rc;

use super::env::Env;
use super::eval::eval_apply;
use super::value::{LispError, NativeFn, Value};

/// Register all standard builtins into `env`.
pub fn register_builtins(env: &Env) {
    // ── Arithmetic ─────────────────────────────────────────────────────────
    native(env, "+", |args| {
        let mut acc_i: i64 = 0;
        let mut acc_f: f64 = 0.0;
        let mut is_float = false;
        for a in args {
            match a {
                Value::Int(n) => {
                    acc_i = acc_i.wrapping_add(*n);
                    acc_f += *n as f64;
                }
                Value::Float(f) => {
                    is_float = true;
                    acc_f += f;
                }
                _ => return Err(type_err("+", "number", a)),
            }
        }
        if is_float {
            Ok(Value::Float(acc_f))
        } else {
            Ok(Value::Int(acc_i))
        }
    });

    native(env, "-", |args| {
        if args.is_empty() {
            return Err(LispError::new("-: requires at least 1 argument"));
        }
        if args.len() == 1 {
            return match &args[0] {
                Value::Int(n) => Ok(Value::Int(-n)),
                Value::Float(f) => Ok(Value::Float(-f)),
                a => Err(type_err("-", "number", a)),
            };
        }
        let mut is_float = matches!(&args[0], Value::Float(_));
        let mut acc_i = int_val("-", &args[0])?;
        let mut acc_f = float_val(&args[0]);
        for a in &args[1..] {
            match a {
                Value::Int(n) => {
                    acc_i = acc_i.wrapping_sub(*n);
                    acc_f -= *n as f64;
                }
                Value::Float(f) => {
                    is_float = true;
                    acc_f -= f;
                }
                _ => return Err(type_err("-", "number", a)),
            }
        }
        if is_float {
            Ok(Value::Float(acc_f))
        } else {
            Ok(Value::Int(acc_i))
        }
    });

    native(env, "*", |args| {
        let mut acc_i: i64 = 1;
        let mut acc_f: f64 = 1.0;
        let mut is_float = false;
        for a in args {
            match a {
                Value::Int(n) => {
                    acc_i = acc_i.wrapping_mul(*n);
                    acc_f *= *n as f64;
                }
                Value::Float(f) => {
                    is_float = true;
                    acc_f *= f;
                }
                _ => return Err(type_err("*", "number", a)),
            }
        }
        if is_float {
            Ok(Value::Float(acc_f))
        } else {
            Ok(Value::Int(acc_i))
        }
    });

    native(env, "/", |args| {
        if args.is_empty() {
            return Err(LispError::new("/: requires at least 1 argument"));
        }
        if args.len() == 1 {
            return match &args[0] {
                Value::Int(n) => Ok(Value::Float(1.0 / *n as f64)),
                Value::Float(f) => Ok(Value::Float(1.0 / f)),
                a => Err(type_err("/", "number", a)),
            };
        }
        let first_float = matches!(&args[0], Value::Float(_));
        let first_i = int_val("/", &args[0])?;
        let mut acc_f = float_val(&args[0]);
        let mut is_float = first_float;
        let mut acc_i = first_i;
        for a in &args[1..] {
            match a {
                Value::Int(0) => return Err(LispError::new("/: division by zero")),
                Value::Int(n) => {
                    if !is_float && acc_i % n == 0 {
                        acc_i /= n;
                    } else {
                        is_float = true;
                    }
                    acc_f /= *n as f64;
                }
                Value::Float(f) if *f == 0.0 => return Err(LispError::new("/: division by zero")),
                Value::Float(f) => {
                    is_float = true;
                    acc_f /= f;
                }
                _ => return Err(type_err("/", "number", a)),
            }
        }
        if is_float {
            Ok(Value::Float(acc_f))
        } else {
            Ok(Value::Int(acc_i))
        }
    });

    native(env, "quotient", |args| {
        check_arity("quotient", 2, args)?;
        let a = int_val("quotient", &args[0])?;
        let b = int_val("quotient", &args[1])?;
        if b == 0 {
            return Err(LispError::new("quotient: division by zero"));
        }
        Ok(Value::Int(a / b))
    });

    native(env, "remainder", |args| {
        check_arity("remainder", 2, args)?;
        let a = int_val("remainder", &args[0])?;
        let b = int_val("remainder", &args[1])?;
        if b == 0 {
            return Err(LispError::new("remainder: division by zero"));
        }
        Ok(Value::Int(a % b))
    });

    native(env, "modulo", |args| {
        check_arity("modulo", 2, args)?;
        let a = int_val("modulo", &args[0])?;
        let b = int_val("modulo", &args[1])?;
        if b == 0 {
            return Err(LispError::new("modulo: division by zero"));
        }
        Ok(Value::Int(((a % b) + b) % b))
    });

    native(env, "abs", |args| {
        check_arity("abs", 1, args)?;
        match &args[0] {
            Value::Int(n) => Ok(Value::Int(n.abs())),
            Value::Float(f) => Ok(Value::Float(f.abs())),
            a => Err(type_err("abs", "number", a)),
        }
    });

    native(env, "min", |args| {
        if args.is_empty() {
            return Err(LispError::new("min: requires at least 1 argument"));
        }
        let mut is_float = false;
        let mut min_i = i64::MAX;
        let mut min_f = f64::INFINITY;
        for a in args {
            match a {
                Value::Int(n) => {
                    if *n < min_i {
                        min_i = *n;
                    }
                    if (*n as f64) < min_f {
                        min_f = *n as f64;
                    }
                }
                Value::Float(f) => {
                    is_float = true;
                    if *f < min_f {
                        min_f = *f;
                    }
                }
                _ => return Err(type_err("min", "number", a)),
            }
        }
        if is_float {
            Ok(Value::Float(min_f))
        } else {
            Ok(Value::Int(min_i))
        }
    });

    native(env, "max", |args| {
        if args.is_empty() {
            return Err(LispError::new("max: requires at least 1 argument"));
        }
        let mut is_float = false;
        let mut max_i = i64::MIN;
        let mut max_f = f64::NEG_INFINITY;
        for a in args {
            match a {
                Value::Int(n) => {
                    if *n > max_i {
                        max_i = *n;
                    }
                    if (*n as f64) > max_f {
                        max_f = *n as f64;
                    }
                }
                Value::Float(f) => {
                    is_float = true;
                    if *f > max_f {
                        max_f = *f;
                    }
                }
                _ => return Err(type_err("max", "number", a)),
            }
        }
        if is_float {
            Ok(Value::Float(max_f))
        } else {
            Ok(Value::Int(max_i))
        }
    });

    native(env, "expt", |args| {
        check_arity("expt", 2, args)?;
        match (&args[0], &args[1]) {
            (Value::Int(b), Value::Int(e)) if *e >= 0 => Ok(Value::Int(b.wrapping_pow(*e as u32))),
            _ => {
                let b = to_float("expt", &args[0])?;
                let e = to_float("expt", &args[1])?;
                Ok(Value::Float(b.powf(e)))
            }
        }
    });

    // ── Comparison ─────────────────────────────────────────────────────────
    for op in &["=", "<", ">", "<=", ">="] {
        let op_str = op.to_string();
        let f: NativeFn = Rc::new(move |args: &[Value]| -> Result<Value, LispError> {
            if args.len() < 2 {
                return Err(LispError::new(format!(
                    "{}: requires at least 2 arguments",
                    op_str
                )));
            }
            let result = args.windows(2).all(|w| match cmp_nums(&w[0], &w[1]) {
                Ok(ord) => match op_str.as_str() {
                    "=" => ord == std::cmp::Ordering::Equal,
                    "<" => ord == std::cmp::Ordering::Less,
                    ">" => ord == std::cmp::Ordering::Greater,
                    "<=" => ord != std::cmp::Ordering::Greater,
                    ">=" => ord != std::cmp::Ordering::Less,
                    _ => false,
                },
                Err(_) => false,
            });
            Ok(Value::Bool(result))
        });
        env.borrow_mut()
            .define(op, Value::Native(op.to_string(), f));
    }

    native(env, "equal?", |args| {
        check_arity("equal?", 2, args)?;
        Ok(Value::Bool(values_equal(&args[0], &args[1])))
    });

    native(env, "eqv?", |args| {
        check_arity("eqv?", 2, args)?;
        Ok(Value::Bool(values_eqv(&args[0], &args[1])))
    });

    native(env, "eq?", |args| {
        check_arity("eq?", 2, args)?;
        Ok(Value::Bool(values_eqv(&args[0], &args[1])))
    });

    // ── Boolean ────────────────────────────────────────────────────────────
    native(env, "not", |args| {
        check_arity("not", 1, args)?;
        Ok(Value::Bool(!args[0].is_truthy()))
    });

    native(env, "boolean?", |args| {
        check_arity("boolean?", 1, args)?;
        Ok(Value::Bool(matches!(&args[0], Value::Bool(_))))
    });

    // ── Pairs / Lists ──────────────────────────────────────────────────────
    native(env, "cons", |args| {
        check_arity("cons", 2, args)?;
        Ok(Value::Pair(Rc::new((args[0].clone(), args[1].clone()))))
    });

    native(env, "car", |args| {
        check_arity("car", 1, args)?;
        match &args[0] {
            Value::Pair(p) => Ok(p.0.clone()),
            a => Err(type_err("car", "pair", a)),
        }
    });

    native(env, "cdr", |args| {
        check_arity("cdr", 1, args)?;
        match &args[0] {
            Value::Pair(p) => Ok(p.1.clone()),
            a => Err(type_err("cdr", "pair", a)),
        }
    });

    native(env, "cadr", |args| {
        check_arity("cadr", 1, args)?;
        let cdr = cdr_of("cadr", &args[0])?;
        car_of("cadr", &cdr)
    });

    native(env, "caddr", |args| {
        check_arity("caddr", 1, args)?;
        let cdr = cdr_of("caddr", &args[0])?;
        let cddr = cdr_of("caddr", &cdr)?;
        car_of("caddr", &cddr)
    });

    native(env, "caar", |args| {
        check_arity("caar", 1, args)?;
        car_of("caar", &car_of("caar", &args[0])?)
    });

    native(env, "cdar", |args| {
        check_arity("cdar", 1, args)?;
        cdr_of("cdar", &car_of("cdar", &args[0])?)
    });

    native(env, "cddr", |args| {
        check_arity("cddr", 1, args)?;
        cdr_of("cddr", &cdr_of("cddr", &args[0])?)
    });

    native(env, "caddr", |args| {
        check_arity("caddr", 1, args)?;
        let v = cdr_of("caddr", &cdr_of("caddr", &args[0])?)?;
        car_of("caddr", &v)
    });

    native(env, "list", |args| Ok(Value::list(args.iter().cloned())));

    native(env, "null?", |args| {
        check_arity("null?", 1, args)?;
        Ok(Value::Bool(matches!(&args[0], Value::Nil)))
    });

    native(env, "pair?", |args| {
        check_arity("pair?", 1, args)?;
        Ok(Value::Bool(matches!(&args[0], Value::Pair(_))))
    });

    native(env, "list?", |args| {
        check_arity("list?", 1, args)?;
        Ok(Value::Bool(args[0].is_list()))
    });

    native(env, "length", |args| {
        check_arity("length", 1, args)?;
        let vec = args[0].to_vec()?;
        Ok(Value::Int(vec.len() as i64))
    });

    native(env, "append", |args| {
        if args.is_empty() {
            return Ok(Value::Nil);
        }
        let mut result = args.last().unwrap().clone();
        for list in args[..args.len() - 1].iter().rev() {
            let items = list.to_vec()?;
            for item in items.into_iter().rev() {
                result = Value::Pair(Rc::new((item, result)));
            }
        }
        Ok(result)
    });

    native(env, "reverse", |args| {
        check_arity("reverse", 1, args)?;
        let items = args[0].to_vec()?;
        Ok(Value::list(items.into_iter().rev()))
    });

    native(env, "list-ref", |args| {
        check_arity("list-ref", 2, args)?;
        let idx = int_val("list-ref", &args[1])? as usize;
        let items = args[0].to_vec()?;
        items
            .into_iter()
            .nth(idx)
            .ok_or_else(|| LispError::new(format!("list-ref: index {} out of range", idx)))
    });

    native(env, "iota", |args| {
        if args.is_empty() || args.len() > 3 {
            return Err(LispError::new("iota: expected 1–3 arguments"));
        }
        let count = int_val("iota", &args[0])? as usize;
        let start = if args.len() >= 2 {
            int_val("iota", &args[1])?
        } else {
            0
        };
        let step = if args.len() == 3 {
            int_val("iota", &args[2])?
        } else {
            1
        };
        Ok(Value::list(
            (0..count).map(|i| Value::Int(start + (i as i64) * step)),
        ))
    });

    native(env, "assoc", |args| {
        check_arity("assoc", 2, args)?;
        let key = &args[0];
        let list = args[1].to_vec()?;
        for item in list {
            if let Value::Pair(p) = &item {
                if values_equal(&p.0, key) {
                    return Ok(item.clone());
                }
            }
        }
        Ok(Value::Bool(false))
    });

    native(env, "assv", |args| {
        check_arity("assv", 2, args)?;
        let key = &args[0];
        let list = args[1].to_vec()?;
        for item in list {
            if let Value::Pair(p) = &item {
                if values_eqv(&p.0, key) {
                    return Ok(item.clone());
                }
            }
        }
        Ok(Value::Bool(false))
    });

    // ── Higher-order ───────────────────────────────────────────────────────
    native(env, "map", |args| {
        if args.len() < 2 {
            return Err(LispError::new(
                "map: requires function and at least one list",
            ));
        }
        let func = args[0].clone();
        let lists: Vec<Vec<Value>> = args[1..]
            .iter()
            .map(|l| l.to_vec())
            .collect::<Result<_, _>>()?;
        let len = lists[0].len();
        for l in &lists[1..] {
            if l.len() != len {
                return Err(LispError::new("map: all lists must have the same length"));
            }
        }
        let mut results = Vec::with_capacity(len);
        for i in 0..len {
            let call_args: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            results.push(eval_apply(&func, &call_args)?);
        }
        Ok(Value::list(results.into_iter()))
    });

    native(env, "filter", |args| {
        check_arity("filter", 2, args)?;
        let func = args[0].clone();
        let items = args[1].to_vec()?;
        let mut results = Vec::new();
        for item in items {
            if eval_apply(&func, std::slice::from_ref(&item))?.is_truthy() {
                results.push(item);
            }
        }
        Ok(Value::list(results.into_iter()))
    });

    native(env, "for-each", |args| {
        if args.len() < 2 {
            return Err(LispError::new(
                "for-each: requires function and at least one list",
            ));
        }
        let func = args[0].clone();
        let lists: Vec<Vec<Value>> = args[1..]
            .iter()
            .map(|l| l.to_vec())
            .collect::<Result<_, _>>()?;
        let len = lists[0].len();
        for i in 0..len {
            let call_args: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            eval_apply(&func, &call_args)?;
        }
        Ok(Value::Nil)
    });

    native(env, "apply", |args| {
        if args.len() < 2 {
            return Err(LispError::new("apply: requires function and argument list"));
        }
        let func = args[0].clone();
        let mut call_args: Vec<Value> = args[1..args.len() - 1].to_vec();
        let last = &args[args.len() - 1];
        let tail_items = last.to_vec()?;
        call_args.extend(tail_items);
        eval_apply(&func, &call_args)
    });

    native(env, "fold-left", |args| {
        check_arity("fold-left", 3, args)?;
        let func = args[0].clone();
        let mut acc = args[1].clone();
        let items = args[2].to_vec()?;
        for item in items {
            acc = eval_apply(&func, &[acc, item])?;
        }
        Ok(acc)
    });

    native(env, "fold-right", |args| {
        check_arity("fold-right", 3, args)?;
        let func = args[0].clone();
        let mut acc = args[1].clone();
        let items = args[2].to_vec()?;
        for item in items.into_iter().rev() {
            acc = eval_apply(&func, &[item, acc])?;
        }
        Ok(acc)
    });

    // ── Strings ────────────────────────────────────────────────────────────
    native(env, "string?", |args| {
        check_arity("string?", 1, args)?;
        Ok(Value::Bool(matches!(&args[0], Value::Str(_))))
    });

    native(env, "string-append", |args| {
        let mut s = String::new();
        for a in args {
            match a {
                Value::Str(t) => s.push_str(t),
                _ => return Err(type_err("string-append", "string", a)),
            }
        }
        Ok(Value::Str(s))
    });

    native(env, "string-length", |args| {
        check_arity("string-length", 1, args)?;
        match &args[0] {
            Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
            a => Err(type_err("string-length", "string", a)),
        }
    });

    native(env, "substring", |args| {
        if args.len() < 2 || args.len() > 3 {
            return Err(LispError::new("substring: expected 2 or 3 arguments"));
        }
        let s = str_val("substring", &args[0])?;
        let chars: Vec<char> = s.chars().collect();
        let start = int_val("substring", &args[1])? as usize;
        let end = if args.len() == 3 {
            int_val("substring", &args[2])? as usize
        } else {
            chars.len()
        };
        if start > end || end > chars.len() {
            return Err(LispError::new("substring: index out of range"));
        }
        Ok(Value::Str(chars[start..end].iter().collect()))
    });

    native(env, "string->number", |args| {
        if args.is_empty() || args.len() > 2 {
            return Err(LispError::new("string->number: expected 1 or 2 arguments"));
        }
        let s = str_val("string->number", &args[0])?;
        let radix = if args.len() == 2 {
            int_val("string->number", &args[1])? as u32
        } else {
            10
        };
        if radix == 10 {
            if let Ok(n) = s.parse::<i64>() {
                return Ok(Value::Int(n));
            }
            if let Ok(f) = s.parse::<f64>() {
                return Ok(Value::Float(f));
            }
        } else if let Ok(n) = i64::from_str_radix(&s, radix) {
            return Ok(Value::Int(n));
        }
        Ok(Value::Bool(false))
    });

    native(env, "number->string", |args| {
        if args.is_empty() || args.len() > 2 {
            return Err(LispError::new("number->string: expected 1 or 2 arguments"));
        }
        match &args[0] {
            Value::Int(n) => {
                let radix = if args.len() == 2 {
                    int_val("number->string", &args[1])? as u32
                } else {
                    10
                };
                let s = match radix {
                    2 => format!("{:b}", n),
                    8 => format!("{:o}", n),
                    10 => n.to_string(),
                    16 => format!("{:x}", n),
                    _ => return Err(LispError::new("number->string: unsupported radix")),
                };
                Ok(Value::Str(s))
            }
            Value::Float(f) => Ok(Value::Str(f.to_string())),
            a => Err(type_err("number->string", "number", a)),
        }
    });

    native(env, "string-upcase", |args| {
        check_arity("string-upcase", 1, args)?;
        Ok(Value::Str(
            str_val("string-upcase", &args[0])?.to_uppercase(),
        ))
    });

    native(env, "string-downcase", |args| {
        check_arity("string-downcase", 1, args)?;
        Ok(Value::Str(
            str_val("string-downcase", &args[0])?.to_lowercase(),
        ))
    });

    native(env, "string=?", |args| {
        check_arity("string=?", 2, args)?;
        Ok(Value::Bool(
            str_val("string=?", &args[0])? == str_val("string=?", &args[1])?,
        ))
    });

    native(env, "string<?", |args| {
        check_arity("string<?", 2, args)?;
        Ok(Value::Bool(
            str_val("string<?", &args[0])? < str_val("string<?", &args[1])?,
        ))
    });

    native(env, "string>?", |args| {
        check_arity("string>?", 2, args)?;
        Ok(Value::Bool(
            str_val("string>?", &args[0])? > str_val("string>?", &args[1])?,
        ))
    });

    native(env, "string-contains", |args| {
        check_arity("string-contains", 2, args)?;
        let haystack = str_val("string-contains", &args[0])?;
        let needle = str_val("string-contains", &args[1])?;
        Ok(Value::Bool(haystack.contains(needle.as_str())))
    });

    // ── Symbols ────────────────────────────────────────────────────────────
    native(env, "symbol?", |args| {
        check_arity("symbol?", 1, args)?;
        Ok(Value::Bool(matches!(&args[0], Value::Symbol(_))))
    });

    native(env, "symbol->string", |args| {
        check_arity("symbol->string", 1, args)?;
        match &args[0] {
            Value::Symbol(s) => Ok(Value::Str(s.clone())),
            a => Err(type_err("symbol->string", "symbol", a)),
        }
    });

    native(env, "string->symbol", |args| {
        check_arity("string->symbol", 1, args)?;
        match &args[0] {
            Value::Str(s) => Ok(Value::Symbol(s.clone())),
            a => Err(type_err("string->symbol", "string", a)),
        }
    });

    // ── Type predicates ────────────────────────────────────────────────────
    native(env, "number?", |args| {
        check_arity("number?", 1, args)?;
        Ok(Value::Bool(matches!(
            &args[0],
            Value::Int(_) | Value::Float(_)
        )))
    });

    native(env, "integer?", |args| {
        check_arity("integer?", 1, args)?;
        Ok(Value::Bool(match &args[0] {
            Value::Int(_) => true,
            Value::Float(f) => f.fract() == 0.0,
            _ => false,
        }))
    });

    native(env, "procedure?", |args| {
        check_arity("procedure?", 1, args)?;
        Ok(Value::Bool(matches!(
            &args[0],
            Value::Lambda { .. } | Value::Native(_, _)
        )))
    });

    // ── I/O ───────────────────────────────────────────────────────────────
    native(env, "display", |args| {
        if args.is_empty() {
            return Err(LispError::new("display: requires at least 1 argument"));
        }
        // Strings are displayed without quotes.
        match &args[0] {
            Value::Str(s) => print!("{}", s),
            v => print!("{}", v),
        }
        Ok(Value::Nil)
    });

    native(env, "newline", |_args| {
        println!();
        Ok(Value::Nil)
    });

    native(env, "error", |args| {
        let msg = if args.is_empty() {
            "error".to_string()
        } else {
            match &args[0] {
                Value::Str(s) => {
                    let rest: Vec<String> = args[1..].iter().map(|v| v.to_string()).collect();
                    if rest.is_empty() {
                        s.clone()
                    } else {
                        format!("{} {}", s, rest.join(" "))
                    }
                }
                v => v.to_string(),
            }
        };
        Err(LispError::new(msg))
    });

    native(env, "write", |args| {
        if args.is_empty() {
            return Err(LispError::new("write: requires at least 1 argument"));
        }
        print!("{}", args[0]);
        Ok(Value::Nil)
    });

    // ── Format string ──────────────────────────────────────────────────────
    // (format fmt arg...)
    // ~a = display: strings without quotes, numbers as-is
    // ~s = write:   all values in their Value::Display form (strings WITH quotes)
    native(env, "format", |args| {
        if args.is_empty() {
            return Err(LispError::new("format: requires format string"));
        }
        let fmt = str_val("format", &args[0])?;
        let mut result = String::new();
        let mut arg_idx = 1usize;
        let mut chars = fmt.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '~' {
                match chars.next() {
                    Some('a') | Some('A') => {
                        if arg_idx >= args.len() {
                            return Err(LispError::new("format: too few arguments"));
                        }
                        // Display: strings without quotes.
                        match &args[arg_idx] {
                            Value::Str(s) => result.push_str(s),
                            v => result.push_str(&v.to_string()),
                        }
                        arg_idx += 1;
                    }
                    Some('s') | Some('S') => {
                        if arg_idx >= args.len() {
                            return Err(LispError::new("format: too few arguments"));
                        }
                        // Write: use Value's Display (strings have quotes).
                        result.push_str(&args[arg_idx].to_string());
                        arg_idx += 1;
                    }
                    Some('%') => result.push('\n'),
                    Some('~') => result.push('~'),
                    Some(other) => {
                        result.push('~');
                        result.push(other);
                    }
                    None => result.push('~'),
                }
            } else {
                result.push(c);
            }
        }
        Ok(Value::Str(result))
    });

    // ── Sleep ──────────────────────────────────────────────────────────────
    // (sleep secs) — accepts int or float; returns ()
    native(env, "sleep", |args| {
        check_arity("sleep", 1, args)?;
        let secs = match &args[0] {
            Value::Int(n) => *n as f64,
            Value::Float(f) => *f,
            a => return Err(type_err("sleep", "number", a)),
        };
        if secs > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(secs));
        }
        Ok(Value::Nil)
    });
}

// ---------------------------------------------------------------------------
// Registration helper
// ---------------------------------------------------------------------------

fn native<F>(env: &Env, name: &str, f: F)
where
    F: Fn(&[Value]) -> Result<Value, LispError> + 'static,
{
    env.borrow_mut()
        .define(name, Value::Native(name.to_string(), Rc::new(f)));
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn check_arity(name: &str, expected: usize, args: &[Value]) -> Result<(), LispError> {
    if args.len() != expected {
        Err(LispError::new(format!(
            "{}: expected {} argument(s), got {}",
            name,
            expected,
            args.len()
        )))
    } else {
        Ok(())
    }
}

fn type_err(name: &str, expected: &str, got: &Value) -> LispError {
    LispError::new(format!(
        "{}: expected {}, got {}",
        name,
        expected,
        got.type_name()
    ))
}

fn int_val(name: &str, v: &Value) -> Result<i64, LispError> {
    match v {
        Value::Int(n) => Ok(*n),
        Value::Float(f) if f.fract() == 0.0 => Ok(*f as i64),
        _ => Err(type_err(name, "integer", v)),
    }
}

fn float_val(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Float(f) => *f,
        _ => f64::NAN,
    }
}

fn to_float(name: &str, v: &Value) -> Result<f64, LispError> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        _ => Err(type_err(name, "number", v)),
    }
}

fn str_val(name: &str, v: &Value) -> Result<String, LispError> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        _ => Err(type_err(name, "string", v)),
    }
}

fn car_of(name: &str, v: &Value) -> Result<Value, LispError> {
    match v {
        Value::Pair(p) => Ok(p.0.clone()),
        _ => Err(type_err(name, "pair", v)),
    }
}

fn cdr_of(name: &str, v: &Value) -> Result<Value, LispError> {
    match v {
        Value::Pair(p) => Ok(p.1.clone()),
        _ => Err(type_err(name, "pair", v)),
    }
}

fn cmp_nums(a: &Value, b: &Value) -> Result<std::cmp::Ordering, LispError> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x
            .partial_cmp(y)
            .ok_or_else(|| LispError::new("comparison of NaN")),
        (Value::Int(x), Value::Float(y)) => (*x as f64)
            .partial_cmp(y)
            .ok_or_else(|| LispError::new("comparison of NaN")),
        (Value::Float(x), Value::Int(y)) => x
            .partial_cmp(&(*y as f64))
            .ok_or_else(|| LispError::new("comparison of NaN")),
        _ => Err(LispError::new("comparison requires numbers")),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Symbol(x), Value::Symbol(y)) => x == y,
        (Value::Pair(x), Value::Pair(y)) => values_equal(&x.0, &y.0) && values_equal(&x.1, &y.1),
        _ => false,
    }
}

fn values_eqv(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => std::ptr::eq(x.as_str(), y.as_str()) || x == y,
        (Value::Symbol(x), Value::Symbol(y)) => x == y,
        _ => false,
    }
}
