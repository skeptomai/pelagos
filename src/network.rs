//! Native container networking — N1 (loopback) and N2 (veth + bridge).
//!
//! ## Architecture
//!
//! - **N1 loopback**: [`bring_up_loopback`] is called inside the container's
//!   `pre_exec` closure, after `unshare(CLONE_NEWNET)`, using `ioctl(SIOCSIFFLAGS)`
//!   to set `IFF_UP` on `lo`. The kernel then automatically activates 127.0.0.1.
//!
//! - **N2 bridge**: [`setup_bridge_network`] is called by the parent **before**
//!   `fork()`. It creates a named network namespace (`ip netns add`), fully
//!   configures it (veth pair, IP, routes, bridge attachment), then returns.
//!   The child's `pre_exec` joins the named netns via `setns()`.
//!
//! ### Why named netns (not /proc/{pid}/ns/net)?
//!
//! Opening `/proc/{pid}/ns/net` after `spawn()` races with fast-exiting
//! containers (`exit 0`). A sync pipe in `pre_exec` deadlocks because
//! `std::process::Command::spawn()` blocks until the child `exec()`s via an
//! internal CLOEXEC fail-pipe, and blocking in `pre_exec` prevents `exec()`.
//! Named netns are created *before* fork — no race, no deadlock.
//!
//! Teardown removes the host-side veth (`ip link del`) and the named netns
//! (`ip netns del`).

use std::io::{self, Read, Seek, SeekFrom, Write as IoWrite};
use std::net::Ipv4Addr;
use std::os::unix::io::AsRawFd;
use std::process::Command as SysCmd;
use std::sync::atomic::{AtomicU32, Ordering};

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

/// Monotonically increasing counter for generating unique netns/veth names.
static NS_COUNTER: AtomicU32 = AtomicU32::new(0);

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
    /// Name of the host-side veth interface (e.g. `vh-a1b2c3d4`).
    pub veth_host: String,
    /// Name of the named network namespace (e.g. `rem-12345-0`).
    pub ns_name: String,
    /// IP assigned to the container inside `remora0`'s subnet.
    pub container_ip: Ipv4Addr,
}

// ── Name generation ───────────────────────────────────────────────────────────

/// Generate a unique name for a container network namespace.
///
/// Format: `rem-{pid}-{counter}` — unique within a host (pid + monotonic counter).
/// The name is used both as the named netns identifier and as the basis for
/// deriving veth interface names.
pub fn generate_ns_name() -> String {
    let pid = unsafe { libc::getpid() };
    let n = NS_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("rem-{}-{}", pid, n)
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

/// Derive unique veth interface names from a namespace name via FNV-1a hash.
///
/// Interface names are limited to 15 bytes (IFNAMSIZ − 1).
/// `"vh-" + 8 hex digits` = 11 chars — safely within limit.
fn veth_names_for(ns_name: &str) -> (String, String) {
    let mut hash: u32 = 0x811c9dc5;
    for b in ns_name.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    (format!("vh-{:08x}", hash), format!("vp-{:08x}", hash))
}

/// Set up full bridge networking for a container using a named network namespace.
///
/// Called **from the parent process** **before** `fork()` / `spawn()`.
/// By the time the child's `pre_exec` runs, the netns is fully configured —
/// no race between the container and network setup.
///
/// ## What this does
///
/// 1. Ensures the `remora0` bridge exists (172.19.0.1/24) — idempotent.
/// 2. Allocates a container IP via file-locked IPAM.
/// 3. Creates a named netns: `ip netns add {ns_name}` → `/run/netns/{ns_name}`.
/// 4. Brings up loopback inside the named netns.
/// 5. Creates a `vh-{hash}` / `vp-{hash}` veth pair in the host netns.
/// 6. Moves `vp-{hash}` into the named netns and renames it `eth0`.
/// 7. Assigns the allocated IP and default route to `eth0`.
/// 8. Attaches `vh-{hash}` to `remora0` and brings it up.
///
/// The child's `pre_exec` then calls `setns(open("/run/netns/{ns_name}"), CLONE_NEWNET)`
/// to join the pre-configured namespace.
///
/// Returns a [`NetworkSetup`] that must be passed to [`teardown_network`]
/// after the container exits.
pub fn setup_bridge_network(ns_name: &str) -> io::Result<NetworkSetup> {
    ensure_bridge()?;

    let container_ip = allocate_ip()?;
    let (veth_host, veth_peer) = veth_names_for(ns_name);

    // 1. Create the named netns — this creates /run/netns/{ns_name}
    run("ip", &["netns", "add", ns_name])?;

    // 2. Bring up loopback inside the named netns (kernel assigns 127.0.0.1/8)
    run("ip", &["-n", ns_name, "link", "set", "lo", "up"])?;

    // 3. Create veth pair in the host netns
    run("ip", &[
        "link", "add", &veth_host,
        "type", "veth",
        "peer", "name", &veth_peer,
    ])?;

    // 4. Move the peer into the named netns
    run("ip", &["link", "set", &veth_peer, "netns", ns_name])?;

    let ip_cidr = format!("{}/24", container_ip);

    // 5. Configure eth0 inside the named netns (rename, assign IP, bring up, add route)
    run("ip", &["-n", ns_name, "link", "set", &veth_peer, "name", "eth0"])?;
    run("ip", &["-n", ns_name, "addr", "add", &ip_cidr, "dev", "eth0"])?;
    run("ip", &["-n", ns_name, "link", "set", "eth0", "up"])?;
    run("ip", &["-n", ns_name, "route", "add", "default", "via", BRIDGE_GW])?;

    // 6. Attach host-side veth to bridge and bring it up
    run("ip", &["link", "set", &veth_host, "master", BRIDGE_NAME])?;
    run("ip", &["link", "set", &veth_host, "up"])?;

    Ok(NetworkSetup { veth_host, ns_name: ns_name.to_string(), container_ip })
}

/// Remove the container's veth pair and named network namespace.
///
/// - Deleting the host-side veth cascades: the kernel removes it from the
///   bridge and destroys the container-side peer.
/// - Deleting the named netns unmounts `/run/netns/{ns_name}`.
///
/// Errors are non-fatal (logged via `log::warn!`).
pub fn teardown_network(setup: &NetworkSetup) {
    if let Err(e) = run("ip", &["link", "del", &setup.veth_host]) {
        log::warn!("network teardown veth (non-fatal): {}", e);
    }
    if let Err(e) = run("ip", &["netns", "del", &setup.ns_name]) {
        log::warn!("network teardown netns (non-fatal): {}", e);
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
