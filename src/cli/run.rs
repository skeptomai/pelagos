//! `remora run` — create and start a container.

use super::{
    check_liveness, container_dir, containers_dir, generate_name, parse_capability, parse_cpus,
    parse_memory, parse_ulimit, parse_user, parse_user_in_layers, rootfs_path, write_state,
    ContainerState, ContainerStatus, HealthConfig, HealthStatus,
};
use remora::container::{Capability, Command, Namespace, Stdio, Volume};
use remora::network::NetworkMode;
use std::io::{self, Read, Write};
use std::path::PathBuf;

#[derive(Debug, clap::Args)]
#[clap(trailing_var_arg = true)]
pub struct RunArgs {
    /// Container name (auto-generated if omitted: remora-1, remora-2, …)
    #[clap(long)]
    pub name: Option<String>,

    /// Run in the background; print container name and exit
    #[clap(long, short = 'd')]
    pub detach: bool,

    /// Allocate a PTY for interactive use (incompatible with --detach)
    #[clap(long, short = 'i')]
    pub interactive: bool,

    /// Network mode (repeatable for multi-network): none, loopback, bridge, pasta, or named
    /// First value is primary; additional values attach secondary bridge interfaces.
    #[clap(long)]
    pub network: Vec<String>,

    /// TCP port forward HOST:CONTAINER (repeatable; requires bridge/pasta)
    #[clap(long = "publish", short = 'p')]
    pub publish: Vec<String>,

    /// Enable MASQUERADE NAT (requires bridge)
    #[clap(long)]
    pub nat: bool,

    /// DNS server (repeatable; requires bridge/pasta)
    #[clap(long)]
    pub dns: Vec<String>,

    /// Named volume or bind mount: NAME:/path or /host:/container
    #[clap(long = "volume", short = 'v')]
    pub volume: Vec<String>,

    /// Read-write bind mount /host:/container (repeatable)
    #[clap(long = "bind")]
    pub bind: Vec<String>,

    /// Read-only bind mount /host:/container (repeatable)
    #[clap(long = "bind-ro")]
    pub bind_ro: Vec<String>,

    /// tmpfs mount /path[:options] (repeatable)
    #[clap(long = "tmpfs")]
    pub tmpfs: Vec<String>,

    /// Make rootfs read-only
    #[clap(long = "read-only")]
    pub read_only: bool,

    /// Environment variable KEY=VALUE (repeatable)
    #[clap(long = "env", short = 'e')]
    pub env: Vec<String>,

    /// Load environment from file (KEY=VALUE lines)
    #[clap(long = "env-file")]
    pub env_file: Option<PathBuf>,

    /// Working directory inside the container
    #[clap(long = "workdir", short = 'w')]
    pub workdir: Option<String>,

    /// UID[:GID] to run as (e.g. 1000 or 1000:1000)
    #[clap(long = "user", short = 'u')]
    pub user: Option<String>,

    /// Hostname inside the container
    #[clap(long)]
    pub hostname: Option<String>,

    /// Memory limit (e.g. 256m, 1g)
    #[clap(long)]
    pub memory: Option<String>,

    /// CPU quota (e.g. 0.5 = 50%)
    #[clap(long)]
    pub cpus: Option<String>,

    /// CPU shares / weight
    #[clap(long = "cpu-shares")]
    pub cpu_shares: Option<u64>,

    /// Maximum number of processes
    #[clap(long = "pids-limit")]
    pub pids_limit: Option<u64>,

    /// rlimit RESOURCE=SOFT:HARD (repeatable)
    #[clap(long = "ulimit")]
    pub ulimit: Vec<String>,

    /// Capability to drop: ALL or CAP_NAME (repeatable)
    #[clap(long = "cap-drop")]
    pub cap_drop: Vec<String>,

    /// Capability to add after --cap-drop ALL (repeatable)
    #[clap(long = "cap-add")]
    pub cap_add: Vec<String>,

    /// Security options: seccomp=default|minimal|none, no-new-privileges
    #[clap(long = "security-opt")]
    pub security_opt: Vec<String>,

    /// Link to another container for /etc/hosts name resolution (repeatable).
    /// Format: NAME or NAME:ALIAS
    #[clap(long = "link")]
    pub link: Vec<String>,

    /// Kernel parameter KEY=VALUE (repeatable)
    #[clap(long = "sysctl")]
    pub sysctl: Vec<String>,

