//! DNS daemon management — start/stop/update the `remora-dns` daemon.
//!
//! The DNS daemon provides container name resolution for bridge networks.
//! It runs as a separate process (`remora-dns`), listening on bridge gateway
//! IPs for A-record queries and resolving container names to their bridge IPs.
//!
//! Config files are stored in `<runtime>/dns/`, one per network. The daemon
//! reloads on SIGHUP when entries change and auto-exits when all config files
//! are empty.

use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;

/// Config directory for DNS daemon files.
pub fn dns_config_dir() -> PathBuf {
    crate::paths::dns_config_dir()
}

/// Read the daemon PID from the PID file. Returns `None` if not running.
fn daemon_pid() -> Option<i32> {
    let pid_file = crate::paths::dns_pid_file();
    let content = std::fs::read_to_string(pid_file).ok()?;
    let pid: i32 = content.trim().parse().ok()?;
    // Check if the process is actually alive.
    if unsafe { libc::kill(pid, 0) } == 0 {
        Some(pid)
    } else {
        None
    }
}

/// Send SIGHUP to the DNS daemon (reload config).
fn signal_reload() -> io::Result<()> {
    if let Some(pid) = daemon_pid() {
        let ret = unsafe { libc::kill(pid, libc::SIGHUP) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Start the DNS daemon if not already running.
///
/// Double-forks to daemonize. The daemon binary (`remora-dns`) is expected to
/// be in the same directory as the current executable. Idempotent: if the PID
/// file exists and the process is alive, does nothing.
pub fn ensure_dns_daemon() -> io::Result<()> {
    // Already running?
    if daemon_pid().is_some() {
        return Ok(());
    }

    let config_dir = dns_config_dir();
    std::fs::create_dir_all(&config_dir)?;

    // Find the remora-dns binary next to the current executable.
    let dns_bin = find_dns_binary()?;

    log::info!("starting DNS daemon: {}", dns_bin.display());

    // Double-fork to daemonize.
    let fork1 = unsafe { libc::fork() };
    match fork1 {
        -1 => return Err(io::Error::last_os_error()),
        0 => {
            // First child: setsid + second fork.
            unsafe { libc::setsid() };
            let fork2 = unsafe { libc::fork() };
            match fork2 {
                -1 => unsafe { libc::_exit(1) },
                0 => {
                    // Grandchild: exec the DNS daemon.
                    // Redirect stdin/stdout/stderr to /dev/null.
                    let devnull = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
                    if devnull >= 0 {
                        unsafe {
                            libc::dup2(devnull, 0);
                            libc::dup2(devnull, 1);
                            // Keep stderr for daemon's own logging
                            libc::close(devnull);
                        }
                    }

                    let config_dir_str = config_dir.to_string_lossy().to_string();
                    let err = exec_dns_binary(&dns_bin, &config_dir_str);
                    eprintln!("remora: failed to exec remora-dns: {}", err);
                    unsafe { libc::_exit(1) };
                }
                _ => {
                    // First child exits immediately.
                    unsafe { libc::_exit(0) };
                }
            }
        }
        child_pid => {
            // Parent: wait for first child to exit.
            unsafe {
                libc::waitpid(child_pid, std::ptr::null_mut(), 0);
            }
            // Give the daemon a moment to start and write its PID file.
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    Ok(())
}

/// Find the `remora-dns` binary. Looks next to the current executable first,
/// then falls back to PATH.
fn find_dns_binary() -> io::Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        // Try next to current executable (e.g. target/debug/remora-dns).
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("remora-dns");
            if candidate.exists() {
                return Ok(candidate);
            }
            // During `cargo test`, exe is in target/debug/deps/ — try parent.
            if let Some(parent) = dir.parent() {
                let candidate = parent.join("remora-dns");
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }
    }

    // Fall back to PATH lookup.
    if let Ok(output) = std::process::Command::new("which")
        .arg("remora-dns")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    Err(io::Error::other(
        "remora-dns binary not found (expected next to remora binary or in PATH)",
    ))
}

/// Exec the DNS binary (called in the grandchild after double-fork).
fn exec_dns_binary(bin: &std::path::Path, config_dir: &str) -> io::Error {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let bin_c = CString::new(bin.as_os_str().as_bytes()).unwrap();
    let arg_config = CString::new("--config-dir").unwrap();
    let arg_dir = CString::new(config_dir).unwrap();
    let args = [
        bin_c.as_ptr(),
        arg_config.as_ptr(),
        arg_dir.as_ptr(),
        std::ptr::null(),
    ];

    unsafe {
        libc::execv(bin_c.as_ptr(), args.as_ptr());
    }
    io::Error::last_os_error()
}

/// Add a container entry to a network's DNS config file.
///
/// Creates the file if it doesn't exist (first container on network).
/// Sends SIGHUP to the daemon to reload. Starts the daemon if not running.
pub fn dns_add_entry(
    network_name: &str,
    container_name: &str,
    ip: Ipv4Addr,
    gateway: Ipv4Addr,
    upstream: &[String],
) -> io::Result<()> {
    let config_dir = dns_config_dir();
    std::fs::create_dir_all(&config_dir)?;

    let config_file = crate::paths::dns_network_file(network_name);

    // Use file locking to prevent races between concurrent `remora run` invocations.
    let lock_path = config_dir.join(format!("{}.lock", network_name));
    let lock_file = std::fs::File::create(&lock_path)?;
    flock_exclusive(&lock_file)?;

    // Read existing content or create new.
    let content = std::fs::read_to_string(&config_file).unwrap_or_default();

    let new_content = if content.is_empty() {
        // Create new config file with header.
        let upstream_str = if upstream.is_empty() {
            "8.8.8.8,1.1.1.1".to_string()
        } else {
            upstream.join(",")
        };
        format!("{} {}\n{} {}\n", gateway, upstream_str, container_name, ip)
    } else {
        // Append entry (remove old entry for same container name first).
        let mut lines: Vec<String> = content
            .lines()
            .filter(|line| {
                // Keep lines that don't start with this container name.
                let first_word = line.split_whitespace().next().unwrap_or("");
                first_word != container_name
            })
            .map(|s| s.to_string())
            .collect();
        lines.push(format!("{} {}", container_name, ip));
        lines.join("\n") + "\n"
    };

    std::fs::write(&config_file, new_content)?;

    // Drop the lock before signaling.
    drop(lock_file);
    let _ = std::fs::remove_file(&lock_path);

    // Ensure firewall allows DNS on this bridge.
    if let Ok(net_def) = crate::network::load_network_def(network_name) {
        allow_dns_on_bridge(&net_def.bridge_name);
    }

    // Ensure daemon is running and signal reload.
    ensure_dns_daemon()?;
    signal_reload()
}

/// Remove a container entry from a network's DNS config file.
///
/// If the file becomes empty (no containers), removes it. Sends SIGHUP to
/// the daemon to reload. If all config files are gone, the daemon will
/// auto-exit on the next reload.
pub fn dns_remove_entry(network_name: &str, container_name: &str) -> io::Result<()> {
    let config_dir = dns_config_dir();
    let config_file = crate::paths::dns_network_file(network_name);

    if !config_file.exists() {
        return Ok(());
    }

    // Use file locking.
    let lock_path = config_dir.join(format!("{}.lock", network_name));
    let lock_file = std::fs::File::create(&lock_path)?;
    flock_exclusive(&lock_file)?;

    let content = std::fs::read_to_string(&config_file)?;
    let mut header = String::new();
    let mut entries = Vec::new();

    for (i, line) in content.lines().enumerate() {
        if i == 0 {
            header = line.to_string();
            continue;
        }
        let first_word = line.split_whitespace().next().unwrap_or("");
        if first_word != container_name && !line.trim().is_empty() {
            entries.push(line.to_string());
        }
    }

    if entries.is_empty() {
        // No more containers on this network — remove config file and firewall rule.
        let _ = std::fs::remove_file(&config_file);
        if let Ok(net_def) = crate::network::load_network_def(network_name) {
            disallow_dns_on_bridge(&net_def.bridge_name);
        }
    } else {
        let mut new_content = header + "\n";
        for entry in &entries {
            new_content.push_str(entry);
            new_content.push('\n');
        }
        std::fs::write(&config_file, new_content)?;
    }

    // Drop the lock before signaling.
    drop(lock_file);
    let _ = std::fs::remove_file(&lock_path);

    // Signal reload.
    signal_reload()
}

/// Add an iptables INPUT rule to allow UDP port 53 on a bridge interface.
///
/// Hosts with restrictive INPUT policies (DROP/REJECT) block DNS queries
/// from containers to the gateway. This rule ensures the DNS daemon can
/// receive queries on the bridge.
fn allow_dns_on_bridge(bridge: &str) {
    use std::process::Command as SysCmd;

    // Purge any stale duplicates first.
    while SysCmd::new("iptables")
        .args([
            "-D", "INPUT", "-i", bridge, "-p", "udp", "--dport", "53", "-j", "ACCEPT",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
    {}

    // Insert fresh rule.
    let _ = SysCmd::new("iptables")
        .args([
            "-I", "INPUT", "-i", bridge, "-p", "udp", "--dport", "53", "-j", "ACCEPT",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Remove the iptables INPUT rule for DNS on a bridge interface.
fn disallow_dns_on_bridge(bridge: &str) {
    use std::process::Command as SysCmd;

    while SysCmd::new("iptables")
        .args([
            "-D", "INPUT", "-i", bridge, "-p", "udp", "--dport", "53", "-j", "ACCEPT",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
    {}
}

/// Apply an exclusive flock on the file.
fn flock_exclusive(file: &std::fs::File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
