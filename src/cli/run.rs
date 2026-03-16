//! `pelagos run` — create and start a container.

use std::sync::atomic::{AtomicI32, Ordering};

/// PID of the container process being watched.  Written once after spawn,
/// read by the watcher's SIGTERM/SIGINT handler to forward the signal.
static WATCHER_CONTAINER_PID: AtomicI32 = AtomicI32::new(0);

/// Signal handler installed in the watcher process.
///
/// Forwards the received signal to the container process so that the normal
/// teardown path (`child.wait()` → `teardown_resources()`) runs.  This
/// ensures that `kill <watcher_pid>` triggers clean resource removal rather
/// than leaving dangling veths and nftables rules.
///
/// # Safety
/// Only async-signal-safe operations: `AtomicI32::load` + `libc::kill`.
#[allow(dead_code)] // used via FFI in watcher child's signal() call
extern "C" fn watcher_forward_signal(signum: libc::c_int) {
    let pid = WATCHER_CONTAINER_PID.load(Ordering::Relaxed);
    if pid > 0 {
        unsafe { libc::kill(pid, signum) };
    }
}

use super::{
    check_liveness, container_dir, containers_dir, generate_name, parse_capability, parse_cpus,
    parse_memory, parse_ulimit, parse_user, parse_user_in_layers, rootfs_path, write_state,
    ContainerState, ContainerStatus, HealthConfig, HealthStatus, SpawnConfig,
};
use pelagos::container::{Capability, Command, Namespace, Stdio, Volume};
use pelagos::network::NetworkMode;
use pelagos::wasm::WasmRuntime;
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Debug, clap::Args)]
#[clap(trailing_var_arg = true)]
pub struct RunArgs {
    /// Container name (auto-generated if omitted: pelagos-1, pelagos-2, …)
    #[clap(long)]
    pub name: Option<String>,

    /// Run in the background; print container name and exit
    #[clap(long, short = 'd')]
    pub detach: bool,

    /// Automatically remove the container when it exits
    #[clap(long)]
    pub rm: bool,

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

    /// tmpfs mount `/path[:options]` (repeatable)
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

    /// `UID[:GID]` to run as (e.g. 1000 or 1000:1000)
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

    /// Capability to drop from the default set: ALL or a capability name (repeatable)
    #[clap(long = "cap-drop")]
    pub cap_drop: Vec<String>,

    /// Capability to add on top of the default set (repeatable)
    #[clap(long = "cap-add")]
    pub cap_add: Vec<String>,

    /// Security options: seccomp=default|minimal|iouring|none, no-new-privileges
    #[clap(long = "security-opt")]
    pub security_opt: Vec<String>,

    /// AppArmor profile to apply at container exec time (e.g. "pelagos-container")
    #[clap(long = "apparmor-profile", value_name = "PROFILE")]
    pub apparmor_profile: Option<String>,

    /// SELinux process label to apply at container exec time
    /// (e.g. "system_u:system_r:container_t:s0")
    #[clap(long = "selinux-label", value_name = "LABEL")]
    pub selinux_label: Option<String>,

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