    /// Path to mask inside the container (repeatable)
    #[clap(long = "masked-path")]
    pub masked_path: Vec<String>,

    /// DNS backend: builtin (default) or dnsmasq
    #[clap(long = "dns-backend", value_name = "BACKEND")]
    pub dns_backend: Option<String>,

    /// Use a local rootfs instead of an OCI image (advanced)
    #[clap(long)]
    pub rootfs: Option<String>,

    /// Image reference (or command when using --rootfs): IMAGE [COMMAND [ARGS...]]
    #[clap(multiple_values = true)]
    pub args: Vec<String>,
}

pub fn cmd_run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    if args.detach && args.interactive {
        return Err("--detach and --interactive are mutually exclusive".into());
    }

    // Set DNS backend env var before any DNS calls so active_backend() picks it up.
    if let Some(ref backend) = args.dns_backend {
        // SAFETY: called early in single-threaded CLI startup, before spawning threads.
        unsafe { std::env::set_var("REMORA_DNS_BACKEND", backend) };
    }

    // Generate container name
    let name = match args.name {
        Some(ref n) => n.clone(),
        None => generate_name()?,
    };

    // Validate name is not already in use
    if container_dir(&name).exists() {
        let state = super::read_state(&name).ok();
        if let Some(s) = state {
            if s.status == ContainerStatus::Running && check_liveness(s.pid) {
                return Err(format!("container '{}' already exists and is running", name).into());
            }
        }
    }

    // Parse port forwards and network mode (shared by both paths).
    let port_forwards = parse_port_forwards(&args.publish)?;
    let primary_network_str = args.network.first().map(|s| s.as_str()).unwrap_or("none");
    let network_mode = parse_network_mode(primary_network_str)?;
    let additional_networks: Vec<String> = args.network.iter().skip(1).cloned().collect();
    // Validate additional networks exist.
    for net_name in &additional_networks {
        let config = remora::paths::network_config_dir(net_name).join("config.json");
        if !config.exists() {
            return Err(format!(
                "additional network '{}' not found — create it first: remora network create {} --subnet CIDR",
                net_name, net_name
            ).into());
        }
    }

    // Branch: --rootfs (local rootfs) vs positional args (OCI image, default).
    let (rootfs_label, exe_and_args, cmd, health_config) =
        if let Some(ref rootfs_name) = args.rootfs {
            let exe_and_args: Vec<String> = if args.args.is_empty() {
                vec!["/bin/sh".to_string()]
            } else {
                args.args.clone()
            };
            let rootfs_dir = rootfs_path(rootfs_name)?;
            let cmd = build_command(
                &args,
                &rootfs_dir,
                &exe_and_args,
                &port_forwards,
                network_mode,
                &additional_networks,
            )?;
            (rootfs_name.clone(), exe_and_args, cmd, None)
        } else {
            if args.args.is_empty() {
                return Err("an image name is required".into());
            }
            let image_ref = &args.args[0];
            let cmd_args: Vec<String> = args.args[1..].to_vec();
            build_image_run(
                &args,
                image_ref,
                &cmd_args,
                &port_forwards,
                network_mode,
                &additional_networks,
            )?
        };

    if args.detach {
        run_detached(name, rootfs_label, exe_and_args, cmd, health_config)
    } else if args.interactive {
        run_interactive(cmd)
    } else {
        run_foreground(name, rootfs_label, exe_and_args, cmd)
    }
}

/// (rootfs_label, exe_and_args, Command, health_config)
type ImageRunResult = (String, Vec<String>, Command, Option<HealthConfig>);

