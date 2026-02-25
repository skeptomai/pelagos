//! Imperative runtime builtins for the Lisp interpreter.
//!
//! Registered only when the interpreter is created via
//! [`Interpreter::new_with_runtime`].  Not available in plain `Interpreter::new()`.
//!
//! # Available functions
//!
//! | Function | Signature | Description |
//! |----------|-----------|-------------|
//! | `container-start` | `(svc-spec)` → ContainerHandle | Spawn a container |
//! | `container-stop`  | `(handle)` → `()` | Send SIGTERM to a container |
//! | `container-wait`  | `(handle)` → Int | Wait for a container to exit |
//! | `container-run`   | `(svc-spec)` → Int | Start + wait; returns exit code |
//! | `container-ip`    | `(handle)` → Str\|Nil | Primary IP of container |
//! | `container-status`| `(handle)` → Str | `"running"` or `"exited"` |
//! | `await-port`      | `(host port [timeout-secs])` → Bool | TCP connect loop |

use std::cell::RefCell;
use std::io::Read;
use std::net::TcpStream;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::compose::ServiceSpec;
use crate::container::{Command, Stdio, Volume};
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
    registry: Rc<RefCell<Vec<(String, i32)>>>,
    project: String,
    compose_dir: PathBuf,
) {
    let project = Rc::new(project);
    let compose_dir = Rc::new(compose_dir);

    // ── container-start ────────────────────────────────────────────────────
    {
        let registry = Rc::clone(&registry);
        let project = Rc::clone(&project);
        let compose_dir = Rc::clone(&compose_dir);
        native(env, "container-start", move |args| {
            if args.len() != 1 {
                return Err(LispError::new(
                    "container-start: expected 1 argument (service-spec)",
                ));
            }
            let svc = extract_service_spec("container-start", &args[0])?;
            do_container_start(svc, &project, &compose_dir, &registry)
        });
    }

    // ── container-stop ─────────────────────────────────────────────────────
    {
        let registry = Rc::clone(&registry);
        native(env, "container-stop", move |args| {
            if args.len() != 1 {
                return Err(LispError::new(
                    "container-stop: expected 1 argument (container-handle)",
                ));
            }
            let (name, pid) = extract_handle("container-stop", &args[0])?;
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
            registry.borrow_mut().retain(|(n, _)| n != &name);
            Ok(Value::Nil)
        });
    }

    // ── container-wait ─────────────────────────────────────────────────────
    // Polls kill(pid, 0) until the process is gone, then returns the exit code
    // from the container state file (if available) or 0.
    native(env, "container-wait", |args| {
        if args.len() != 1 {
            return Err(LispError::new(
                "container-wait: expected 1 argument (container-handle)",
            ));
        }
        let (_, pid) = extract_handle("container-wait", &args[0])?;
        loop {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                Err(nix::errno::Errno::ESRCH) => break,
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        Ok(Value::Int(0))
    });

    // ── container-run ──────────────────────────────────────────────────────
    {
        let registry = Rc::clone(&registry);
        let project = Rc::clone(&project);
        let compose_dir = Rc::clone(&compose_dir);
        native(env, "container-run", move |args| {
            if args.len() != 1 {
                return Err(LispError::new(
                    "container-run: expected 1 argument (service-spec)",
                ));
            }
            let svc = extract_service_spec("container-run", &args[0])?;
            let handle = do_container_start(svc, &project, &compose_dir, &registry)?;
            let (name, pid) = extract_handle("container-run", &handle)?;
            // Wait for process to exit.
            loop {
                match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                    Err(nix::errno::Errno::ESRCH) => break,
                    _ => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            registry.borrow_mut().retain(|(n, _)| n != &name);
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
// Core container-start logic
// ---------------------------------------------------------------------------

fn do_container_start(
    svc: ServiceSpec,
    project: &str,
    compose_dir: &std::path::Path,
    registry: &Rc<RefCell<Vec<(String, i32)>>>,
) -> Result<Value, LispError> {
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
    let container_name = format!("{}-{}", project, svc.name);

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

    std::thread::spawn(move || {
        if let Some(mut src) = stdout_handle.take() {
            let mut buf = [0u8; 4096];
            while matches!(src.read(&mut buf), Ok(n) if n > 0) {}
        }
    });

    std::thread::spawn(move || {
        if let Some(mut src) = stderr_handle.take() {
            let mut buf = [0u8; 4096];
            while matches!(src.read(&mut buf), Ok(n) if n > 0) {}
        }
    });

    // Spawn waiter that cleans up DNS when the container exits.
    let all_ips_wait = all_ips.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        for (net_name, _) in &all_ips_wait {
            let _ = crate::dns::dns_remove_entry(net_name, &svc_name_log);
        }
    });

    // Register in interpreter's cleanup registry.
    registry.borrow_mut().push((container_name.clone(), pid));

    log::info!(
        "container-start: '{}' started (pid {}, ip {:?})",
        container_name,
        pid,
        ip
    );

    Ok(Value::ContainerHandle {
        name: container_name,
        pid,
        ip,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
            "image '{}' not found locally (run 'remora image pull {}'): {}",
            image_ref, image_ref, e
        ))
    })?;
    Ok((normalised, m))
}

/// Normalize short image references (e.g. `alpine` → `docker.io/library/alpine:latest`).
fn normalise_image_reference(r: &str) -> String {
    let (name, tag) = r.split_once(':').map_or((r, "latest"), |(n, t)| (n, t));
    if name.contains('/') {
        if name.contains('.') || name.contains(':') {
            format!("{}:{}", name, tag)
        } else {
            format!("docker.io/{}:{}", name, tag)
        }
    } else {
        format!("docker.io/library/{}:{}", name, tag)
    }
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