    /// Label KEY=VALUE (repeatable; stored in state.json and filterable via pelagos ps --filter)
    #[clap(long = "label", short = 'l')]
    pub label: Vec<String>,

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
        unsafe { std::env::set_var("PELAGOS_DNS_BACKEND", backend) };
    }

    // Parse network mode early (no filesystem access) so the rootless guard can fire
    // before we touch the state directory.
    let port_forwards = parse_port_forwards(&args.publish)?;
    let primary_network_str = args.network.first().map(|s| s.as_str()).unwrap_or("none");
    let network_mode = parse_network_mode(primary_network_str)?;
    let additional_networks: Vec<String> = args.network.iter().skip(1).cloned().collect();

    // Early rootless + bridge guard — emit a friendly message before doing any filesystem work.
    if let Some(msg) = super::check_rootless_bridge(
        pelagos::paths::is_rootless(),
        &network_mode,
        args.nat,
        !args.publish.is_empty(),
    ) {
        eprintln!("{}", msg);
        std::process::exit(1);
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

    // Validate additional networks exist.
    for net_name in &additional_networks {
        let config = pelagos::paths::network_config_dir(net_name).join("config.json");
        if !config.exists() {
            return Err(format!(
                "additional network '{}' not found — create it first: pelagos network create {} --subnet CIDR",
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
                &name,
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
                &name,
            )?
        };

    let labels = parse_labels(&args.label);
    let spawn_config = build_spawn_config(&args, &rootfs_label, &exe_and_args);

    if args.detach {
        run_detached(
            name,
            rootfs_label,
            exe_and_args,
            cmd,
            health_config,
            Some(spawn_config),
            labels,
        )
    } else if args.interactive {
        run_interactive(cmd)
    } else {
        run_foreground(
            name,
            rootfs_label,
            exe_and_args,
            cmd,
            args.rm,
            Some(spawn_config),
            labels,
        )
    }
}

/// Parse "KEY=VALUE" label strings into a HashMap.
fn parse_labels(label_args: &[String]) -> std::collections::HashMap<String, String> {
    label_args
        .iter()
        .filter_map(|s| {
            let (k, v) = s.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// Capture RunArgs fields into a SpawnConfig for container restart.
fn build_spawn_config(args: &RunArgs, rootfs_label: &str, exe_and_args: &[String]) -> SpawnConfig {
    // image is None for --rootfs containers; otherwise it's the normalized image reference.
    let image = if args.rootfs.is_none() && !args.args.is_empty() {
        Some(rootfs_label.to_string())
    } else {
        None
    };
    SpawnConfig {
        image,
        exe: exe_and_args.first().cloned().unwrap_or_default(),
        args: exe_and_args.get(1..).unwrap_or(&[]).to_vec(),
        env: args.env.clone(),
        bind: args.bind.clone(),
        bind_ro: args.bind_ro.clone(),
        volume: args.volume.clone(),
        network: args.network.clone(),
        publish: args.publish.clone(),
        dns: args.dns.clone(),
        working_dir: args.workdir.clone(),
        user: args.user.clone(),
        hostname: args.hostname.clone(),
        cap_drop: args.cap_drop.clone(),
        cap_add: args.cap_add.clone(),
        security_opt: args.security_opt.clone(),
        read_only: args.read_only,
        rm: args.rm,
        nat: args.nat,
        labels: args.label.clone(),
    }
}

/// (rootfs_label, exe_and_args, Command, health_config)
type ImageRunResult = (String, Vec<String>, Command, Option<HealthConfig>);

/// Build a Command from a pulled OCI image.
fn build_image_run(
    args: &RunArgs,
    image_ref: &str,
    cmd_args: &[String],
    port_forwards: &[(u16, u16, pelagos::network::PortProto)],
    network_mode: NetworkMode,
    additional_networks: &[String],
    container_name: &str,
) -> Result<ImageRunResult, Box<dyn std::error::Error>> {
    use pelagos::image;

    // Resolve the image reference: load_image already tries <ref>:latest for
    // bare refs, so fall back directly to the normalised registry form.
    let (full_ref, manifest) = if let Ok(m) = image::load_image(image_ref) {
        (image_ref.to_string(), m)
    } else {
        let normalised = normalise_image_reference(image_ref);
        let m = image::load_image(&normalised).map_err(|e| {
            format!(
                "image '{}' not found locally (run 'pelagos image pull {}'): {}",
                image_ref, image_ref, e
            )
        })?;
        (normalised, m)
    };

    // --- Wasm image fast-path ---
    // If every layer is a Wasm blob, skip overlayfs/namespaces and run via
    // the system Wasm runtime (wasmtime / wasmedge).
    if manifest.is_wasm_image() {
        let wasm_path = manifest
            .wasm_module_path()
            .ok_or("Wasm image has no module.wasm layer — re-pull the image")?;
        let exe_and_args: Vec<String> = if !cmd_args.is_empty() {
            cmd_args.to_vec()
        } else {
            // Default WASI argv[0] is the wasm path itself.
            vec![wasm_path.to_string_lossy().into_owned()]
        };
        let wasm_str = wasm_path.to_string_lossy().into_owned();
        let extra_args = &exe_and_args[1..];

        let mut cmd = Command::new(&wasm_str)
            .args(extra_args)
            .with_wasm_runtime(WasmRuntime::Auto);

        // Pass image env vars as WASI env.
        for env_str in &manifest.config.env {
            if let Some((k, v)) = env_str.split_once('=') {
                cmd = cmd.with_wasi_env(k, v);
            }
        }

        // Extra WASI CLI env vars.
        for env_str in &args.env {
            if let Some((k, v)) = env_str.split_once('=') {
                cmd = cmd.with_wasi_env(k, v);
            }
        }

        // Bind-mount requested dirs become WASI preopened dirs (host→guest mapping).
        for bind_str in &args.bind {
            if let Some((host, guest)) = bind_str.split_once(':') {
                cmd = cmd.with_wasi_preopened_dir_mapped(host, guest);
            }
        }

        let health_config = manifest.config.healthcheck.clone();
        return Ok((full_ref, exe_and_args, cmd, health_config));
    }

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

    // Apply image config environment.  This includes any PATH set by Dockerfile
    // ENV instructions.  apply_cli_options no longer injects a fallback PATH
    // (doing so unconditionally would clobber the image's custom PATH — issue #114).
    // Inject the OCI-default PATH here only when the image config omits it.
    if !manifest
        .config
        .env
        .iter()
        .any(|e| e == "PATH" || e.starts_with("PATH="))
    {
        cmd = cmd.env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        );
    }
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
    cmd = apply_cli_options(
        cmd,
        args,
        port_forwards,
        network_mode,
        additional_networks,
        container_name,
    )?;

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
    port_forwards: &[(u16, u16, pelagos::network::PortProto)],
    network_mode: NetworkMode,
    additional_networks: &[String],
    container_name: &str,
) -> Result<Command, Box<dyn std::error::Error>> {
    let exe = &exe_and_args[0];
    let rest = &exe_and_args[1..];

    let mut cmd = Command::new(exe)
        .args(rest)
        .with_chroot(rootfs_dir)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT | Namespace::PID)
        .with_proc_mount()
        .with_dev_mount()
        // Rootfs-based runs have no image config; inject the OCI default PATH
        // so executables in standard locations are always findable.
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        );

    cmd = apply_cli_options(
        cmd,
        args,
        port_forwards,
        network_mode,
        additional_networks,
        container_name,
    )?;
    Ok(cmd)
}

/// Apply all CLI options (network, filesystem, env, security, etc.) to a Command.
/// Shared between the rootfs path and the --image path.
fn apply_cli_options(
    mut cmd: Command,
    args: &RunArgs,
    port_forwards: &[(u16, u16, pelagos::network::PortProto)],
    network_mode: NetworkMode,
    additional_networks: &[String],
    container_name: &str,
) -> Result<Command, Box<dyn std::error::Error>> {
    // Network
    if network_mode != NetworkMode::None {
        cmd = cmd.with_network(network_mode);
    }
    for net_name in additional_networks {
        cmd = cmd.with_additional_network(net_name);
    }
    for &(host, container, proto) in port_forwards {
        use pelagos::network::PortProto;
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
    // Do NOT set a fallback PATH here.  Callers (build_command for rootfs,
    // build_image_run for image runs) are responsible for injecting a default
    // PATH when neither the image config nor --env provides one.  Doing it here
    // unconditionally overwrites Dockerfile `ENV PATH=...` entries that were
    // already applied before apply_cli_options is called (issue #114).

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

    // Hostname: explicit --hostname wins; otherwise default to the container name
    // so the UTS namespace always shows something meaningful rather than the host's hostname.
    let hostname = args.hostname.as_deref().unwrap_or(container_name);
    cmd = cmd.with_hostname(hostname);

    // Cgroups
    if let Some(ref m) = args.memory {
        let bytes = parse_memory(m)?;
        cmd = cmd.with_cgroup_memory(bytes);
        // Disable swap for the cgroup so the memory limit acts as a hard
        // ceiling and the OOM killer fires instead of paging to swap.
        // (Matches Docker's --memory-only behaviour on systems with swap.)
        cmd = cmd.with_cgroup_memory_swap(0);
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

    // Capabilities: start from DEFAULT_CAPS, apply --cap-drop then --cap-add.
    // --cap-drop ALL zeros the baseline; individual --cap-drop NAME removes one cap.
    if !args.cap_drop.is_empty() || !args.cap_add.is_empty() {
        let drop_all = args.cap_drop.iter().any(|c| c.eq_ignore_ascii_case("ALL"));
        let mut effective = if drop_all {
            Capability::empty()
        } else {
            Capability::DEFAULT_CAPS
        };
        if !drop_all {
            for cap_name in &args.cap_drop {
                effective &= !parse_capability(cap_name)?;
            }
        }
        for cap_name in &args.cap_add {
            effective |= parse_capability(cap_name)?;
        }
        cmd = cmd.with_capabilities(effective);
    }

    // Security options
    for opt in &args.security_opt {
        let (key, val) = opt.split_once('=').unwrap_or((opt.as_str(), ""));
        match key {
            "seccomp" => match val {
                "default" | "" => cmd = cmd.with_seccomp_default(),
                "minimal" => cmd = cmd.with_seccomp_minimal(),
                "iouring" | "io-uring" => cmd = cmd.with_seccomp_allow_io_uring(),
                "none" => {}
                other => {
                    return Err(format!(
                        "unknown seccomp profile '{}' (use: default, minimal, iouring, none)",
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

    // MAC: AppArmor profile and SELinux label
    if let Some(ref profile) = args.apparmor_profile {
        cmd = cmd.with_apparmor_profile(profile);
    }
    if let Some(ref label) = args.selinux_label {
        cmd = cmd.with_selinux_label(label);
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
            let config = pelagos::paths::network_config_dir(name).join("config.json");
            if config.exists() {
                Ok(NetworkMode::BridgeNamed(name.to_string()))
            } else {
                Err(format!(
                    "unknown network '{}' — use a mode (none, loopback, bridge, pasta) \
                     or create it first: pelagos network create {} --subnet CIDR",
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
) -> Result<Vec<(u16, u16, pelagos::network::PortProto)>, Box<dyn std::error::Error>> {
    use pelagos::network::PortProto;
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
    auto_remove: bool,
    spawn_config: Option<SpawnConfig>,
    labels: std::collections::HashMap<String, String>,
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
        spawn_config,
        labels,
        mnt_ns_inode: None,
    };
    write_state(&state)?;

    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {}", e))?;
    let pid = child.pid();

    // Update state with real PID, mount-namespace inode, and network IPs.
    let mut state2 = state;
    state2.pid = pid;
    state2.mnt_ns_inode = super::read_mnt_ns_inode(pid);
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

    if auto_remove {
        // Remove state directory immediately; ignore errors (best-effort).
        let dir = super::container_dir(&name);
        let _ = std::fs::remove_dir_all(&dir);
    } else {
        // Update final state
        state2.status = ContainerStatus::Exited;
        state2.exit_code = Some(code);
        write_state(&state2)?;
    }

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
    spawn_config: Option<SpawnConfig>,
    labels: std::collections::HashMap<String, String>,
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
        spawn_config,
        labels,
        mnt_ns_inode: None,
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

            // Become a subreaper: any orphaned descendant is re-parented to us
            // instead of host init.  This ensures that if the watcher is killed
            // (e.g. OOM), the container process (C) is re-parented to us rather
            // than to PID 1, so our eventual exit causes C's PR_SET_PDEATHSIG to
            // fire directly — one hop, no fragile two-hop chain.
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };

            // Record watcher_pid immediately so `pelagos ps` can verify we are alive
            // via check_liveness(watcher_pid) while pid==0 (container not yet spawned).
            // Without this, ps would see pid=0, call check_liveness(0) → false, and
            // permanently mark the container as Exited before the container starts.
            let watcher_pid = unsafe { libc::getpid() };
            {
                let mut early = state.clone();
                early.watcher_pid = watcher_pid;
                let _ = write_state(&early);
            }

            // Set up piped stdio so we can relay to log files.
            cmd = cmd
                .stdin(Stdio::Null)
                .stdout(Stdio::Piped)
                .stderr(Stdio::Piped);

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("pelagos watcher: spawn failed: {}", e);
                    unsafe { libc::_exit(1) };
                }
            };
            let pid = child.pid();

            // Store the container PID so the signal handler can forward signals to it,
            // then install SIGTERM/SIGINT handlers.  Any signal sent to the watcher
            // (e.g. `kill <watcher_pid>`) is forwarded to the container, which then
            // exits normally, causing child.wait() to return and teardown to run.
            WATCHER_CONTAINER_PID.store(pid as i32, Ordering::Relaxed);
            unsafe {
                libc::signal(
                    libc::SIGTERM,
                    watcher_forward_signal as *const () as libc::sighandler_t,
                );
                libc::signal(
                    libc::SIGINT,
                    watcher_forward_signal as *const () as libc::sighandler_t,
                );
            }

            // Update state with real PIDs, mount-namespace inode, and network IPs.
            let mut updated = state;
            updated.pid = pid;
            updated.mnt_ns_inode = super::read_mnt_ns_inode(pid);
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

            // Single epoll relay thread: multiplexes stdout and stderr into log files.
            let t_relay = super::relay::start_log_relay(
                child.take_stdout(),
                child.take_stderr(),
                stdout_log.clone(),
                stderr_log.clone(),
            );

            // Wait for the container to exit.
            let exit = match child.wait() {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("pelagos watcher: wait failed: {}", e);
                    unsafe { libc::_exit(1) };
                }
            };
            // Signal the health monitor to stop; join relay and health threads.
            health_stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = t_relay.join();
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
        let net_def = match pelagos::network::load_network_def(net_name) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if let Err(e) = pelagos::dns::dns_add_entry(
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
        if let Err(e) = pelagos::dns::dns_remove_entry(net_name, container_name) {
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
        assert_eq!(fwds[0], (8080, 80, pelagos::network::PortProto::Tcp));
    }

    #[test]
    fn test_parse_port_forwards_explicit_tcp() {
        let specs = vec!["8080:80/tcp".to_string()];
        let fwds = parse_port_forwards(&specs).unwrap();
        assert_eq!(fwds[0], (8080, 80, pelagos::network::PortProto::Tcp));
    }

    #[test]
    fn test_parse_port_forwards_udp() {
        let specs = vec!["5353:53/udp".to_string()];
        let fwds = parse_port_forwards(&specs).unwrap();
        assert_eq!(fwds[0], (5353, 53, pelagos::network::PortProto::Udp));
    }

    #[test]
    fn test_parse_port_forwards_both() {
        let specs = vec!["53:53/both".to_string()];
        let fwds = parse_port_forwards(&specs).unwrap();
        assert_eq!(fwds[0], (53, 53, pelagos::network::PortProto::Both));
    }

    #[test]
    fn test_parse_port_forwards_multiple() {
        let specs = vec!["80:80/tcp".to_string(), "5353:53/udp".to_string()];
        let fwds = parse_port_forwards(&specs).unwrap();
        assert_eq!(fwds.len(), 2);
        assert_eq!(fwds[0].2, pelagos::network::PortProto::Tcp);
        assert_eq!(fwds[1].2, pelagos::network::PortProto::Udp);
    }

    #[test]
    fn test_parse_port_forwards_invalid() {
        assert!(parse_port_forwards(&["notaport".to_string()]).is_err());
        assert!(parse_port_forwards(&["abc:80".to_string()]).is_err());
    }
}