/// Build a Command from a pulled OCI image.
fn build_image_run(
    args: &RunArgs,
    image_ref: &str,
    cmd_args: &[String],
    port_forwards: &[(u16, u16, remora::network::PortProto)],
    network_mode: NetworkMode,
    additional_networks: &[String],
) -> Result<ImageRunResult, Box<dyn std::error::Error>> {
    use remora::image;

    // Try loading the raw reference first (locally-built images), then normalised.
    let (full_ref, manifest) = if let Ok(m) = image::load_image(image_ref) {
        (image_ref.to_string(), m)
    } else {
        let normalised = normalise_image_reference(image_ref);
        let m = image::load_image(&normalised).map_err(|e| {
            format!(
                "image '{}' not found locally (run 'remora image pull {}'): {}",
                image_ref, image_ref, e
            )
        })?;
        (normalised, m)
    };

    // Resolve layer directories (top-first for overlayfs).
    let layers = image::layer_dirs(&manifest);
    if layers.is_empty() {
        return Err("image has no layers".into());
    }
    let layer_dirs = layers.clone();

    // Determine the command: CLI args override image Entrypoint+Cmd.
    let exe_and_args = if !cmd_args.is_empty() {
        cmd_args.to_vec()
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

    let mut cmd = Command::new(exe)
        .args(rest)
        .with_image_layers(layers)
        // Add UTS (hostname isolation) + PID namespace.  Use add_namespaces so
        // we OR into the flags already set by with_image_layers (MOUNT) rather
        // than replacing them.
        .add_namespaces(Namespace::UTS | Namespace::PID);

    // Apply image config defaults: environment.
    for env_str in &manifest.config.env {
        if let Some((k, v)) = env_str.split_once('=') {
            cmd = cmd.env(k, v);
        }
    }

    // Apply image config working directory.
    if !manifest.config.working_dir.is_empty() && args.workdir.is_none() {
        cmd = cmd.with_cwd(&manifest.config.working_dir);
    }

    // Apply image config user as default (CLI --user overrides).
    if args.user.is_none() && !manifest.config.user.is_empty() {
        let (uid, gid) = parse_user_in_layers(&manifest.config.user, &layer_dirs)?;
        cmd = cmd.with_uid(uid);
        if let Some(g) = gid {
            cmd = cmd.with_gid(g);
        }
    }

    // Apply shared CLI options (network, volumes, security, etc.)
    cmd = apply_cli_options(cmd, args, port_forwards, network_mode, additional_networks)?;

    let health_config = manifest.config.healthcheck.clone();
    Ok((full_ref, exe_and_args, cmd, health_config))
}

/// Expand bare image names: "alpine" → "docker.io/library/alpine:latest".
fn normalise_image_reference(reference: &str) -> String {
    let r = reference.to_string();
    let r = if !r.contains(':') && !r.contains('@') {
        format!("{}:latest", r)
    } else {
        r
    };
    if !r.contains('/') {
        format!("docker.io/library/{}", r)
    } else {
        r
    }
}

fn build_command(
    args: &RunArgs,
    rootfs_dir: &std::path::Path,
    exe_and_args: &[String],
    port_forwards: &[(u16, u16, remora::network::PortProto)],
    network_mode: NetworkMode,
    additional_networks: &[String],
) -> Result<Command, Box<dyn std::error::Error>> {
    let exe = &exe_and_args[0];
    let rest = &exe_and_args[1..];

    let mut cmd = Command::new(exe)
        .args(rest)
        .with_chroot(rootfs_dir)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
        .with_proc_mount()
        .with_dev_mount();

    cmd = apply_cli_options(cmd, args, port_forwards, network_mode, additional_networks)?;
    Ok(cmd)
}

/// Apply all CLI options (network, filesystem, env, security, etc.) to a Command.
/// Shared between the rootfs path and the --image path.
fn apply_cli_options(
    mut cmd: Command,
    args: &RunArgs,
    port_forwards: &[(u16, u16, remora::network::PortProto)],
    network_mode: NetworkMode,
    additional_networks: &[String],
) -> Result<Command, Box<dyn std::error::Error>> {
    // Network
    if network_mode != NetworkMode::None {
        cmd = cmd.with_network(network_mode);
    }
    for net_name in additional_networks {
        cmd = cmd.with_additional_network(net_name);
    }
    for &(host, container, proto) in port_forwards {
        use remora::network::PortProto;
        cmd = match proto {
            PortProto::Tcp => cmd.with_port_forward(host, container),
            PortProto::Udp => cmd.with_port_forward_udp(host, container),
            PortProto::Both => cmd.with_port_forward_both(host, container),
        };
    }
    if args.nat {
        cmd = cmd.with_nat();
    }
    if !args.dns.is_empty() {
        cmd = cmd.with_dns(&args.dns.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }
    for link_spec in &args.link {
        if let Some((name, alias)) = link_spec.split_once(':') {
            cmd = cmd.with_link_alias(name, alias);
        } else {
            cmd = cmd.with_link(link_spec);
        }
    }

    // Filesystem
    if args.read_only {
        cmd = cmd.with_readonly_rootfs(true);
    }
    for v in &args.volume {
        if let Some((src, rest)) = v.split_once(':') {
            // Support "src:tgt" and "src:tgt:ro" (Docker compat).
            let (tgt, readonly) = match rest.rsplit_once(':') {
                Some((t, "ro")) => (t, true),
                Some((t, "rw")) => (t, false),
                _ => (rest, false),
            };
            if src.starts_with('/') {
                if readonly {
                    cmd = cmd.with_bind_mount_ro(src, tgt);
                } else {
                    cmd = cmd.with_bind_mount(src, tgt);
                }
            } else {
                let vol = Volume::open(src).or_else(|_| Volume::create(src))?;
                cmd = cmd.with_volume(&vol, tgt);
            }
        } else {
            return Err(format!(
                "invalid --volume '{}': expected NAME:/path or /host:/path[:ro|:rw]",
                v
            )
            .into());
        }
    }
    for b in &args.bind {
        let (src, tgt) = split_mount_spec(b, "--bind")?;
        cmd = cmd.with_bind_mount(src, tgt);
    }
    for b in &args.bind_ro {
        let (src, tgt) = split_mount_spec(b, "--bind-ro")?;
        cmd = cmd.with_bind_mount_ro(src, tgt);
    }
    for t in &args.tmpfs {
        let (path, opts) = t.split_once(':').unwrap_or((t.as_str(), ""));
        cmd = cmd.with_tmpfs(path, opts);
    }

    // Environment
    if let Some(ref ef) = args.env_file {
        let content = std::fs::read_to_string(ef)
            .map_err(|e| format!("--env-file {}: {}", ef.display(), e))?;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                cmd = cmd.env(k, v);
            }
        }
    }
    for e in &args.env {
        if let Some((k, v)) = e.split_once('=') {
            cmd = cmd.env(k, v);
        } else if let Ok(v) = std::env::var(e) {
            cmd = cmd.env(e, v);
        }
    }
    // Always set a sensible PATH
    cmd = cmd.env(
        "PATH",
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    );

    // User
    if let Some(ref u) = args.user {
        let (uid, gid) = parse_user(u)?;
        cmd = cmd.with_uid(uid);
        if let Some(g) = gid {
            cmd = cmd.with_gid(g);
        }
    }

    // Workdir
    if let Some(ref w) = args.workdir {
        cmd = cmd.with_cwd(w);
    }

    // Hostname
    if let Some(ref h) = args.hostname {
        cmd = cmd.with_hostname(h);
    }

    // Cgroups
    if let Some(ref m) = args.memory {
        let bytes = parse_memory(m)?;
        cmd = cmd.with_cgroup_memory(bytes);
    }
    if let Some(ref c) = args.cpus {
        let (quota, period) = parse_cpus(c)?;
        cmd = cmd.with_cgroup_cpu_quota(quota, period);
    }
    if let Some(shares) = args.cpu_shares {
        cmd = cmd.with_cgroup_cpu_shares(shares);
    }
    if let Some(pids) = args.pids_limit {
        cmd = cmd.with_cgroup_pids_limit(pids);
    }

    // Ulimits
    for u in &args.ulimit {
        let (res, soft, hard) = parse_ulimit(u)?;
        cmd = cmd.with_rlimit(res, soft, hard);
    }

    // Capabilities
    let drop_all = args.cap_drop.iter().any(|c| c.eq_ignore_ascii_case("ALL"));
    if drop_all {
        cmd = cmd.drop_all_capabilities();
        let mut add_caps = Capability::empty();
        for cap_name in &args.cap_add {
            let cap = parse_capability(cap_name)?;
            add_caps |= cap;
        }
        if !add_caps.is_empty() {
            cmd = cmd.with_capabilities(add_caps);
        }
    } else if !args.cap_drop.is_empty() {
        return Err("--cap-drop only supports 'ALL'; use --cap-drop ALL --cap-add CAP_NAME to keep specific capabilities".into());
    }

    // Security options
    for opt in &args.security_opt {
        let (key, val) = opt.split_once('=').unwrap_or((opt.as_str(), ""));
        match key {
            "seccomp" => match val {
                "default" | "" => cmd = cmd.with_seccomp_default(),
                "minimal" => cmd = cmd.with_seccomp_minimal(),
                "none" => {}
                other => {
                    return Err(format!(
                        "unknown seccomp profile '{}' (use: default, minimal, none)",
                        other
                    )
                    .into())
                }
            },
            "no-new-privileges" => cmd = cmd.with_no_new_privileges(true),
            other => return Err(format!("unknown --security-opt '{}'", other).into()),
        }
    }

    // Sysctl
    for s in &args.sysctl {
        if let Some((k, v)) = s.split_once('=') {
            cmd = cmd.with_sysctl(k, v);
        } else {
            return Err(format!("invalid --sysctl '{}': expected KEY=VALUE", s).into());
        }
    }

    // Masked paths
    if !args.masked_path.is_empty() {
        let paths: Vec<&str> = args.masked_path.iter().map(|s| s.as_str()).collect();
        cmd = cmd.with_masked_paths(&paths);
    }

    Ok(cmd)
}

