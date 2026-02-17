//! Native container networking — N1 (loopback) and N2 (veth + bridge).
//!
//! ## Architecture
//!
//! - **N1 loopback**: [`bring_up_loopback`] is called inside the container's
//!   `pre_exec` closure, after `unshare(CLONE_NEWNET)`, using `ioctl(SIOCSIFFLAGS)`
//!   to set `IFF_UP` on `lo`. The kernel then automatically activates 127.0.0.1.
//!
//! - **N2 bridge**: [`setup_bridge_network`] is called by the parent *after*
//!   `fork()`. It shells out to `ip` and `nsenter` to:
//!   1. Ensure the `remora0` bridge exists (172.19.0.1/24)
//!   2. Create a `veth-{pid}` / `eth0` pair
//!   3. Move `eth0` into the container's netns via `/proc/{pid}/ns/net`
//!   4. Assign an IP from the 172.19.0.x/24 range (IPAM via file lock)
//!   5. Add default route inside the container
//!   6. Attach the host-side veth to `remora0`
//!
//! Teardown removes the host-side veth (`ip link del`), which cascades to the
//! container-side peer automatically.

use std::io::{self, Read, Seek, SeekFrom, Write as IoWrite};
use std::net::Ipv4Addr;
use std::os::unix::io::AsRawFd;
use std::process::Command as SysCmd;

/// Bridge name used by all Remora containers.
pub const BRIDGE_NAME: &str = "remora0";
/// Gateway IP assigned to the bridge (also the default route for containers).
pub const BRIDGE_GW: &str = "172.19.0.1";
/// CIDR for the bridge subnet.
const BRIDGE_CIDR: &str = "172.19.0.1/24";
/// Directory for Remora runtime state (IPAM file, etc.).
const REMORA_RUN_DIR: &str = "/run/remora";
/// Tracks the next IP to allocate; protected by flock.
const IPAM_FILE: &str = "/run/remora/next_ip";

// ── Public types ─────────────────────────────────────────────────────────────

/// Container network mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkMode {
    /// Share the host's network stack (default — no changes).
    None,
    /// Isolated network namespace with loopback only.
    Loopback,
    /// Full connectivity via the `remora0` bridge (172.19.0.x/24).
    Bridge,
}

/// Network configuration for a container.
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub mode: NetworkMode,
}

/// Runtime state from setting up bridge networking; needed for teardown.
#[derive(Debug)]
pub struct NetworkSetup {
    /// Name of the host-side veth interface (e.g. `veth-12345`).
    pub veth_host: String,
    /// IP assigned to the container inside `remora0`'s subnet.
    pub container_ip: Ipv4Addr,
}

// ── N1: Loopback ─────────────────────────────────────────────────────────────

