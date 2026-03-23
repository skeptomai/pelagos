//! `pelagos compose` — multi-service orchestration with Lisp compose files (`.reml`).

use super::{
    check_liveness, container_dir, containers_dir, now_iso8601, write_state, ContainerState,
    ContainerStatus, HealthStatus,
};
use pelagos::compose::{
    parse_compose, topo_sort, ComposeFile, Dependency, HealthCheck, ServiceSpec,
};
use pelagos::container::{Command, Namespace, Stdio, Volume};
use pelagos::lisp::{HookMap, Interpreter};
use pelagos::network::NetworkMode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// CLI args (clap)
// ---------------------------------------------------------------------------

#[derive(Debug, clap::Subcommand)]
pub enum ComposeCmd {
    /// Start all services defined in the compose file
    Up {
        /// Path to compose file (default: compose.reml)
        #[clap(long = "file", short = 'f', default_value = "compose.reml")]
        file: PathBuf,
        /// Project name (default: parent directory name)
        #[clap(long = "project", short = 'p')]
        project: Option<String>,
        /// Run in foreground (don't daemonise)
        #[clap(long)]
        foreground: bool,
    },
    /// Stop and remove all services
    Down {
        /// Path to compose file (default: compose.reml)
        #[clap(long = "file", short = 'f', default_value = "compose.reml")]
        file: PathBuf,
        /// Project name
        #[clap(long = "project", short = 'p')]
        project: Option<String>,
        /// Also remove volumes
        #[clap(long = "volumes", short = 'v')]
        volumes: bool,
    },
    /// List services in the compose project
    Ps {
        /// Path to compose file (default: compose.reml)
        #[clap(long = "file", short = 'f', default_value = "compose.reml")]
        file: PathBuf,
        /// Project name
        #[clap(long = "project", short = 'p')]
        project: Option<String>,
    },
    /// View logs for compose services
    Logs {
        /// Path to compose file (default: compose.reml)
        #[clap(long = "file", short = 'f', default_value = "compose.reml")]
        file: PathBuf,
        /// Project name
        #[clap(long = "project", short = 'p')]
        project: Option<String>,
        /// Follow log output
        #[clap(long)]
        follow: bool,
        /// Service name (show all if omitted)
        service: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Project state (persisted as JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeProject {
    pub name: String,
    pub file_path: String,
    pub services: HashMap<String, ComposeServiceState>,
    pub networks: Vec<String>,
    pub volumes: Vec<String>,
    pub supervisor_pid: i32,
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeServiceState {
    pub container_name: String,
    pub status: String,
    pub pid: i32,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub fn cmd_compose(cmd: ComposeCmd) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        ComposeCmd::Up {
            file,
            project,
            foreground,
        } => cmd_compose_up(&file, project.as_deref(), foreground),
        ComposeCmd::Down {
            file,
            project,
            volumes,
        } => cmd_compose_down(&file, project.as_deref(), volumes),
        ComposeCmd::Ps { file, project } => cmd_compose_ps(&file, project.as_deref()),
        ComposeCmd::Logs {
            file,
            project,
            follow,
            service,
        } => cmd_compose_logs(&file, project.as_deref(), follow, service.as_deref()),
    }
}

// ---------------------------------------------------------------------------
// compose up
// ---------------------------------------------------------------------------

fn cmd_compose_up(
    file: &std::path::Path,
    project_name: Option<&str>,
    foreground: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    cmd_compose_up_reml(file, project_name, foreground)
}

fn cmd_compose_up_reml(
    file: &std::path::Path,
    project_name: Option<&str>,
    foreground: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Derive a preliminary project name from the file path so that
    // imperative `container-start` calls during eval use the right scope.
    let preliminary_project = if let Some(name) = project_name {
        name.to_string()
    } else {
        derive_project_name(file, None)?
    };
    let compose_dir = file
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();

    let mut interp = Interpreter::new_with_runtime(preliminary_project, compose_dir.clone());
    interp
        .eval_file(file)
        .map_err(|e| format!("{}: {}", file.display(), e))?;

    // If the script never called (compose-up ...) it was purely imperative:
    // containers were started/stopped during evaluation.  Nothing more to do.
    let pending = match interp.take_pending() {
        Some(p) => p,
        None => return Ok(()),
    };
    let spec = pending.spec.ok_or("compose-up: missing spec")?;
    let effective_foreground = pending.foreground || foreground;

    // Honour explicit project override; fall back to spec or file-directory.
    let project = if let Some(name) = project_name {
        name.to_string()
    } else if let Some(name) = pending.project {
        name
    } else {
        derive_project_name(file, None)?
    };

    let hooks = interp.take_hooks();
    let order = topo_sort(&spec.services)?;

    // Check for already-running project.
    let state_file = pelagos::paths::compose_state_file(&project);
    if state_file.exists() {
        if let Ok(existing) = load_project_state(&project) {
            if existing.supervisor_pid > 0 && check_liveness(existing.supervisor_pid) {
                return Err(format!(
                    "project '{}' is already running (supervisor PID {})",
                    project, existing.supervisor_pid
                )
                .into());
            }
        }
    }

    let mut created_networks = Vec::new();
    for net in &spec.networks {
        let scoped = scoped_network_name(&project, &net.name);
        let config = pelagos::paths::network_config_dir(&scoped).join("config.json");
        if !config.exists() {
            let subnet = net.subnet.as_deref().unwrap_or("10.99.0.0/24");
            super::network::cmd_network_create(&scoped, subnet)
                .map_err(|e| format!("compose: failed to create network '{}': {}", scoped, e))?;
        }
        created_networks.push(scoped);
    }

    let mut created_volumes = Vec::new();
    for vol in &spec.volumes {
        let scoped = scoped_volume_name(&project, vol);
        let _ = Volume::open(&scoped).or_else(|_| Volume::create(&scoped));
        created_volumes.push(scoped);
    }

    for net in &created_networks {
        let dns_file = pelagos::paths::dns_network_file(net);
        if dns_file.exists() {
            let _ = std::fs::remove_file(&dns_file);
        }
    }

    if effective_foreground {
        run_compose_with_hooks(
            &project,
            file,
            &spec,
            &order,
            &created_networks,
            &created_volumes,
            &hooks,
            &compose_dir,
        )
    } else {
        let fork_result = unsafe { libc::fork() };
        match fork_result {
            -1 => Err(std::io::Error::last_os_error().into()),
            0 => {
                unsafe { libc::setsid() };
                // Become a subreaper so orphaned container descendants are
                // re-parented to us, not host init.
                unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
                if let Err(e) = run_compose_with_hooks(
                    &project,
                    file,
                    &spec,
                    &order,
                    &created_networks,
                    &created_volumes,
                    &hooks,
                    &compose_dir,
                ) {
                    log::error!("compose supervisor: {}", e);
                    unsafe { libc::_exit(1) };
                }
                unsafe { libc::_exit(0) };
            }
            _child_pid => {
                std::thread::sleep(Duration::from_millis(200));
                println!("Project '{}' started", project);
                Ok(())
            }
        }
    }
}

/// Run compose supervision with optional `on-ready` hooks.
///
/// Called by the compose supervisor to run services with optional `on-ready` hooks.
#[allow(clippy::too_many_arguments)]
pub fn run_compose_with_hooks(
    project: &str,
    file: &std::path::Path,
    compose: &ComposeFile,
    order: &[String],
    created_networks: &[String],
    created_volumes: &[String],
    on_ready: &HookMap,
    compose_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let supervisor_pid = unsafe { libc::getpid() };

    let mut project_state = ComposeProject {
        name: project.to_string(),
        file_path: file.to_string_lossy().into_owned(),
        services: HashMap::new(),
        networks: created_networks.to_vec(),
        volumes: created_volumes.to_vec(),
        supervisor_pid,
        started_at: now_iso8601(),
    };
    save_project_state(&project_state)?;

    let svc_map: HashMap<&str, &ServiceSpec> = compose
        .services
        .iter()
        .map(|s| (s.name.as_str(), s))
        .collect();

    let mut container_pids: HashMap<String, i32> = HashMap::new();
    let mut container_ips: HashMap<String, String> = HashMap::new();

    for svc_name in order {
        let svc = svc_map[svc_name.as_str()];
        let container_name = scoped_container_name(project, svc_name);

        for dep in &svc.depends_on {
            wait_for_dependency(project, dep, &container_pids, &container_ips)?;
        }

        log::info!(
            "compose: starting service '{}' as '{}'",
            svc_name,
            container_name
        );
        let pid = spawn_service(project, svc, &container_name, compose, compose_dir)?;

        if let Ok(cstate) = super::read_state(&container_name) {
            if let Some(ip) = cstate.bridge_ip.as_ref() {
                container_ips.insert(svc_name.clone(), ip.clone());
            }
            for (net, ip) in &cstate.network_ips {
                container_ips.insert(svc_name.clone(), ip.clone());
                let _ = net;
            }
        }

        container_pids.insert(svc_name.clone(), pid);

        // Fire on-ready hooks for this service.
        if let Some(hooks) = on_ready.get(svc_name.as_str()) {
            for hook in hooks {
                hook().map_err(|e| format!("on-ready '{}': {}", svc_name, e))?;
            }
        }

        project_state.services.insert(
            svc_name.clone(),
            ComposeServiceState {
                container_name: container_name.clone(),
                status: "running".into(),
                pid,
            },
        );
        save_project_state(&project_state)?;
    }

    println!("All services started for project '{}'", project);

    loop {
        std::thread::sleep(Duration::from_secs(2));
        let mut all_exited = true;
        for (svc_name, svc_state) in &mut project_state.services {
            if svc_state.status == "exited" {
                continue;
            }
            if check_liveness(svc_state.pid) {
                all_exited = false;
            } else {
                log::info!("compose: service '{}' exited", svc_name);
                svc_state.status = "exited".into();
            }
        }
        save_project_state(&project_state)?;
        if all_exited {
            log::info!("compose: all services exited for project '{}'", project);
            break;
        }
    }

    Ok(())
}

fn spawn_service(
    project: &str,
    svc: &ServiceSpec,
    container_name: &str,
    compose: &ComposeFile,
    compose_dir: &std::path::Path,
) -> Result<i32, Box<dyn std::error::Error>> {
    // Resolve image layers.
    let image_ref = &svc.image;
    let (full_ref, manifest) = resolve_image(image_ref)?;

    let layers = pelagos::image::layer_dirs(&manifest);
    if layers.is_empty() {
        return Err(format!("service '{}': image has no layers", svc.name).into());
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
        let (uid, gid) = super::parse_user_in_layers(&manifest.config.user, &layer_dirs)?;
        cmd = cmd.with_uid(uid);
        if let Some(g) = gid {
            cmd = cmd.with_gid(g);
        }
    }

    // Apply service-specific settings.

    // Networks: first is primary, rest are additional.
    let mut svc_network_names: Vec<String> = svc
        .networks
        .iter()
        .map(|n| scoped_network_name(project, n))
        .collect();
    if svc_network_names.is_empty() {
        // If no networks specified but compose has networks, use the first one.
        if let Some(first_net) = compose.networks.first() {
            svc_network_names.push(scoped_network_name(project, &first_net.name));
        }
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

    // Note: DNS nameservers (gateway IPs) are auto-injected by the container
    // runtime's spawn() — no explicit with_dns() needed here.

    // Volumes.
    for vol in &svc.volumes {
        let scoped = scoped_volume_name(project, &vol.name);
        let v = Volume::open(&scoped)?;
        cmd = cmd.with_volume(&v, &vol.mount_path);
    }

    // Bind mounts. Relative host paths are resolved against the compose file's directory.
    for bm in &svc.bind_mounts {
        let host = if std::path::Path::new(&bm.host_path).is_relative() {
            compose_dir
                .join(&bm.host_path)
                .canonicalize()
                .map_err(|e| {
                    format!(
                        "service '{}': bind-mount host path '{}': {}",
                        svc.name, bm.host_path, e
                    )
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

    // tmpfs mounts: in-memory writable filesystems (empty options = kernel defaults).
    for path in &svc.tmpfs_mounts {
        cmd = cmd.with_tmpfs(path, "");
    }

    // Environment: service overrides, then a fallback PATH only if the image
    // didn't already supply one (to avoid clobbering bundler/gem/nvm paths).
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
        let bytes = super::parse_memory(mem)?;
        cmd = cmd.with_cgroup_memory(bytes);
    }
    if let Some(ref cpus) = svc.cpus {
        let (quota, period) = super::parse_cpus(cpus)?;
        cmd = cmd.with_cgroup_cpu_quota(quota, period);
    }

    // User.
    if let Some(ref u) = svc.user {
        let (uid, gid) = super::parse_user_in_layers(u, &layer_dirs)?;
        cmd = cmd.with_uid(uid);
        if let Some(g) = gid {
            cmd = cmd.with_gid(g);
        }
    }

    // Workdir.
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
        .with_hostname(container_name);

    // Security: seccomp + capabilities + no-new-privileges + masked paths.
    //
    // Start from DEFAULT_CAPS (the Podman 11-cap set), then apply (cap-drop ...)
    // and (cap-add ...) from the service spec.  (cap-drop "ALL") drops everything
    // first, giving a zero-cap baseline before any cap-add is applied.
    let drop_all = svc.cap_drop.iter().any(|c| c.eq_ignore_ascii_case("ALL"));
    let mut effective_caps = if drop_all {
        pelagos::container::Capability::empty()
    } else {
        pelagos::container::Capability::DEFAULT_CAPS
    };
    if !drop_all && !svc.cap_drop.is_empty() {
        effective_caps &= !super::parse_capability_mask(&svc.cap_drop);
    }
    if !svc.cap_add.is_empty() {
        effective_caps |= super::parse_capability_mask(&svc.cap_add);
    }
    cmd = cmd
        .with_seccomp_default()
        .with_capabilities(effective_caps)
        .with_no_new_privileges(true)
        .with_masked_paths_default();

    if let Some(ref profile) = svc.apparmor_profile {
        cmd = cmd.with_apparmor_profile(profile);
    }
    if let Some(ref label) = svc.selinux_label {
        cmd = cmd.with_selinux_label(label);
    }

    // Spawn detached with log capture.
    std::fs::create_dir_all(containers_dir())?;
    let dir = container_dir(container_name);
    std::fs::create_dir_all(&dir)?;

    let stdout_log = dir.join("stdout.log");
    let stderr_log = dir.join("stderr.log");

    cmd = cmd
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn '{}' failed: {}", svc.name, e))?;
    let pid = child.pid();

    // Write container state.
    let cstate = ContainerState {
        name: container_name.to_string(),
        rootfs: full_ref,
        status: ContainerStatus::Running,
        pid,
        watcher_pid: unsafe { libc::getpid() },
        started_at: now_iso8601(),
        exit_code: None,
        command: exe_and_args.clone(),
        stdout_log: Some(stdout_log.to_string_lossy().into_owned()),
        stderr_log: Some(stderr_log.to_string_lossy().into_owned()),
        bridge_ip: child.container_ip(),
        network_ips: child
            .container_ips()
            .into_iter()
            .map(|(name, ip)| (name.to_string(), ip))
            .collect(),
        health: None,
        health_config: None,
        spawn_config: None,
        labels: std::collections::HashMap::new(),
        mnt_ns_inode: super::read_mnt_ns_inode(pid),
        upper_dir: None,
    };
    write_state(&cstate)?;

    // Register DNS with bare service name.
    let all_ips: Vec<(String, String)> = cstate
        .network_ips
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (net_name, ip_str) in &all_ips {
        let ip: Ipv4Addr = match ip_str.parse() {
            Ok(ip) => ip,
            Err(_) => continue,
        };
        let net_def = match pelagos::network::load_network_def(net_name) {
            Ok(d) => d,
            Err(_) => continue,
        };
        // Register with the bare service name (not project-prefixed).
        if let Err(e) = pelagos::dns::dns_add_entry(
            net_name,
            &svc.name,
            ip,
            net_def.gateway,
            &["8.8.8.8".to_string(), "1.1.1.1".to_string()],
        ) {
            log::warn!(
                "dns: failed to register '{}' on {}: {}",
                svc.name,
                net_name,
                e
            );
        }
    }

    // Wait for DNS daemon to be reachable on each network's gateway.
    // After dns_add_entry sends SIGHUP, the daemon needs time to reload
    // and bind new sockets. Without this, dependent services may start
    // before DNS is ready.
    for (net_name, _) in &all_ips {
        if let Ok(net_def) = pelagos::network::load_network_def(net_name) {
            let gw = net_def.gateway;
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if probe_dns(gw, &svc.name) {
                    log::debug!("dns: '{}' resolves on {} via {}", svc.name, net_name, gw);
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    // Single epoll relay thread: multiplexes stdout and stderr into log files.
    let svc_name = svc.name.clone();
    let cn = container_name.to_string();
    super::relay::start_log_relay(
        child.take_stdout(),
        child.take_stderr(),
        stdout_log.clone(),
        stderr_log.clone(),
    );

    // Spawn a waiter thread that updates state when the container exits.
    let cn_wait = cn.clone();
    let all_ips_wait = all_ips.clone();
    let svc_name_wait = svc_name.clone();
    std::thread::spawn(move || {
        let exit = child.wait();
        // Deregister DNS.
        for (net_name, _) in &all_ips_wait {
            let _ = pelagos::dns::dns_remove_entry(net_name, &svc_name_wait);
        }
        // Update container state.
        if let Ok(mut st) = super::read_state(&cn_wait) {
            st.status = ContainerStatus::Exited;
            st.exit_code = exit.ok().and_then(|e| e.code());
            let _ = write_state(&st);
        }
    });

    Ok(pid)
}

fn resolve_image(
    image_ref: &str,
) -> Result<(String, pelagos::image::ImageManifest), Box<dyn std::error::Error>> {
    use pelagos::image;

    if let Ok(m) = image::load_image(image_ref) {
        return Ok((image_ref.to_string(), m));
    }
    let normalised = normalise_image_reference(image_ref);
    let m = image::load_image(&normalised).map_err(|e| {
        format!(
            "image '{}' not found locally (run 'pelagos image pull {}'): {}",
            image_ref, image_ref, e
        )
    })?;
    Ok((normalised, m))
}

fn normalise_image_reference(reference: &str) -> String {
    pelagos::image::normalise_reference(reference)
}

// ---------------------------------------------------------------------------
// compose down
// ---------------------------------------------------------------------------

fn cmd_compose_down(
    file: &std::path::Path,
    project_name: Option<&str>,
    remove_volumes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let project = derive_project_name(file, project_name)?;
    let project_state = load_project_state(&project)
        .map_err(|_| format!("no running project '{}' found", project))?;

    // Kill supervisor if alive.
    if project_state.supervisor_pid > 0 && check_liveness(project_state.supervisor_pid) {
        unsafe { libc::kill(project_state.supervisor_pid, libc::SIGTERM) };
        std::thread::sleep(Duration::from_millis(500));
        if check_liveness(project_state.supervisor_pid) {
            unsafe { libc::kill(project_state.supervisor_pid, libc::SIGKILL) };
        }
    }

    // Stop services in reverse order.
    // Re-read the compose file to get topo order for reverse teardown.
    let order: Vec<String> = if let Ok(content) = std::fs::read_to_string(file) {
        if let Ok(compose) = parse_compose(&content) {
            topo_sort(&compose.services).unwrap_or_default()
        } else {
            project_state.services.keys().cloned().collect()
        }
    } else {
        project_state.services.keys().cloned().collect()
    };

    let reverse_order: Vec<String> = order.into_iter().rev().collect();

    for svc_name in &reverse_order {
        if let Some(svc_state) = project_state.services.get(svc_name) {
            let cn = &svc_state.container_name;
            // SIGTERM
            if svc_state.pid > 0 && check_liveness(svc_state.pid) {
                log::info!("compose: stopping service '{}'", svc_name);
                unsafe { libc::kill(svc_state.pid, libc::SIGTERM) };
                // Wait up to 10s.
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline && check_liveness(svc_state.pid) {
                    std::thread::sleep(Duration::from_millis(250));
                }
                // SIGKILL if still alive.
                if check_liveness(svc_state.pid) {
                    log::warn!("compose: SIGKILL service '{}'", svc_name);
                    unsafe { libc::kill(svc_state.pid, libc::SIGKILL) };
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            // Remove container state.
            let dir = container_dir(cn);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    // Also stop any services not in the current compose file order.
    for (svc_name, svc_state) in &project_state.services {
        if !reverse_order.contains(svc_name) {
            if svc_state.pid > 0 && check_liveness(svc_state.pid) {
                unsafe { libc::kill(svc_state.pid, libc::SIGKILL) };
            }
            let dir = container_dir(&svc_state.container_name);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    // Clean up DNS entries for all services on all networks.
    for svc_name in project_state.services.keys() {
        for net in &project_state.networks {
            let _ = pelagos::dns::dns_remove_entry(net, svc_name);
        }
    }

    // Remove networks.
    for net in &project_state.networks {
        if let Err(e) = super::network::cmd_network_rm(net) {
            log::warn!("compose: failed to remove network '{}': {}", net, e);
        }
    }

    // Remove volumes if requested.
    if remove_volumes {
        for vol in &project_state.volumes {
            if let Err(e) = Volume::delete(vol) {
                log::warn!("compose: failed to remove volume '{}': {}", vol, e);
            }
        }
    }

    // Remove project state.
    let project_dir = pelagos::paths::compose_project_dir(&project);
    let _ = std::fs::remove_dir_all(&project_dir);

    println!("Project '{}' stopped and removed", project);
    Ok(())
}

// ---------------------------------------------------------------------------
// compose ps
// ---------------------------------------------------------------------------

fn cmd_compose_ps(
    file: &std::path::Path,
    project_name: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let project = derive_project_name(file, project_name)?;
    let project_state =
        load_project_state(&project).map_err(|_| format!("no project '{}' found", project))?;

    println!(
        "{:<15} {:<25} {:<10} {:<8}",
        "SERVICE", "CONTAINER", "STATUS", "PID"
    );
    for (svc_name, svc_state) in &project_state.services {
        let status = if check_liveness(svc_state.pid) {
            "running"
        } else {
            "exited"
        };
        println!(
            "{:<15} {:<25} {:<10} {:<8}",
            svc_name, svc_state.container_name, status, svc_state.pid
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// compose logs
// ---------------------------------------------------------------------------

fn cmd_compose_logs(
    file: &std::path::Path,
    project_name: Option<&str>,
    follow: bool,
    service: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let project = derive_project_name(file, project_name)?;
    let project_state =
        load_project_state(&project).map_err(|_| format!("no project '{}' found", project))?;

    let services_to_show: Vec<(&str, &ComposeServiceState)> = if let Some(svc_name) = service {
        let svc_state = project_state
            .services
            .get(svc_name)
            .ok_or_else(|| format!("service '{}' not found in project '{}'", svc_name, project))?;
        vec![(svc_name, svc_state)]
    } else {
        project_state
            .services
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect()
    };

    for (svc_name, svc_state) in &services_to_show {
        // Delegate to the existing logs infrastructure.
        let cn = &svc_state.container_name;
        if let Ok(cstate) = super::read_state(cn) {
            if let Some(ref log_path) = cstate.stdout_log {
                if let Ok(data) = std::fs::read(log_path) {
                    if !data.is_empty() {
                        // Prefix each line with service name.
                        let text = String::from_utf8_lossy(&data);
                        for line in text.lines() {
                            println!("{} | {}", svc_name, line);
                        }
                    }
                }
            }
            if let Some(ref log_path) = cstate.stderr_log {
                if let Ok(data) = std::fs::read(log_path) {
                    if !data.is_empty() {
                        let text = String::from_utf8_lossy(&data);
                        for line in text.lines() {
                            eprintln!("{} | {}", svc_name, line);
                        }
                    }
                }
            }
        }
    }

    if follow {
        // Poll for new content.
        loop {
            std::thread::sleep(Duration::from_millis(500));
            // Simple approach: re-check if any services are still running.
            let mut any_running = false;
            for (_, svc_state) in &services_to_show {
                if check_liveness(svc_state.pid) {
                    any_running = true;
                    break;
                }
            }
            if !any_running {
                break;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Dependency readiness
// ---------------------------------------------------------------------------

fn wait_for_dependency(
    project: &str,
    dep: &Dependency,
    container_pids: &HashMap<String, i32>,
    container_ips: &HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dep_name = &dep.service;

    // Make sure the dependency is running.
    let dep_pid = container_pids.get(dep_name.as_str()).copied();
    if let Some(pid) = dep_pid {
        if !check_liveness(pid) {
            return Err(format!("dependency '{}' exited before becoming ready", dep_name).into());
        }
    }

    // If no health_check, just verify the process is alive and return.
    let check = match &dep.health_check {
        Some(c) => c,
        None => return Ok(()),
    };

    let timeout = Duration::from_secs(60);
    let interval = Duration::from_millis(250);
    let deadline = Instant::now() + timeout;

    // `:condition service_healthy` — poll state.json health field.
    if matches!(check, HealthCheck::Healthy) {
        let scoped_name = scoped_container_name(project, dep_name);
        log::info!(
            "compose: waiting for service '{}' to become healthy",
            dep_name
        );
        while Instant::now() < deadline {
            if let Ok(state) = super::read_state(&scoped_name) {
                if state.health == Some(super::HealthStatus::Healthy) {
                    log::info!("compose: service '{}' is healthy", dep_name);
                    return Ok(());
                }
            }
            if let Some(pid) = dep_pid {
                if !check_liveness(pid) {
                    return Err(format!(
                        "dependency '{}' exited before becoming healthy",
                        dep_name
                    )
                    .into());
                }
            }
            std::thread::sleep(interval);
        }
        return Err(format!(
            "dependency '{}' did not become healthy within {}s",
            dep_name,
            timeout.as_secs()
        )
        .into());
    }

    // Resolve the container IP (needed by Port and Http checks).
    let ip_str = container_ips
        .get(dep_name.as_str())
        .map(|s| s.as_str())
        .unwrap_or("");

    log::info!(
        "compose: waiting for service '{}' to pass health check",
        dep_name
    );

    while Instant::now() < deadline {
        if eval_health_check(check, dep_pid.unwrap_or(0), ip_str) {
            log::info!("compose: service '{}' is ready", dep_name);
            return Ok(());
        }
        // Check the dependency is still alive before sleeping.
        if let Some(pid) = dep_pid {
            if !check_liveness(pid) {
                return Err(format!(
                    "dependency '{}' exited while waiting for health check",
                    dep_name
                )
                .into());
            }
        }
        std::thread::sleep(interval);
    }

    Err(format!(
        "dependency '{}' did not become ready within {}s",
        dep_name,
        timeout.as_secs()
    )
    .into())
}

/// Recursively evaluate a [`HealthCheck`] expression.
///
/// - `pid`: PID of the container process (for `Cmd` checks).
/// - `ip`: container IP string (for `Port` and `Http` checks).
fn eval_health_check(check: &HealthCheck, pid: i32, ip: &str) -> bool {
    match check {
        HealthCheck::Port(p) => try_tcp(ip, *p),
        HealthCheck::Http(url) => try_http(url, ip),
        HealthCheck::Cmd(args) => try_exec(pid, args),
        HealthCheck::And(checks) => checks.iter().all(|c| eval_health_check(c, pid, ip)),
        HealthCheck::Or(checks) => checks.iter().any(|c| eval_health_check(c, pid, ip)),
        // Healthy is handled separately in wait_for_dependency via state.json polling.
        HealthCheck::Healthy => false,
    }
}

/// Attempt a TCP connection to `ip:port` with a 500ms timeout.
fn try_tcp(ip: &str, port: u16) -> bool {
    let addr_str = format!("{}:{}", ip, port);
    let addr: SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// Attempt an HTTP GET to `url`, replacing the host with `container_ip`.
///
/// Returns true if the response status is 2xx (200–299).
fn try_http(url: &str, container_ip: &str) -> bool {
    // Strip scheme.
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);

    // Split host:port from path.
    let (host_port, path) = if let Some(slash) = rest.find('/') {
        (&rest[..slash], &rest[slash..])
    } else {
        (rest, "/")
    };

    // Extract port from host:port, defaulting to 80.
    let port: u16 = if let Some(colon) = host_port.rfind(':') {
        host_port[colon + 1..].parse().unwrap_or(80)
    } else {
        80
    };

    let target_url = format!("http://{}:{}{}", container_ip, port, path);

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(500))
        .build();

    match agent.get(&target_url).call() {
        Ok(resp) => {
            let status = resp.status();
            (200..300).contains(&status)
        }
        Err(_) => false,
    }
}

/// Run `args` inside the container identified by `pid`'s namespaces.
///
/// Returns true if the command exits with status 0.
fn try_exec(pid: i32, args: &[String]) -> bool {
    super::exec::exec_in_container(pid, args).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn derive_project_name(
    file: &std::path::Path,
    explicit: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(name) = explicit {
        return Ok(name.to_string());
    }
    // Use the parent directory name.
    let abs = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let parent = abs
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("default");
    // Sanitise: only alphanumeric + hyphen.
    let sanitised: String = parent
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Ok(sanitised)
}

fn scoped_network_name(project: &str, net: &str) -> String {
    let name = format!("{}-{}", project, net);
    // Kernel IFNAMSIZ limit is 15; bridge name is "rm-{name}".
    // Network name itself is max 12 chars.
    if name.len() > 12 {
        // Truncate and append short hash.
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

fn scoped_volume_name(project: &str, vol: &str) -> String {
    format!("{}-{}", project, vol)
}

fn scoped_container_name(project: &str, service: &str) -> String {
    format!("{}-{}", project, service)
}

/// Send a DNS A-record query for `name` to the given gateway IP on port 53.
/// Returns true if we get any response (even NXDOMAIN means the daemon is alive).
fn probe_dns(gateway: Ipv4Addr, name: &str) -> bool {
    use std::net::UdpSocket;

    let addr = SocketAddr::new(gateway.into(), 53);
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(_) => return false,
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));

    // Build a minimal DNS A query.
    let mut pkt = Vec::with_capacity(64);
    pkt.extend_from_slice(&[0xAB, 0xCD]); // ID
    pkt.extend_from_slice(&[0x01, 0x00]); // Flags: RD=1
    pkt.extend_from_slice(&[0x00, 0x01]); // QDCOUNT=1
    pkt.extend_from_slice(&[0x00, 0x00]); // ANCOUNT
    pkt.extend_from_slice(&[0x00, 0x00]); // NSCOUNT
    pkt.extend_from_slice(&[0x00, 0x00]); // ARCOUNT
                                          // QNAME
    for label in name.split('.') {
        if !label.is_empty() {
            pkt.push(label.len() as u8);
            pkt.extend_from_slice(label.as_bytes());
        }
    }
    pkt.push(0); // Root label
    pkt.extend_from_slice(&[0x00, 0x01]); // QTYPE = A
    pkt.extend_from_slice(&[0x00, 0x01]); // QCLASS = IN

    if sock.send_to(&pkt, addr).is_err() {
        return false;
    }

    let mut buf = [0u8; 512];
    matches!(sock.recv_from(&mut buf), Ok((n, _)) if n >= 12)
}

fn save_project_state(state: &ComposeProject) -> Result<(), Box<dyn std::error::Error>> {
    let dir = pelagos::paths::compose_project_dir(&state.name);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(pelagos::paths::compose_state_file(&state.name), json)?;
    Ok(())
}

fn load_project_state(project: &str) -> Result<ComposeProject, Box<dyn std::error::Error>> {
    let data = std::fs::read_to_string(pelagos::paths::compose_state_file(project))?;
    let state: ComposeProject = serde_json::from_str(&data)?;
    Ok(state)
}
