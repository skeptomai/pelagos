//! Remora-specific Lisp builtins: `service`, `network`, `volume`, `compose`,
//! `compose-up`, `on-ready`, `env`, `log`.
//!
//! `compose-up` stores the requested spec in `PendingCompose` so the CLI can
//! run it after `eval_file` returns, keeping I/O out of the library.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::env::Env;
use super::eval::eval_apply;
use super::value::{LispError, Value};
use crate::compose::{
    BindMount, ComposeFile, Dependency, HealthCheck, NetworkSpec, PortMapping, ServiceSpec,
    VolumeMount,
};

/// Zero-argument hook closure fired after a service becomes healthy.
pub type HookFn = Rc<dyn Fn() -> Result<(), LispError>>;

/// Map of service name → list of hooks to fire when that service is ready.
pub type HookMap = HashMap<String, Vec<HookFn>>;

/// Deferred compose invocation stored by `compose-up`.
#[derive(Default)]
pub struct PendingCompose {
    pub spec: Option<ComposeFile>,
    pub project: Option<String>,
    pub foreground: bool,
}

/// Register all Remora builtins into `env`.
///
/// Hooks registered by `(on-ready ...)` are accumulated into `hooks`.
/// A `compose-up` call stores its spec in `pending`.
pub fn register_remora_builtins(
    env: &Env,
    hooks: Rc<RefCell<HookMap>>,
    pending: Rc<RefCell<PendingCompose>>,
) {
    // ── (service name opt...) → ServiceSpec ───────────────────────────────
    env.borrow_mut().define(
        "service",
        Value::Native(
            "service".into(),
            Rc::new(|args: &[Value]| -> Result<Value, LispError> {
                if args.is_empty() {
                    return Err(LispError::new("service: expected name"));
                }
                let name = str_or_sym("service", &args[0])?;
                let mut spec = ServiceSpec {
                    name: name.clone(),
                    image: String::new(),
                    networks: Vec::new(),
                    volumes: Vec::new(),
                    bind_mounts: Vec::new(),
                    tmpfs_mounts: Vec::new(),
                    env: HashMap::new(),
                    ports: Vec::new(),
                    depends_on: Vec::new(),
                    memory: None,
                    cpus: None,
                    command: None,
                    workdir: None,
                    user: None,
                };
                parse_service_opts(&mut spec, &args[1..])?;
                if spec.image.is_empty() {
                    return Err(LispError::new(format!(
                        "service '{}': missing (image ...) option",
                        name
                    )));
                }
                Ok(Value::ServiceSpec(Box::new(spec)))
            }),
        ),
    );

    // ── (network name opt...) → NetworkSpec ───────────────────────────────
    env.borrow_mut().define(
        "network",
        Value::Native(
            "network".into(),
            Rc::new(|args: &[Value]| -> Result<Value, LispError> {
                if args.is_empty() {
                    return Err(LispError::new("network: expected name"));
                }
                let name = str_or_sym("network", &args[0])?;
                let mut subnet = None;
                for opt in &args[1..] {
                    if let Value::Pair(p) = opt {
                        if let Value::Symbol(key) = &p.0 {
                            if key == "subnet" {
                                if let Value::Pair(kv) = &p.1 {
                                    subnet = Some(str_or_sym("network subnet", &kv.0)?);
                                }
                            }
                        }
                    }
                }
                Ok(Value::NetworkSpec(Box::new(NetworkSpec { name, subnet })))
            }),
        ),
    );

    // ── (volume name) → VolumeSpec ─────────────────────────────────────────
    env.borrow_mut().define(
        "volume",
        Value::Native(
            "volume".into(),
            Rc::new(|args: &[Value]| -> Result<Value, LispError> {
                if args.is_empty() {
                    return Err(LispError::new("volume: expected name"));
                }
                let name = str_or_sym("volume", &args[0])?;
                Ok(Value::VolumeSpec(name))
            }),
        ),
    );

    // ── (compose items...) → ComposeSpec ──────────────────────────────────
    // Accepts ServiceSpec, NetworkSpec, VolumeSpec, and Lisp lists thereof.
    env.borrow_mut().define(
        "compose",
        Value::Native(
            "compose".into(),
            Rc::new(|args: &[Value]| -> Result<Value, LispError> {
                let mut networks = Vec::new();
                let mut volumes = Vec::new();
                let mut services = Vec::new();
                for arg in args {
                    collect_compose_items(arg, &mut networks, &mut volumes, &mut services)?;
                }
                Ok(Value::ComposeSpec(Box::new(ComposeFile {
                    networks,
                    volumes,
                    services,
                })))
            }),
        ),
    );

    // ── (compose-up spec [project] [foreground?]) ─────────────────────────
    // Stores the spec in `pending` for the CLI to run after eval_file returns.
    {
        let pending2 = Rc::clone(&pending);
        env.borrow_mut().define(
            "compose-up",
            Value::Native(
                "compose-up".into(),
                Rc::new(move |args: &[Value]| -> Result<Value, LispError> {
                    if args.is_empty() {
                        return Err(LispError::new("compose-up: expected a compose spec"));
                    }
                    let spec = match &args[0] {
                        Value::ComposeSpec(c) => *c.clone(),
                        _ => {
                            return Err(LispError::new(format!(
                                "compose-up: expected compose-spec, got {}",
                                args[0].type_name()
                            )))
                        }
                    };
                    let project = args
                        .get(1)
                        .map(|v| match v {
                            Value::Str(s) | Value::Symbol(s) => Ok(s.clone()),
                            _ => Err(LispError::new("compose-up: project must be a string")),
                        })
                        .transpose()?;
                    let foreground = args.get(2).map(|v| v.is_truthy()).unwrap_or(false);
                    let mut p = pending2.borrow_mut();
                    p.spec = Some(spec);
                    p.project = project;
                    p.foreground = foreground;
                    Ok(Value::Nil)
                }),
            ),
        );
    }

    // ── (on-ready "svc" lambda) ────────────────────────────────────────────
    {
        let hooks2 = Rc::clone(&hooks);
        env.borrow_mut().define(
            "on-ready",
            Value::Native(
                "on-ready".into(),
                Rc::new(move |args: &[Value]| -> Result<Value, LispError> {
                    if args.len() != 2 {
                        return Err(LispError::new("on-ready: expected service name and lambda"));
                    }
                    let svc_name = str_or_sym("on-ready", &args[0])?;
                    let lambda = args[1].clone();
                    // Verify it's callable.
                    match &lambda {
                        Value::Lambda { .. } | Value::Native(_, _) => {}
                        _ => {
                            return Err(LispError::new(format!(
                                "on-ready: second argument must be a procedure, got {}",
                                lambda.type_name()
                            )))
                        }
                    }
                    // Wrap into a zero-arg Rust closure.
                    let hook: HookFn = Rc::new(move || eval_apply(&lambda, &[]).map(|_| ()));
                    hooks2.borrow_mut().entry(svc_name).or_default().push(hook);
                    Ok(Value::Nil)
                }),
            ),
        );
    }

    // ── (env "VAR") → string | () ─────────────────────────────────────────
    env.borrow_mut().define(
        "env",
        Value::Native(
            "env".into(),
            Rc::new(|args: &[Value]| -> Result<Value, LispError> {
                if args.is_empty() {
                    return Err(LispError::new("env: expected variable name"));
                }
                let name = str_or_sym("env", &args[0])?;
                match std::env::var(&name) {
                    Ok(v) => Ok(Value::Str(v)),
                    Err(_) => Ok(Value::Nil),
                }
            }),
        ),
    );

    // ── (log msg...) → () ─────────────────────────────────────────────────
    env.borrow_mut().define(
        "log",
        Value::Native(
            "log".into(),
            Rc::new(|args: &[Value]| -> Result<Value, LispError> {
                let parts: Vec<String> = args
                    .iter()
                    .map(|v| match v {
                        Value::Str(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                log::info!("[lisp] {}", parts.join(" "));
                Ok(Value::Nil)
            }),
        ),
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn str_or_sym(ctx: &str, v: &Value) -> Result<String, LispError> {
    match v {
        Value::Str(s) | Value::Symbol(s) => Ok(s.clone()),
        _ => Err(LispError::new(format!(
            "{}: expected string or symbol, got {}",
            ctx,
            v.type_name()
        ))),
    }
}

/// Flatten `ServiceSpec`, `NetworkSpec`, `VolumeSpec`, and proper lists of
/// such values into the three accumulator vecs.
fn collect_compose_items(
    val: &Value,
    networks: &mut Vec<NetworkSpec>,
    volumes: &mut Vec<String>,
    services: &mut Vec<ServiceSpec>,
) -> Result<(), LispError> {
    match val {
        Value::ServiceSpec(s) => services.push(*s.clone()),
        Value::NetworkSpec(n) => networks.push(*n.clone()),
        Value::VolumeSpec(v) => volumes.push(v.clone()),
        Value::Nil => {}
        Value::Pair(_) => {
            // Flatten one level of Lisp list.
            let items = val.to_vec()?;
            for item in items {
                collect_compose_items(&item, networks, volumes, services)?;
            }
        }
        other => {
            return Err(LispError::new(format!(
                "compose: unexpected value {} ({})",
                other,
                other.type_name()
            )))
        }
    }
    Ok(())
}

/// Parse `(image ...)`, `(network ...)`, `(env ...)`, `(port ...)`,
/// `(depends-on ...)`, `(memory ...)`, `(cpus ...)`, `(command ...)`,
/// `(workdir ...)`, `(user ...)` options into `spec`.
///
/// Options may be passed as Lisp lists `(key val...)` or as keyword pairs.
fn parse_service_opts(spec: &mut ServiceSpec, opts: &[Value]) -> Result<(), LispError> {
    for opt in opts {
        match opt {
            // Each option is a list: (keyword args...)
            Value::Pair(_) => {
                let items = opt.to_vec()?;
                if items.is_empty() {
                    continue;
                }
                let key = str_or_sym("service option", &items[0])?;
                let vals = &items[1..];
                apply_service_opt(spec, &key, vals)?;
            }
            // Bare service specs appended as-is (not typical but safe to ignore).
            _ => {
                return Err(LispError::new(format!(
                    "service option must be a list, got {}",
                    opt.type_name()
                )))
            }
        }
    }
    Ok(())
}

fn apply_service_opt(spec: &mut ServiceSpec, key: &str, vals: &[Value]) -> Result<(), LispError> {
    match key {
        "image" => {
            spec.image = str_or_sym_at("image", vals, 0)?;
        }
        "network" => {
            for v in vals {
                spec.networks.push(str_or_sym("network", v)?);
            }
        }
        "env" => {
            let k = str_or_sym_at("env key", vals, 0)?;
            let v = str_or_sym_at("env value", vals, 1)?;
            spec.env.insert(k, v);
        }
        "port" => {
            let host = parse_port("port host", &vals[0])?;
            let container = parse_port("port container", &vals[1])?;
            spec.ports.push(PortMapping { host, container });
        }
        "depends-on" => {
            // Accepts one dependency per option call:
            //   (list 'depends-on "svc")        — process-alive only
            //   (list 'depends-on "svc" 6379)   — TCP port readiness check
            let dep_name = str_or_sym_at("depends-on", vals, 0)?;
            let health_check = match vals.get(1) {
                Some(Value::Int(port)) => {
                    let p = u16::try_from(*port)
                        .map_err(|_| LispError::new("depends-on: port out of range"))?;
                    Some(HealthCheck::Port(p))
                }
                Some(_) => {
                    return Err(LispError::new(
                        "depends-on: optional second arg must be a port number",
                    ))
                }
                None => None,
            };
            spec.depends_on.push(Dependency {
                service: dep_name,
                health_check,
            });
        }
        "memory" => {
            spec.memory = Some(str_or_sym_at("memory", vals, 0)?);
        }
        "cpus" => {
            spec.cpus = Some(str_or_sym_at("cpus", vals, 0)?);
        }
        "command" => {
            spec.command = Some(
                vals.iter()
                    .map(|v| str_or_sym("command arg", v))
                    .collect::<Result<_, _>>()?,
            );
        }
        "tmpfs" => {
            let path = str_or_sym_at("tmpfs", vals, 0)?;
            spec.tmpfs_mounts.push(path);
        }
        "volume" => {
            let name = str_or_sym_at("volume name", vals, 0)?;
            let mount_path = str_or_sym_at("volume mount_path", vals, 1)?;
            spec.volumes.push(VolumeMount { name, mount_path });
        }
        "bind" => {
            let host_path = str_or_sym_at("bind host_path", vals, 0)?;
            let container_path = str_or_sym_at("bind container_path", vals, 1)?;
            spec.bind_mounts.push(BindMount {
                host_path,
                container_path,
                read_only: false,
            });
        }
        "bind-ro" => {
            let host_path = str_or_sym_at("bind-ro host_path", vals, 0)?;
            let container_path = str_or_sym_at("bind-ro container_path", vals, 1)?;
            spec.bind_mounts.push(BindMount {
                host_path,
                container_path,
                read_only: true,
            });
        }
        "workdir" => {
            spec.workdir = Some(str_or_sym_at("workdir", vals, 0)?);
        }
        "user" => {
            spec.user = Some(str_or_sym_at("user", vals, 0)?);
        }
        other => {
            return Err(LispError::new(format!(
                "service: unknown option '{}'",
                other
            )))
        }
    }
    Ok(())
}

fn str_or_sym_at(ctx: &str, vals: &[Value], idx: usize) -> Result<String, LispError> {
    vals.get(idx)
        .ok_or_else(|| LispError::new(format!("{}: missing value", ctx)))
        .and_then(|v| str_or_sym(ctx, v))
}

fn parse_port(ctx: &str, v: &Value) -> Result<u16, LispError> {
    match v {
        Value::Int(n) => {
            u16::try_from(*n).map_err(|_| LispError::new(format!("{}: port out of range", ctx)))
        }
        Value::Str(s) | Value::Symbol(s) => s
            .parse::<u16>()
            .map_err(|_| LispError::new(format!("{}: invalid port '{}'", ctx, s))),
        _ => Err(LispError::new(format!("{}: expected integer port", ctx))),
    }
}