/// Bring up the loopback interface (`lo`) inside the current network namespace.
///
/// Must be called **from within the container process** (inside `pre_exec`),
/// after `unshare(CLONE_NEWNET)`. Uses `SIOCSIFFLAGS` to set `IFF_UP`; the
/// kernel then automatically activates `127.0.0.1/8` on the interface.
///
/// # Safety
///
/// Calls `socket(2)`, `ioctl(2)`, and `close(2)` — all async-signal-safe.
pub fn bring_up_loopback() -> io::Result<()> {
    // A minimal ifreq layout sufficient for SIOCGIFFLAGS / SIOCSIFFLAGS:
    //   char   ifr_name[16];   // IFNAMSIZ
    //   short  ifr_flags;      // part of the union
    //   u8     _pad[22];       // rest of the 24-byte union
    #[repr(C)]
    struct Ifreq {
        ifr_name: [u8; 16],
        ifr_flags: libc::c_short,
        _pad: [u8; 22],
    }

    let mut req = Ifreq { ifr_name: [0u8; 16], ifr_flags: 0, _pad: [0u8; 22] };
    req.ifr_name[0] = b'l';
    req.ifr_name[1] = b'o';

    unsafe {
        let sock = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if sock < 0 {
            return Err(io::Error::last_os_error());
        }

        // Get current flags
        let ret = libc::ioctl(sock, libc::SIOCGIFFLAGS, &mut req as *mut Ifreq);
        if ret < 0 {
            let e = io::Error::last_os_error();
            libc::close(sock);
            return Err(e);
        }

        // Set IFF_UP (bit 0)
        req.ifr_flags |= libc::IFF_UP as libc::c_short;

        let ret = libc::ioctl(sock, libc::SIOCSIFFLAGS, &mut req as *mut Ifreq);
        libc::close(sock);

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

// ── N2: Bridge + veth ────────────────────────────────────────────────────────

/// Ensure the `remora0` bridge exists, has its IP, and is up.
///
/// Idempotent — safe to call for every container spawn.
fn ensure_bridge() -> io::Result<()> {
    // Create bridge (ignore error if it already exists)
    let _ = SysCmd::new("ip")
        .args(["link", "add", BRIDGE_NAME, "type", "bridge"])
        .status();

    // Assign gateway IP (ignore error if already assigned)
    let _ = SysCmd::new("ip")
        .args(["addr", "add", BRIDGE_CIDR, "dev", BRIDGE_NAME])
        .status();

    // Bring up (idempotent)
    run("ip", &["link", "set", BRIDGE_NAME, "up"])
}

/// Allocate the next IP from the 172.19.0.x/24 pool.
///
/// Uses `flock(LOCK_EX)` on `/run/remora/next_ip` to serialize concurrent
/// spawns. Wraps at 254 (skipping 0=network, 1=gateway, 255=broadcast).
fn allocate_ip() -> io::Result<Ipv4Addr> {
    std::fs::create_dir_all(REMORA_RUN_DIR)?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(IPAM_FILE)?;

    // Exclusive lock — blocks until other spawns release their lock.
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_EX);
    }

    let mut content = String::new();
    file.read_to_string(&mut content)?;

    let current: u8 = content.trim().parse().unwrap_or(2);
    let ip = Ipv4Addr::new(172, 19, 0, current);

    // Advance, wrapping around. Skip 0, 1 (network/gateway), 255 (broadcast).
    let next = match current.wrapping_add(1) {
        0 | 1 | 255 => 2,
        n => n,
    };

    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    write!(file, "{}", next)?;
    // flock released when `file` is dropped here

    Ok(ip)
}

/// Set up full bridge networking for a container.
///
/// Called **from the parent process** after `fork()`, before returning the
/// `Child` handle to the caller. The container's network namespace is
/// identified by `/proc/{child_pid}/ns/net`.
///
/// Returns a [`NetworkSetup`] that must be passed to [`teardown_network`]
/// after the container exits.
pub fn setup_bridge_network(child_pid: u32) -> io::Result<NetworkSetup> {
    ensure_bridge()?;

    let container_ip = allocate_ip()?;
    let veth_host = format!("veth-{}", child_pid);
    let netns = format!("/proc/{}/ns/net", child_pid);

    // Create veth pair: veth-{pid} (host side) <-> eth0 (container side)
    run("ip", &[
        "link", "add", &veth_host,
        "type", "veth",
        "peer", "name", "eth0",
    ])?;

    // Move eth0 into the container's network namespace
    run("ip", &["link", "set", "eth0", "netns", &child_pid.to_string()])?;

    let ns_arg = format!("--net={}", netns);
    let ip_cidr = format!("{}/24", container_ip);

    // Configure eth0 inside the container via nsenter
    run("nsenter", &[&ns_arg, "--", "ip", "addr", "add", &ip_cidr, "dev", "eth0"])?;
    run("nsenter", &[&ns_arg, "--", "ip", "link", "set", "eth0", "up"])?;
    run("nsenter", &[&ns_arg, "--", "ip", "link", "set", "lo", "up"])?;
    run("nsenter", &[&ns_arg, "--", "ip", "route", "add", "default", "via", BRIDGE_GW])?;

    // Attach host-side veth to the bridge and bring it up
    run("ip", &["link", "set", &veth_host, "master", BRIDGE_NAME])?;
    run("ip", &["link", "set", &veth_host, "up"])?;

    Ok(NetworkSetup { veth_host, container_ip })
}

/// Remove the container's veth pair.
///
/// Deleting the host-side veth cascades: the kernel removes it from the bridge
/// and destroys the container-side peer. Errors are non-fatal (logged via
/// `eprintln!`).
pub fn teardown_network(setup: &NetworkSetup) {
    if let Err(e) = run("ip", &["link", "del", &setup.veth_host]) {
        log::warn!("network teardown (non-fatal): {}", e);
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Run a command, returning an error if it exits non-zero.
fn run(cmd: &str, args: &[&str]) -> io::Result<()> {
    let status = SysCmd::new(cmd).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "`{} {}` exited with {}",
            cmd,
            args.join(" "),
            status
        )))
    }
}