fn parse_network_mode(s: &str) -> Result<NetworkMode, Box<dyn std::error::Error>> {
    match s.to_ascii_lowercase().as_str() {
        "none" | "" => Ok(NetworkMode::None),
        "loopback" => Ok(NetworkMode::Loopback),
        "bridge" => Ok(NetworkMode::Bridge),
        "pasta" => Ok(NetworkMode::Pasta),
        name => {
            // Check if it's a named network.
            let config = remora::paths::network_config_dir(name).join("config.json");
            if config.exists() {
                Ok(NetworkMode::BridgeNamed(name.to_string()))
            } else {
                Err(format!(
                    "unknown network '{}' — use a mode (none, loopback, bridge, pasta) \
                     or create it first: remora network create {} --subnet CIDR",
                    name, name
                )
                .into())
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn parse_port_forwards(
    specs: &[String],
) -> Result<Vec<(u16, u16, remora::network::PortProto)>, Box<dyn std::error::Error>> {
    use remora::network::PortProto;
    let mut out = Vec::new();
    for s in specs {
        // Accept HOST:CONTAINER[/tcp|/udp|/both]
        let (ports_part, proto_str) = match s.rsplit_once('/') {
            Some((p, pr)) => (p, pr),
            None => (s.as_str(), "tcp"),
        };
        let (h, c) = ports_part
            .split_once(':')
            .ok_or_else(|| format!("invalid --publish '{}': expected HOST:CONTAINER[/PROTO]", s))?;
        let host = h
            .trim()
            .parse::<u16>()
            .map_err(|e| format!("invalid host port '{}': {}", h, e))?;
        let container = c
            .trim()
            .parse::<u16>()
            .map_err(|e| format!("invalid container port '{}': {}", c, e))?;
        let proto = PortProto::parse(proto_str);
        out.push((host, container, proto));
    }
    Ok(out)
}

fn split_mount_spec<'a>(
    s: &'a str,
    flag: &str,
) -> Result<(&'a str, &'a str), Box<dyn std::error::Error>> {
    s.split_once(':')
        .ok_or_else(|| format!("invalid {} '{}': expected /host:/container", flag, s).into())
}

// ---------------------------------------------------------------------------
// Foreground mode
// ---------------------------------------------------------------------------

fn run_foreground(
    name: String,
    rootfs: String,
    command: Vec<String>,
    mut cmd: Command,
) -> Result<(), Box<dyn std::error::Error>> {
    cmd = cmd
        .stdin(Stdio::Inherit)
        .stdout(Stdio::Inherit)
        .stderr(Stdio::Inherit);

    // Write initial state
    std::fs::create_dir_all(containers_dir())?;
    let state = ContainerState {
        name: name.clone(),
        rootfs,
        status: ContainerStatus::Running,
        pid: 0,
        watcher_pid: 0,
        started_at: super::now_iso8601(),
        exit_code: None,
        command: command.clone(),
        stdout_log: None,
        stderr_log: None,
        bridge_ip: None,
        network_ips: std::collections::HashMap::new(),
        health: None,
        health_config: None,
    };
    write_state(&state)?;

    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {}", e))?;
    let pid = child.pid();

    // Update state with real PID and network IPs (if bridge networking).
    let mut state2 = state;
    state2.pid = pid;
    state2.bridge_ip = child.container_ip();
    let all_ips: Vec<(String, String)> = child
        .container_ips()
        .into_iter()
        .map(|(name, ip)| (name.to_string(), ip))
        .collect();
    state2.network_ips = all_ips.iter().cloned().collect();
    write_state(&state2)?;

    // Register container with embedded DNS daemon for each bridge network.
    register_dns(&name, &all_ips);

    let exit = child.wait().map_err(|e| format!("wait failed: {}", e))?;
    let code = exit.code().unwrap_or(1);

    // Deregister container from DNS.
    deregister_dns(&name, &all_ips);

    // Update final state
    state2.status = ContainerStatus::Exited;
    state2.exit_code = Some(code);
    write_state(&state2)?;

    std::process::exit(code);
}

// ---------------------------------------------------------------------------
// Interactive mode
// ---------------------------------------------------------------------------

fn run_interactive(cmd: Command) -> Result<(), Box<dyn std::error::Error>> {
    let session = cmd
        .spawn_interactive()
        .map_err(|e| format!("spawn_interactive failed: {}", e))?;
    match session.run() {
        Ok(status) => {
            let code = status.code().unwrap_or(0);
            std::process::exit(code);
        }
        Err(e) => Err(format!("interactive session failed: {}", e).into()),
    }
}

// ---------------------------------------------------------------------------
// Detached mode
// ---------------------------------------------------------------------------

fn run_detached(
    name: String,
    rootfs: String,
    command: Vec<String>,
    mut cmd: Command,
    health_config: Option<HealthConfig>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create container directory before fork so parent and child both see it.
    std::fs::create_dir_all(containers_dir())?;
    let dir = container_dir(&name);
    std::fs::create_dir_all(&dir)?;

    let stdout_log = dir.join("stdout.log");
    let stderr_log = dir.join("stderr.log");

    let state = ContainerState {
        name: name.clone(),
        rootfs,
        status: ContainerStatus::Running,
        pid: 0,
        watcher_pid: 0,
        started_at: super::now_iso8601(),
        exit_code: None,
        command: command.clone(),
        stdout_log: Some(stdout_log.to_string_lossy().into_owned()),
        stderr_log: Some(stderr_log.to_string_lossy().into_owned()),
        bridge_ip: None,
        network_ips: std::collections::HashMap::new(),
        health: None,
        health_config: None,
    };
    write_state(&state)?;

    // Fork a watcher child; parent prints name and exits.
    let fork_result = unsafe { libc::fork() };
    match fork_result {
        -1 => {
            return Err(io::Error::last_os_error().into());
        }
        0 => {
            // We are the watcher child.
            // Detach from parent's session so we're adopted by init when parent exits.
            unsafe { libc::setsid() };

            // Set up piped stdio so we can relay to log files.
            cmd = cmd
                .stdin(Stdio::Null)
                .stdout(Stdio::Piped)
                .stderr(Stdio::Piped);

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("remora watcher: spawn failed: {}", e);
                    unsafe { libc::_exit(1) };
                }
            };
            let pid = child.pid();
            let watcher_pid = unsafe { libc::getpid() };

            // Update state with real PIDs and network IPs.
            let mut updated = state;
            updated.pid = pid;
            updated.watcher_pid = watcher_pid;
            updated.bridge_ip = child.container_ip();
            let all_ips: Vec<(String, String)> = child
                .container_ips()
                .into_iter()
                .map(|(name, ip)| (name.to_string(), ip))
                .collect();
            updated.network_ips = all_ips.iter().cloned().collect();
            updated.health_config = health_config.clone();
            if health_config.is_some() {
                updated.health = Some(HealthStatus::Starting);
            }
            let _ = write_state(&updated);

            // Register container with embedded DNS daemon.
            register_dns(&name, &all_ips);

            // Spawn health monitor thread (if the image has a HEALTHCHECK).
            let health_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let health_thread = health_config.map(|hc| {
                let stop = std::sync::Arc::clone(&health_stop);
                let name2 = name.clone();
                std::thread::spawn(move || super::health::run_health_monitor(name2, pid, hc, stop))
            });

            // Relay stdout and stderr to log files concurrently.
            let mut stdout_handle = child.take_stdout();
            let mut stderr_handle = child.take_stderr();

            let stdout_path = stdout_log.clone();
            let stderr_path = stderr_log.clone();

            // Use two threads: one for each stream.
            let t_out = std::thread::spawn(move || {
                if let Some(mut src) = stdout_handle.take() {
                    if let Ok(mut f) = std::fs::File::create(&stdout_path) {
                        let mut buf = [0u8; 4096];
                        loop {
                            match src.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let _ = f.write_all(&buf[..n]);
                                }
                            }
                        }
                    }
                }
            });
            let t_err = std::thread::spawn(move || {
                if let Some(mut src) = stderr_handle.take() {
                    if let Ok(mut f) = std::fs::File::create(&stderr_path) {
                        let mut buf = [0u8; 4096];
                        loop {
                            match src.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let _ = f.write_all(&buf[..n]);
                                }
                            }
                        }
                    }
                }
            });

            // Wait for the container to exit.
            let exit = match child.wait() {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("remora watcher: wait failed: {}", e);
                    unsafe { libc::_exit(1) };
                }
            };
            // Signal the health monitor to stop and wait for it.
            health_stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = t_out.join();
            let _ = t_err.join();
            if let Some(t) = health_thread {
                let _ = t.join();
            }

            // Deregister container from DNS.
            deregister_dns(&name, &all_ips);

            // Update final state.
            updated.status = ContainerStatus::Exited;
            updated.exit_code = exit.code();
            let _ = write_state(&updated);

            unsafe { libc::_exit(0) };
        }
        _child_pid => {
            // We are the parent: print the container name and exit immediately.
            println!("{}", name);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DNS registration helpers
// ---------------------------------------------------------------------------

/// Register a container with the embedded DNS daemon for each bridge network.
fn register_dns(container_name: &str, network_ips: &[(String, String)]) {
    for (net_name, ip_str) in network_ips {
        let ip: std::net::Ipv4Addr = match ip_str.parse() {
            Ok(ip) => ip,
            Err(_) => continue,
        };
        let net_def = match remora::network::load_network_def(net_name) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if let Err(e) = remora::dns::dns_add_entry(
            net_name,
            container_name,
            ip,
            net_def.gateway,
            &["8.8.8.8".to_string(), "1.1.1.1".to_string()],
        ) {
            log::warn!(
                "dns: failed to register '{}' on {}: {}",
                container_name,
                net_name,
                e
            );
        }
    }
}

/// Deregister a container from the embedded DNS daemon for each bridge network.
fn deregister_dns(container_name: &str, network_ips: &[(String, String)]) {
    for (net_name, _ip_str) in network_ips {
        if let Err(e) = remora::dns::dns_remove_entry(net_name, container_name) {
            log::warn!(
                "dns: failed to deregister '{}' from {}: {}",
                container_name,
                net_name,
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_port_forwards_tcp_default() {
        let specs = vec!["8080:80".to_string()];
        let fwds = parse_port_forwards(&specs).unwrap();
        assert_eq!(fwds.len(), 1);
        assert_eq!(fwds[0], (8080, 80, remora::network::PortProto::Tcp));
    }

    #[test]
    fn test_parse_port_forwards_explicit_tcp() {
        let specs = vec!["8080:80/tcp".to_string()];
        let fwds = parse_port_forwards(&specs).unwrap();
        assert_eq!(fwds[0], (8080, 80, remora::network::PortProto::Tcp));
    }

    #[test]
    fn test_parse_port_forwards_udp() {
        let specs = vec!["5353:53/udp".to_string()];
        let fwds = parse_port_forwards(&specs).unwrap();
        assert_eq!(fwds[0], (5353, 53, remora::network::PortProto::Udp));
    }

    #[test]
    fn test_parse_port_forwards_both() {
        let specs = vec!["53:53/both".to_string()];
        let fwds = parse_port_forwards(&specs).unwrap();
        assert_eq!(fwds[0], (53, 53, remora::network::PortProto::Both));
    }

    #[test]
    fn test_parse_port_forwards_multiple() {
        let specs = vec!["80:80/tcp".to_string(), "5353:53/udp".to_string()];
        let fwds = parse_port_forwards(&specs).unwrap();
        assert_eq!(fwds.len(), 2);
        assert_eq!(fwds[0].2, remora::network::PortProto::Tcp);
        assert_eq!(fwds[1].2, remora::network::PortProto::Udp);
    }

    #[test]
    fn test_parse_port_forwards_invalid() {
        assert!(parse_port_forwards(&["notaport".to_string()]).is_err());
        assert!(parse_port_forwards(&["abc:80".to_string()]).is_err());
    }
}
