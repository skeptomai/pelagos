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
//! The primary reason is **debuggability**: named netns are visible via
//! `ip netns list` and inspectable with `ip netns exec pelagos-foo ip addr`
//! from the host. Anonymous namespaces via `/proc/{pid}/ns/net` offer none
//! of that visibility.
//!
//! There are also two practical problems with `/proc/{pid}/ns/net` given
//! our current use of `std::process::Command`, though neither is fundamental:
//!
//! 1. **Race with fast exit**: if the container runs e.g. `exit 0`, the
//!    child can terminate before the parent opens `/proc/{pid}/ns/net`.
//!    This isn't truly fatal — a dead container doesn't need networking —
//!    but it does require the parent to handle "PID gone" gracefully
//!    rather than treating it as an error.
//!
//! 2. **CLOEXEC deadlock**: adding a sync pipe so the child blocks in
//!    `pre_exec` while the parent configures networking deadlocks because
//!    `std::process::Command::spawn()` itself blocks on an internal
//!    CLOEXEC fail-pipe until `exec()`. The child can't `exec()` while
//!    blocked on our pipe, and the parent can't signal our pipe until
//!    `spawn()` returns. This is a Rust stdlib limitation — a raw
//!    `fork()`/`exec()` implementation could synchronize freely.
//!
//! Named netns sidestep both issues (created before fork, no coordination
//! needed) and give us host-side observability for free.
//!
//! Teardown removes the host-side veth (`ip link del`) and the named netns
//! (`ip netns del`).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write as IoWrite};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::os::unix::io::AsRawFd;
use std::process::Command as SysCmd;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ── Ipv4Net ──────────────────────────────────────────────────────────────────

/// A compact IPv4 network (address + prefix length), e.g. `10.88.1.0/24`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ipv4Net {
    pub addr: Ipv4Addr,
    pub prefix_len: u8,
}

impl Ipv4Net {
    /// Parse a CIDR string like `"10.88.1.0/24"`.
    pub fn from_cidr(s: &str) -> Result<Self, String> {
        let (addr_s, len_s) = s
            .split_once('/')
            .ok_or_else(|| format!("invalid CIDR '{}': expected ADDR/LEN", s))?;
        let addr: Ipv4Addr = addr_s
            .parse()
            .map_err(|e| format!("invalid IP in '{}': {}", s, e))?;
        let prefix_len: u8 = len_s
            .parse()
            .map_err(|e| format!("invalid prefix in '{}': {}", s, e))?;
        if prefix_len > 32 {
            return Err(format!("prefix length {} > 32", prefix_len));
        }
        Ok(Ipv4Net { addr, prefix_len })
    }

    fn mask(&self) -> u32 {
        if self.prefix_len == 0 {
            0
        } else {
            !0u32 << (32 - self.prefix_len)
        }
    }

    /// Network address (e.g. `10.88.1.0` for `10.88.1.5/24`).
    pub fn network(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.addr) & self.mask())
    }

    /// Broadcast address (e.g. `10.88.1.255` for `10.88.1.0/24`).
    pub fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.addr) | !self.mask())
    }

    /// Gateway — conventionally `.1` in the subnet.
    pub fn gateway(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network()) + 1)
    }

    /// First usable host IP (gateway + 1, i.e. `.2`).
    pub fn host_min(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network()) + 2)
    }

    /// Last usable host IP (broadcast - 1).
    pub fn host_max(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.broadcast()) - 1)
    }

    /// Whether this subnet contains the given IP.
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        (u32::from(ip) & self.mask()) == (u32::from(self.addr) & self.mask())
    }

    /// Whether two subnets overlap.
    pub fn overlaps(&self, other: &Ipv4Net) -> bool {
        // Two networks overlap iff either contains the other's network address.
        let smaller_prefix = self.prefix_len.min(other.prefix_len);
        let mask = if smaller_prefix == 0 {
            0
        } else {
            !0u32 << (32 - smaller_prefix)
        };
        (u32::from(self.addr) & mask) == (u32::from(other.addr) & mask)
    }

    /// CIDR string for the network (e.g. `"10.88.1.0/24"`).
    pub fn cidr_string(&self) -> String {
        format!("{}/{}", self.network(), self.prefix_len)
    }

    /// Gateway with prefix for `ip addr add` (e.g. `"10.88.1.1/24"`).
    pub fn gateway_cidr(&self) -> String {
        format!("{}/{}", self.gateway(), self.prefix_len)
    }
}

impl std::fmt::Display for Ipv4Net {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix_len)
    }
}

// ── NetworkDef ───────────────────────────────────────────────────────────────

/// Persistent definition of a named network (stored in config dir).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkDef {
    pub name: String,
    pub subnet: Ipv4Net,
    pub gateway: Ipv4Addr,
    pub bridge_name: String,
}

impl NetworkDef {
    /// Load a network definition from `<data>/networks/<name>/config.json`.
    pub fn load(name: &str) -> io::Result<Self> {
        let path = crate::paths::network_config_dir(name).join("config.json");
        let data = std::fs::read_to_string(&path).map_err(|e| {
            io::Error::other(format!(
                "network '{}' not found ({}): {}",
                name,
                path.display(),
                e
            ))
        })?;
        serde_json::from_str(&data).map_err(|e| io::Error::other(e.to_string()))
    }

    /// Save this network definition to `<data>/networks/<name>/config.json`.
    pub fn save(&self) -> io::Result<()> {
        let dir = crate::paths::network_config_dir(&self.name);
        std::fs::create_dir_all(&dir)?;
        let json =
            serde_json::to_string_pretty(self).map_err(|e| io::Error::other(e.to_string()))?;
        std::fs::write(dir.join("config.json"), json)
    }

    /// nftables table name for this network (e.g. `"pelagos-frontend"`).
    pub fn nft_table_name(&self) -> String {
        format!("pelagos-{}", self.name)
    }
}

/// Bootstrap or load the default `pelagos0` network definition.
///
/// If a config file exists on disk, loads it. Otherwise creates the default
/// definition (`172.19.0.0/24`, bridge `pelagos0`) and persists it.
/// Also migrates old global state files to per-network directories if they exist.
pub fn bootstrap_default_network() -> io::Result<NetworkDef> {
    let config_path = crate::paths::network_config_dir("pelagos0").join("config.json");
    if config_path.exists() {
        return NetworkDef::load("pelagos0");
    }

    let net = NetworkDef {
        name: "pelagos0".to_string(),
        subnet: Ipv4Net {
            addr: Ipv4Addr::new(172, 19, 0, 0),
            prefix_len: 24,
        },
        gateway: Ipv4Addr::new(172, 19, 0, 1),
        bridge_name: "pelagos0".to_string(),
    };
    net.save()?;

    // Migrate old global IPAM file to per-network dir if it exists.
    // Only next_ip is migrated — nat_refcount and port_forwards tracked
    // state for the old `ip pelagos` nft table which no longer exists
    // (now `ip pelagos-pelagos0`), so stale values would poison refcounts.
    let rt = crate::paths::runtime_dir();
    let net_rt = crate::paths::network_runtime_dir("pelagos0");
    let old_ipam = rt.join("next_ip");
    let new_ipam = net_rt.join("next_ip");
    if old_ipam.exists() && !new_ipam.exists() {
        std::fs::create_dir_all(&net_rt)?;
        if let Err(e) = std::fs::rename(&old_ipam, &new_ipam) {
            log::warn!(
                "migrate {} → {}: {}",
                old_ipam.display(),
                new_ipam.display(),
                e
            );
        }
    }
    // Remove stale old-format files (they reference the old nft table name).
    let _ = std::fs::remove_file(rt.join("nat_refcount"));
    let _ = std::fs::remove_file(rt.join("port_forwards"));

    Ok(net)
}

/// Load a network definition by name.
///
/// For `"pelagos0"`, calls [`bootstrap_default_network`] to ensure it exists.
/// For other names, loads from config dir.
pub fn load_network_def(name: &str) -> io::Result<NetworkDef> {
    if name == "pelagos0" {
        bootstrap_default_network()
    } else {
        NetworkDef::load(name)
    }
}

/// Ensure a named network exists, creating it if necessary.
///
/// If the network config already exists, returns immediately.  Otherwise tries
/// subnets in `10.99.0.0/24 … 10.99.255.0/24` until a non-overlapping one is
/// found, then creates and persists the `NetworkDef`.
///
/// Used by the Lisp runtime to auto-create networks referenced in service specs,
/// mirroring what `compose up` does for the declarative compose path.
pub fn ensure_network(name: &str) -> io::Result<()> {
    let config = crate::paths::network_config_dir(name).join("config.json");
    if config.exists() {
        return Ok(());
    }

    // Collect subnets already in use.
    let networks_dir = crate::paths::networks_config_dir();
    let mut used: Vec<Ipv4Net> = Vec::new();
    if networks_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&networks_dir) {
            for entry in entries.flatten() {
                let cfg = entry.path().join("config.json");
                if let Ok(data) = std::fs::read_to_string(&cfg) {
                    if let Ok(existing) = serde_json::from_str::<NetworkDef>(&data) {
                        used.push(existing.subnet);
                    }
                }
            }
        }
    }

    // Find the first non-overlapping /24 in 10.99.x.0.
    for octet in 0u8..=255 {
        let cidr = format!("10.99.{}.0/24", octet);
        let subnet = Ipv4Net::from_cidr(&cidr)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        if used.iter().any(|u| u.overlaps(&subnet)) {
            continue;
        }
        let bridge_name = if name == "pelagos0" {
            "pelagos0".to_string()
        } else {
            format!("rm-{}", name)
        };
        let net = NetworkDef {
            name: name.to_string(),
            gateway: subnet.gateway(),
            bridge_name,
            subnet,
        };
        net.save()?;
        log::info!("ensure_network: created '{}' ({})", name, cidr);
        return Ok(());
    }

    Err(io::Error::other(format!(
        "ensure_network: all subnets in 10.99.x.0/24 exhausted for '{}'",
        name
    )))
}

// Legacy constants — kept for reference but internal code now uses NetworkDef.
/// Bridge name for the default network.
pub const BRIDGE_NAME: &str = "pelagos0";
/// Gateway IP for the default network.
pub const BRIDGE_GW: &str = "172.19.0.1";

/// Monotonically increasing counter for generating unique netns/veth names.
static NS_COUNTER: AtomicU32 = AtomicU32::new(0);

// ── Public types ─────────────────────────────────────────────────────────────

/// Container network mode.
/// Protocol for a port-forward mapping.
///
/// Matches Docker's `-p HOST:CONTAINER/tcp|udp|both` syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortProto {
    /// TCP only (default).
    Tcp,
    /// UDP only.
    Udp,
    /// Both TCP and UDP.
    Both,
}

impl PortProto {
    fn as_str(self) -> &'static str {
        match self {
            PortProto::Tcp => "tcp",
            PortProto::Udp => "udp",
            PortProto::Both => "both",
        }
    }

    /// Parse a protocol string (`"tcp"`, `"udp"`, `"both"`). Defaults to `Tcp`.
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "udp" => PortProto::Udp,
            "both" => PortProto::Both,
            _ => PortProto::Tcp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkMode {
    /// Share the host's network stack (default — no changes).
    None,
    /// Isolated network namespace with loopback only.
    Loopback,
    /// Full connectivity via the default `pelagos0` bridge (172.19.0.x/24).
    ///
    /// Normalized to `BridgeNamed("pelagos0")` internally by `with_network()`.
    Bridge,
    /// Full connectivity via a named bridge network.
    ///
    /// The string is the network name (e.g. `"frontend"`). The corresponding
    /// `NetworkDef` is loaded at spawn time via [`load_network_def`].
    BridgeNamed(String),
    /// User-mode networking via `pasta` — rootless-compatible, full internet access.
    ///
    /// pasta creates a TAP interface inside the container's network namespace and
    /// relays packets to/from the host using ordinary userspace sockets, requiring
    /// no kernel privileges. Works for both root and rootless containers.
    Pasta,
}

impl NetworkMode {
    /// Returns `true` for any bridge-based mode (Bridge or BridgeNamed).
    pub fn is_bridge(&self) -> bool {
        matches!(self, NetworkMode::Bridge | NetworkMode::BridgeNamed(_))
    }

    /// Extract the network name for bridge modes. Returns `None` for non-bridge modes.
    pub fn bridge_network_name(&self) -> Option<&str> {
        match self {
            NetworkMode::Bridge => Some("pelagos0"),
            NetworkMode::BridgeNamed(name) => Some(name),
            _ => None,
        }
    }
}

/// Network configuration for a container.
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub mode: NetworkMode,
}

/// Runtime state from setting up bridge networking; needed for teardown.
pub struct NetworkSetup {
    /// Name of the host-side veth interface (e.g. `vh-a1b2c3d4`).
    pub veth_host: String,
    /// Name of the named network namespace (e.g. `rem-12345-0`).
    pub ns_name: String,
    /// IP assigned to the container inside the bridge subnet.
    pub container_ip: Ipv4Addr,
    /// Whether NAT (MASQUERADE) was enabled for this container.
    pub nat_enabled: bool,
    /// Port forwards configured for this container: `(host_port, container_port)`.
    pub port_forwards: Vec<(u16, u16, PortProto)>,
    /// Tokio multi-threaded runtime owning all async TCP proxy tasks.
    /// Dropped via `shutdown_background()` during teardown.
    proxy_tcp_runtime: Option<tokio::runtime::Runtime>,
    /// Stop flag for UDP proxy threads (std threads).
    proxy_udp_stop: Option<Arc<AtomicBool>>,
    /// Join handles for per-port UDP proxy threads; joined explicitly in teardown.
    proxy_udp_threads: Vec<std::thread::JoinHandle<()>>,
    /// Name of the network this setup belongs to (e.g. `"pelagos0"`, `"frontend"`).
    pub network_name: String,
}

impl std::fmt::Debug for NetworkSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkSetup")
            .field("veth_host", &self.veth_host)
            .field("ns_name", &self.ns_name)
            .field("container_ip", &self.container_ip)
            .field("nat_enabled", &self.nat_enabled)
            .field("port_forwards", &self.port_forwards)
            .field("proxy_tcp_active", &self.proxy_tcp_runtime.is_some())
            .field("proxy_udp_active", &self.proxy_udp_stop.is_some())
            .field("proxy_udp_threads", &self.proxy_udp_threads.len())
            .field("network_name", &self.network_name)
            .finish()
    }
}

/// Runtime state for a pasta-backed container; holds the pasta process for teardown.
///
/// `output_thread` drains pasta's stdout **and** stderr asynchronously so neither
/// pipe buffer fills and blocks pasta.  The collected output is logged at teardown
/// (or sooner when pasta exits unexpectedly before the TAP appears).
pub struct PastaSetup {
    process: std::process::Child,
    /// Background thread draining pasta's stdout+stderr pipes.  Joined in teardown.
    output_thread: Option<std::thread::JoinHandle<String>>,
    /// In root mode, the bind-mount path used for the netns file.
    /// Unmounted and removed during teardown.
    ns_bind_mount: Option<std::path::PathBuf>,
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

    let mut req = Ifreq {
        ifr_name: [0u8; 16],
        ifr_flags: 0,
        _pad: [0u8; 22],
    };
    req.ifr_name[0] = b'l';
    req.ifr_name[1] = b'o';

    unsafe {
        let sock = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if sock < 0 {
            return Err(io::Error::last_os_error());
        }

        // Get current flags
        let ret = libc::ioctl(sock, libc::SIOCGIFFLAGS as _, &mut req as *mut Ifreq);
        if ret < 0 {
            let e = io::Error::last_os_error();
            libc::close(sock);
            return Err(e);
        }

        // Set IFF_UP (bit 0)
        req.ifr_flags |= libc::IFF_UP as libc::c_short;

        let ret = libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &mut req as *mut Ifreq);
        libc::close(sock);

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

// ── N2: Bridge + veth ────────────────────────────────────────────────────────

/// Ensure the bridge for a network exists, has its IP, and is up.
///
/// Idempotent — safe to call for every container spawn.
fn ensure_bridge(net: &NetworkDef) -> io::Result<()> {
    // Create bridge (ignore error if it already exists)
    let _ = SysCmd::new("ip")
        .args(["link", "add", &net.bridge_name, "type", "bridge"])
        .stderr(std::process::Stdio::null())
        .status();

    // Assign gateway IP (ignore error if already assigned)
    let gw_cidr = net.subnet.gateway_cidr();
    let _ = SysCmd::new("ip")
        .args(["addr", "add", &gw_cidr, "dev", &net.bridge_name])
        .stderr(std::process::Stdio::null())
        .status();

    // Bring up (idempotent)
    run("ip", &["link", "set", &net.bridge_name, "up"])
}

/// Allocate the next IP from the network's subnet pool.
///
/// Uses `flock(LOCK_EX)` on the per-network IPAM file to serialize concurrent
/// spawns. Stores/reads full IP strings to support arbitrary subnets.
fn allocate_ip(net: &NetworkDef) -> io::Result<Ipv4Addr> {
    let ipam_path = crate::paths::network_ipam_file(&net.name);
    if let Some(parent) = ipam_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&ipam_path)?;

    // Exclusive lock — blocks until other spawns release their lock.
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_EX);
    }

    let mut content = String::new();
    file.read_to_string(&mut content)?;

    let host_min = u32::from(net.subnet.host_min());
    let host_max = u32::from(net.subnet.host_max());

    // Parse current IP (full string like "10.88.1.2"), default to host_min.
    let current: Ipv4Addr = content.trim().parse().unwrap_or(net.subnet.host_min());
    let current_u32 = u32::from(current);

    // Clamp to valid range.
    let ip_u32 = if current_u32 < host_min || current_u32 > host_max {
        host_min
    } else {
        current_u32
    };
    let ip = Ipv4Addr::from(ip_u32);

    // Advance, wrapping around within the subnet's host range.
    let next_u32 = ip_u32 + 1;
    let next = if next_u32 > host_max {
        Ipv4Addr::from(host_min)
    } else {
        Ipv4Addr::from(next_u32)
    };

    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    write!(file, "{}", next)?;
    // flock released when `file` is dropped here

    Ok(ip)
}

/// FNV-1a hash of a byte string, returning a u32.
fn fnv1a(input: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for &b in input {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

/// Derive unique veth interface names from a namespace name via FNV-1a hash.
///
/// Interface names are limited to 15 bytes (IFNAMSIZ − 1).
/// `"vh-" + 8 hex digits` = 11 chars — safely within limit.
fn veth_names_for(ns_name: &str) -> (String, String) {
    let hash = fnv1a(ns_name.as_bytes());
    (format!("vh-{:08x}", hash), format!("vp-{:08x}", hash))
}

/// Derive unique veth interface names for a secondary network attachment.
///
/// Hashes `"ns_name:network_name"` to avoid collisions with the primary veth
/// pair (which hashes just `ns_name`).
fn veth_names_for_network(ns_name: &str, network_name: &str) -> (String, String) {
    let input = format!("{}:{}", ns_name, network_name);
    let hash = fnv1a(input.as_bytes());
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
/// 1. Loads the [`NetworkDef`] for `network_name` and ensures its bridge exists.
/// 2. Allocates a container IP via file-locked per-network IPAM.
/// 3. Creates a named netns: `ip netns add {ns_name}` → `/run/netns/{ns_name}`.
/// 4. Brings up loopback inside the named netns.
/// 5. Creates a `vh-{hash}` / `vp-{hash}` veth pair in the host netns.
/// 6. Moves `vp-{hash}` into the named netns and renames it `eth0`.
/// 7. Assigns the allocated IP and default route to `eth0`.
/// 8. Attaches `vh-{hash}` to the network's bridge and brings it up.
/// 9. If `nat` is true, enables IP forwarding and installs an nftables MASQUERADE rule.
/// 10. If `port_forwards` is non-empty, installs nftables DNAT rules.
///
/// The child's `pre_exec` then calls `setns(open("/run/netns/{ns_name}"), CLONE_NEWNET)`
/// to join the pre-configured namespace.
///
/// Returns a [`NetworkSetup`] that must be passed to [`teardown_network`]
/// after the container exits.
pub fn setup_bridge_network(
    ns_name: &str,
    network_name: &str,
    nat: bool,
    port_forwards: Vec<(u16, u16, PortProto)>,
) -> io::Result<NetworkSetup> {
    let net_def = load_network_def(network_name)?;
    ensure_bridge(&net_def)?;

    let container_ip = allocate_ip(&net_def)?;
    let (veth_host, veth_peer) = veth_names_for(ns_name);

    // 1. Create the named netns — this creates /run/netns/{ns_name}
    run("ip", &["netns", "add", ns_name])?;

    // 2. Bring up loopback inside the named netns (kernel assigns 127.0.0.1/8)
    run("ip", &["-n", ns_name, "link", "set", "lo", "up"])?;

    // 3. Create veth pair in the host netns
    run(
        "ip",
        &[
            "link", "add", &veth_host, "type", "veth", "peer", "name", &veth_peer,
        ],
    )?;

    // 4. Move the peer into the named netns
    run("ip", &["link", "set", &veth_peer, "netns", ns_name])?;

    let ip_cidr = format!("{}/{}", container_ip, net_def.subnet.prefix_len);
    let gw_str = net_def.gateway.to_string();

    // 5. Configure eth0 inside the named netns (rename, assign IP, bring up, add route)
    run(
        "ip",
        &["-n", ns_name, "link", "set", &veth_peer, "name", "eth0"],
    )?;
    run(
        "ip",
        &["-n", ns_name, "addr", "add", &ip_cidr, "dev", "eth0"],
    )?;
    run("ip", &["-n", ns_name, "link", "set", "eth0", "up"])?;
    run(
        "ip",
        &["-n", ns_name, "route", "add", "default", "via", &gw_str],
    )?;

    // 6. Attach host-side veth to bridge and bring it up
    run(
        "ip",
        &["link", "set", &veth_host, "master", &net_def.bridge_name],
    )?;
    run("ip", &["link", "set", &veth_host, "up"])?;

    // 7. Optionally enable NAT (MASQUERADE) for internet access.
    if nat {
        enable_nat(ns_name, &net_def)?;
    }

    // 8. Optionally install port-forward (DNAT) rules.
    if !port_forwards.is_empty() {
        enable_port_forwards(ns_name, &net_def, container_ip, &port_forwards)?;
    }

    // 9. Start userspace port proxies (handles localhost traffic that nftables
    //    DNAT in PREROUTING cannot intercept).  TCP uses an async tokio runtime;
    //    UDP uses std threads unchanged.
    let (proxy_tcp_runtime, proxy_udp_stop, proxy_udp_threads) = if !port_forwards.is_empty() {
        start_port_proxies(container_ip, &port_forwards)
    } else {
        (None, None, Vec::new())
    };

    Ok(NetworkSetup {
        veth_host,
        ns_name: ns_name.to_string(),
        container_ip,
        nat_enabled: nat,
        port_forwards,
        proxy_tcp_runtime,
        proxy_udp_stop,
        proxy_udp_threads,
        network_name: network_name.to_string(),
    })
}

/// Remove the container's veth pair and named network namespace.
///
/// - Deleting the host-side veth cascades: the kernel removes it from the
///   bridge and destroys the container-side peer.
/// - Deleting the named netns unmounts `/run/netns/{ns_name}`.
/// - If NAT was enabled, decrements the refcount and removes the nftables
///   table when the last NAT container exits.
///
/// Errors are non-fatal (logged via `log::warn!`).
pub fn teardown_network(mut setup: NetworkSetup) {
    // Shut down TCP proxy runtime (cancels all accept loops and relay tasks).
    if let Some(rt) = setup.proxy_tcp_runtime.take() {
        rt.shutdown_background();
    }
    // Stop UDP proxy threads and join them.  Setting the flag first lets the
    // threads notice the stop signal during their next 100ms recv_from timeout.
    // Joining ensures the inbound socket is fully closed (port released) before
    // we proceed to delete the veth/netns.
    if let Some(ref stop) = setup.proxy_udp_stop {
        stop.store(true, Ordering::Relaxed);
    }
    for handle in setup.proxy_udp_threads.drain(..) {
        let _ = handle.join();
    }
    if let Err(e) = run("ip", &["link", "del", &setup.veth_host]) {
        log::warn!("network teardown veth (non-fatal): {}", e);
    }
    if let Err(e) = run("ip", &["netns", "del", &setup.ns_name]) {
        log::warn!("network teardown netns (non-fatal): {}", e);
    }
    let net_def = match load_network_def(&setup.network_name) {
        Ok(n) => n,
        Err(e) => {
            log::warn!(
                "network teardown: cannot load network '{}': {}",
                setup.network_name,
                e
            );
            return;
        }
    };
    if !setup.port_forwards.is_empty() {
        disable_port_forwards(&setup.ns_name, &net_def);
    }
    if setup.nat_enabled {
        disable_nat(&setup.ns_name, &net_def);
    }
}

// ── Secondary network attachment ──────────────────────────────────────────────

/// Attach an additional bridge network to an existing named netns.
///
/// Unlike [`setup_bridge_network`] which creates the netns and assigns `eth0`
/// with a default route, this adds a secondary interface (e.g. `eth1`, `eth2`)
/// with only a subnet route — no default route.
///
/// Called **from the parent process** after the primary network is set up.
/// The child joins the netns via the primary network's `setns()`.
///
/// Returns a [`NetworkSetup`] for teardown. The `ns_name` field is set to the
/// same namespace as the primary — [`teardown_secondary_network`] will NOT
/// delete the netns (the primary owns it).
pub fn attach_network_to_netns(
    ns_name: &str,
    network_name: &str,
    iface_name: &str,
) -> io::Result<NetworkSetup> {
    let net_def = load_network_def(network_name)?;
    ensure_bridge(&net_def)?;

    let container_ip = allocate_ip(&net_def)?;
    let (veth_host, veth_peer) = veth_names_for_network(ns_name, network_name);

    // 1. Create veth pair in host netns
    run(
        "ip",
        &[
            "link", "add", &veth_host, "type", "veth", "peer", "name", &veth_peer,
        ],
    )?;

    // 2. Move the peer into the existing named netns
    run("ip", &["link", "set", &veth_peer, "netns", ns_name])?;

    let ip_cidr = format!("{}/{}", container_ip, net_def.subnet.prefix_len);

    // 3. Configure the interface inside the netns (rename, assign IP, bring up)
    run(
        "ip",
        &["-n", ns_name, "link", "set", &veth_peer, "name", iface_name],
    )?;
    run(
        "ip",
        &["-n", ns_name, "addr", "add", &ip_cidr, "dev", iface_name],
    )?;
    run("ip", &["-n", ns_name, "link", "set", iface_name, "up"])?;

    // The kernel automatically creates the subnet route when the interface
    // comes up with an IP in CIDR notation (e.g. 10.99.2.2/24 → route for
    // 10.99.2.0/24 dev eth1). No explicit `route add` needed — and attempting
    // one would fail with EEXIST.

    // 4. Attach host-side veth to bridge and bring it up
    run(
        "ip",
        &["link", "set", &veth_host, "master", &net_def.bridge_name],
    )?;
    run("ip", &["link", "set", &veth_host, "up"])?;

    Ok(NetworkSetup {
        veth_host,
        ns_name: ns_name.to_string(),
        container_ip,
        nat_enabled: false,
        port_forwards: Vec::new(),
        proxy_tcp_runtime: None,
        proxy_udp_stop: None,
        proxy_udp_threads: Vec::new(),
        network_name: network_name.to_string(),
    })
}

/// Remove a secondary network's veth pair.
///
/// Unlike [`teardown_network`], this does NOT delete the named netns (the
/// primary network owns it). Only the host-side veth is deleted — the kernel
/// cascades removal of the container-side peer.
pub fn teardown_secondary_network(setup: &NetworkSetup) {
    if let Err(e) = run("ip", &["link", "del", &setup.veth_host]) {
        log::warn!("secondary network teardown veth (non-fatal): {}", e);
    }
}

// ── N3: NAT / MASQUERADE ─────────────────────────────────────────────────────

/// Build the nftables script that installs MASQUERADE + FORWARD rules for a network.
///
/// Uses `add` so the commands are idempotent if the table already exists
/// (e.g. if a previous run crashed with the refcount > 0).
///
/// The forward chain is required because the host's default FORWARD policy may
/// be DROP (common on systems with a firewall). Without it, ICMP (ping) may
/// work but TCP/UDP traffic is silently dropped.
fn build_nat_script(net: &NetworkDef) -> String {
    let table = net.nft_table_name();
    let cidr = net.subnet.cidr_string();
    let bridge = &net.bridge_name;
    format!(
        "add table ip {table}\n\
         add chain ip {table} postrouting {{ type nat hook postrouting priority 100; }}\n\
         add rule ip {table} postrouting ip saddr {cidr} oifname != \"{bridge}\" masquerade\n\
         add chain ip {table} forward {{ type filter hook forward priority 0; }}\n\
         add rule ip {table} forward ip saddr {cidr} accept\n\
         add rule ip {table} forward ip daddr {cidr} accept\n"
    )
}

/// Pipe an nft script to `nft -f -`, returning an error on non-zero exit.
fn run_nft(script: &str) -> io::Result<()> {
    use std::io::Write as IoWriteLocal;
    use std::process::Stdio as ProcStdio;

    let mut child = SysCmd::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(ProcStdio::piped())
        .stdout(ProcStdio::null())
        .stderr(ProcStdio::inherit())
        .spawn()?;

    child.stdin.as_mut().unwrap().write_all(script.as_bytes())?;
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("nft -f - exited with {}", status)))
    }
}

/// Like [`run_nft`] but suppresses stderr (for best-effort / migration commands).
fn run_nft_quiet(script: &str) -> io::Result<()> {
    use std::io::Write as IoWriteLocal;
    use std::process::Stdio as ProcStdio;

    let mut child = SysCmd::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(ProcStdio::piped())
        .stdout(ProcStdio::null())
        .stderr(ProcStdio::null())
        .spawn()?;

    child.stdin.as_mut().unwrap().write_all(script.as_bytes())?;
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("nft -f - exited with {}", status)))
    }
}

/// Increment the NAT refcount; install nftables rules when going 0 → 1.
///
/// Uses `flock(LOCK_EX)` on the per-network NAT refcount file to serialise
/// concurrent spawns. IP forwarding is written to `/proc/sys/net/ipv4/ip_forward`
/// once (never disabled on teardown — other software may rely on it).
/// Returns `true` if the named network namespace still exists on the host.
///
/// Used to detect stale entries in the NAT active-set file left by containers
/// that exited without calling `disable_nat()` (e.g. due to a process crash).
fn netns_exists(ns_name: &str) -> bool {
    std::path::Path::new(&format!("/run/netns/{}", ns_name)).exists()
}

/// Increment the NAT active set; install nftables rules when the set goes from empty → 1.
///
/// The active-set file (`nat_refcount`) now stores **one netns name per line** rather than
/// a plain integer. On each call stale entries (whose `/run/netns/{name}` no longer exists)
/// are filtered out, making the mechanism crash-safe: a container that died without calling
/// `disable_nat()` is automatically evicted the next time any container calls `enable_nat()`.
///
/// `flock(LOCK_EX)` serialises concurrent spawns.
fn enable_nat(ns_name: &str, net: &NetworkDef) -> io::Result<()> {
    let active_path = crate::paths::network_nat_refcount_file(&net.name);
    if let Some(parent) = active_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&active_path)?;

    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };

    let mut content = String::new();
    file.read_to_string(&mut content)?;

    // Filter to live entries only (crash-safe eviction of stale ns names).
    let mut active: Vec<String> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|name| netns_exists(name))
        .map(|s| s.to_string())
        .collect();

    if active.is_empty() {
        // Enable IP forwarding.
        std::fs::write("/proc/sys/net/ipv4/ip_forward", b"1\n")?;

        // Migration: if this is the default network, remove the old global
        // `ip pelagos` table that previous versions created.
        if net.name == "pelagos0" {
            let _ = run_nft_quiet("delete table ip pelagos\n");
        }

        // Install the nftables MASQUERADE rule set.
        let script = build_nat_script(net);
        run_nft(&script)?;

        // Also insert iptables FORWARD rules for compatibility with hosts
        // running UFW, Docker, or other iptables-based firewalls that set
        // the FORWARD chain policy to DROP.
        //
        // Purge any stale duplicates first (from previous crashes where the
        // active set was lost but the kernel rules survived).
        let cidr = net.subnet.cidr_string();
        while run_quiet("iptables", &["-D", "FORWARD", "-s", &cidr, "-j", "ACCEPT"]).is_ok() {}
        while run_quiet("iptables", &["-D", "FORWARD", "-d", &cidr, "-j", "ACCEPT"]).is_ok() {}
        let _ = run("iptables", &["-I", "FORWARD", "-s", &cidr, "-j", "ACCEPT"]);
        let _ = run("iptables", &["-I", "FORWARD", "-d", &cidr, "-j", "ACCEPT"]);
    }

    active.push(ns_name.to_string());
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    write!(file, "{}", active.join("\n"))?;
    // flock released when `file` is dropped.
    Ok(())
}

/// Decrement the NAT refcount; remove or trim the nftables table when reaching zero.
///
/// If port-forward rules are still active when NAT reaches zero, the table is
/// kept but the postrouting chain is flushed (MASQUERADE removed).
/// The table is only fully deleted when both NAT refcount and port-forward list
/// are empty.
///
/// Errors are non-fatal (logged via `log::warn!`).
/// Decrement the NAT active set; remove or trim the nftables table when the set becomes empty.
///
/// Removes `ns_name` from the active-set file and also evicts any stale entries whose
/// `/run/netns/{name}` no longer exists (crash-safe). If the set is then empty, tears
/// down iptables rules and the nftables table (unless port-forwards are still active).
///
/// Errors are non-fatal (logged via `log::warn!`).
fn disable_nat(ns_name: &str, net: &NetworkDef) {
    let active_path = crate::paths::network_nat_refcount_file(&net.name);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&active_path);

    let mut file = match file {
        Ok(f) => f,
        Err(e) => {
            log::warn!("NAT active-set open (non-fatal): {}", e);
            return;
        }
    };

    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };

    let mut content = String::new();
    if let Err(e) = file.read_to_string(&mut content) {
        log::warn!("NAT active-set read (non-fatal): {}", e);
        return;
    }

    // Remove this container explicitly; also evict any other stale entries.
    // Note: by the time disable_nat() is called, ip netns del has already
    // removed /run/netns/{ns_name}, so we must remove by name rather than
    // relying on the liveness filter for this container's own entry.
    let remaining: Vec<String> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|name| *name != ns_name) // remove this container explicitly
        .filter(|name| netns_exists(name)) // evict other stale entries
        .map(|s| s.to_string())
        .collect();

    let table = net.nft_table_name();
    let cidr = net.subnet.cidr_string();

    if let Err(e) = file
        .seek(SeekFrom::Start(0))
        .and_then(|_| file.set_len(0))
        .and_then(|_| {
            write!(file, "{}", remaining.join("\n"))?;
            Ok(())
        })
    {
        log::warn!("NAT active-set write (non-fatal): {}", e);
    }
    drop(file);

    if remaining.is_empty() {
        // Remove the iptables FORWARD rules added by enable_nat().
        let _ = run("iptables", &["-D", "FORWARD", "-s", &cidr, "-j", "ACCEPT"]);
        let _ = run("iptables", &["-D", "FORWARD", "-d", &cidr, "-j", "ACCEPT"]);

        if read_port_forwards_count(&net.name) == 0 {
            // No active port forwards either — remove the entire table.
            // Use run_nft_quiet: deletion is non-fatal and the table may have
            // already been removed by a concurrent disable_port_forwards().
            if let Err(e) = run_nft_quiet(&format!("delete table ip {}\n", table)) {
                log::warn!("nft delete table {} (non-fatal): {}", table, e);
            }
        } else {
            // Port forwards still active — remove MASQUERADE but keep the table.
            let _ = run_nft(&format!("flush chain ip {} postrouting\n", table));
        }
    }
}

// ── N4: Port mapping (DNAT) ───────────────────────────────────────────────────

/// A port-forward state file entry: `(ns_name, container_ip, host_port, container_port, proto)`.
type PortForwardEntry = (String, Ipv4Addr, u16, u16, PortProto);

/// Parse one line from the port-forwards state file.
///
/// Format: `{ip}:{host_port}:{container_port}[:{proto}]`
/// The proto field defaults to `tcp` when absent (backwards compat).
/// Parse one line from the port-forwards state file.
///
/// New format (since crash-safe eviction was added):
///   `ns_name:ip:host_port:container_port:proto`   (5 colon-separated fields)
///
/// Old format (backward-compat; treated as if ns_name = ""):
///   `ip:host_port:container_port[:proto]`          (3-4 fields; first parses as IPv4)
///
/// Returns `(ns_name, ip, host_port, container_port, proto)`.
/// Old-format entries have an empty `ns_name` and are treated as stale by liveness checks.
fn parse_port_forward_line(line: &str) -> Option<(String, Ipv4Addr, u16, u16, PortProto)> {
    // Detect format: if the first colon-delimited token is a valid IPv4 address, it's
    // the old format.  Otherwise the first token is a netns name.
    let first_colon = line.find(':')?;
    let first = &line[..first_colon];
    if let Ok(ip) = first.parse::<Ipv4Addr>() {
        // Old format: ip:hp:cp[:proto]
        let mut parts = line.splitn(4, ':');
        let _ip_str = parts.next()?; // already parsed above
        let host_port: u16 = parts.next()?.parse().ok()?;
        let container_port: u16 = parts.next()?.parse().ok()?;
        let proto = parts.next().map(PortProto::parse).unwrap_or(PortProto::Tcp);
        Some((String::new(), ip, host_port, container_port, proto))
    } else {
        // New format: ns_name:ip:hp:cp:proto
        let mut parts = line.splitn(5, ':');
        let ns_name = parts.next()?.to_string();
        let ip: Ipv4Addr = parts.next()?.parse().ok()?;
        let host_port: u16 = parts.next()?.parse().ok()?;
        let container_port: u16 = parts.next()?.parse().ok()?;
        let proto = parts.next().map(PortProto::parse).unwrap_or(PortProto::Tcp);
        Some((ns_name, ip, host_port, container_port, proto))
    }
}

/// Read all port-forward entries from an already-flocked file.
fn read_port_forwards_locked(file: &mut std::fs::File) -> io::Result<Vec<PortForwardEntry>> {
    file.seek(SeekFrom::Start(0))?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    let entries = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(parse_port_forward_line)
        .collect();
    Ok(entries)
}

/// Count *live* port-forward entries (reads without locking — for teardown checks only).
///
/// Entries from crashed containers are evicted the same way as the NAT refcount:
/// by checking whether `/run/netns/{ns_name}` still exists.  Old-format entries
/// (empty ns_name, written before crash-safe eviction was added) are treated as
/// stale and not counted.
fn read_port_forwards_count(network_name: &str) -> usize {
    let content =
        match std::fs::read_to_string(crate::paths::network_port_forwards_file(network_name)) {
            Ok(c) => c,
            Err(_) => return 0,
        };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(parse_port_forward_line)
        .filter(|(ns_name, _, _, _, _)| !ns_name.is_empty() && netns_exists(ns_name))
        .count()
}

/// Count live NAT active-set entries without locking (for teardown checks only).
///
/// Reads the ns-name list file and counts entries whose `/run/netns/{name}` still exists.
fn read_nat_refcount(network_name: &str) -> u32 {
    let content =
        match std::fs::read_to_string(crate::paths::network_nat_refcount_file(network_name)) {
            Ok(c) => c,
            Err(_) => return 0,
        };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|name| netns_exists(name))
        .count() as u32
}

/// Build the nftables script that (re)installs all current DNAT rules.
///
/// Uses `add table` / `add chain` (idempotent) so it is safe to call even
/// when the table already exists (e.g. because NAT/MASQUERADE is active).
/// `flush chain prerouting` wipes the old rules before rewriting — this is
/// the flush-and-rebuild strategy that avoids needing to track rule handles.
fn build_prerouting_script(
    net: &NetworkDef,
    entries: &[(Ipv4Addr, u16, u16, PortProto)],
) -> String {
    let table = net.nft_table_name();
    let mut s = format!(
        "add table ip {table}\n\
         add chain ip {table} prerouting {{ type nat hook prerouting priority -100; }}\n\
         flush chain ip {table} prerouting\n",
    );
    for (ip, host_port, container_port, proto) in entries {
        if matches!(proto, PortProto::Tcp | PortProto::Both) {
            s.push_str(&format!(
                "add rule ip {} prerouting tcp dport {} dnat to {}:{}\n",
                table, host_port, ip, container_port
            ));
        }
        if matches!(proto, PortProto::Udp | PortProto::Both) {
            s.push_str(&format!(
                "add rule ip {} prerouting udp dport {} dnat to {}:{}\n",
                table, host_port, ip, container_port
            ));
        }
    }
    s
}

/// Add port-forward entries to the state file and install nftables DNAT rules.
///
/// Uses `flock(LOCK_EX)` on the per-network port-forwards file to serialise
/// concurrent spawns. The network's nft table / prerouting chain are created
/// idempotently, so this is safe whether NAT is enabled or not.
///
/// Stale entries (whose `/run/netns/{ns_name}` no longer exists) are evicted on
/// each call — the same crash-safe strategy as `enable_nat`.
fn enable_port_forwards(
    ns_name: &str,
    net: &NetworkDef,
    container_ip: Ipv4Addr,
    forwards: &[(u16, u16, PortProto)],
) -> io::Result<()> {
    let pf_path = crate::paths::network_port_forwards_file(&net.name);
    if let Some(parent) = pf_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&pf_path)?;

    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };

    // Load existing entries and evict stale ones (crashed containers).
    let existing = read_port_forwards_locked(&mut file)?;
    let mut live: Vec<PortForwardEntry> = existing
        .into_iter()
        .filter(|(n, _, _, _, _)| !n.is_empty() && netns_exists(n))
        .collect();

    for &(host_port, container_port, proto) in forwards {
        live.push((
            ns_name.to_string(),
            container_ip,
            host_port,
            container_port,
            proto,
        ));
    }

    // Overwrite file with live + new entries in new format.
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    for (ns, ip, hp, cp, proto) in &live {
        writeln!(file, "{}:{}:{}:{}:{}", ns, ip, hp, cp, proto.as_str())?;
    }

    // Install nftables DNAT rules (build_prerouting_script needs ip/hp/cp/proto only).
    let nft_entries: Vec<(Ipv4Addr, u16, u16, PortProto)> = live
        .iter()
        .map(|(_, ip, hp, cp, proto)| (*ip, *hp, *cp, *proto))
        .collect();
    let script = build_prerouting_script(net, &nft_entries);
    run_nft(&script)?;

    // flock released when `file` is dropped.
    Ok(())
}

/// Remove a container's port-forward entries and update nftables accordingly.
///
/// - If no entries remain AND NAT is also inactive, deletes the entire table.
/// - If no entries remain BUT NAT is still active, flushes only the prerouting chain.
/// - If entries remain, rebuilds the prerouting chain from the survivors.
///
/// Also evicts stale entries from crashed containers (same crash-safe strategy as
/// `disable_nat`).
///
/// Errors are non-fatal (logged via `log::warn!`).
fn disable_port_forwards(ns_name: &str, net: &NetworkDef) {
    let pf_path = crate::paths::network_port_forwards_file(&net.name);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&pf_path);

    let mut file = match file {
        Ok(f) => f,
        Err(e) => {
            log::warn!("port forwards file open (non-fatal): {}", e);
            return;
        }
    };

    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };

    let entries = match read_port_forwards_locked(&mut file) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("port forwards read (non-fatal): {}", e);
            return;
        }
    };

    // Remove this container's entries (by ns_name) and evict other stale entries.
    // ns_name-based matching is simpler and more correct than IP+port matching.
    // Old-format entries (empty ns_name) are treated as stale and evicted.
    let remaining: Vec<PortForwardEntry> = entries
        .into_iter()
        .filter(|(n, _, _, _, _)| !n.is_empty() && n != ns_name && netns_exists(n))
        .collect();

    // Write remaining entries back in new format.
    if let Err(e) = file.seek(SeekFrom::Start(0)).and_then(|_| file.set_len(0)) {
        log::warn!("port forwards file truncate (non-fatal): {}", e);
        return;
    }
    for (ns, ip, hp, cp, proto) in &remaining {
        let _ = writeln!(file, "{}:{}:{}:{}:{}", ns, ip, hp, cp, proto.as_str());
    }

    // flock released; now update nftables.
    drop(file);

    // Build nft entries without ns_name.
    let nft_remaining: Vec<(Ipv4Addr, u16, u16, PortProto)> = remaining
        .iter()
        .map(|(_, ip, hp, cp, proto)| (*ip, *hp, *cp, *proto))
        .collect();

    let table = net.nft_table_name();
    if nft_remaining.is_empty() {
        // No more port forwards — check if NAT is also gone.
        if read_nat_refcount(&net.name) == 0 {
            // Nothing using the table — remove it entirely.
            // Use run_nft_quiet: deletion is non-fatal and the table may have
            // already been removed by a concurrent disable_nat().
            if let Err(e) = run_nft_quiet(&format!("delete table ip {}\n", table)) {
                log::warn!("nft delete table {} (non-fatal): {}", table, e);
            }
        } else {
            // NAT still active — flush prerouting chain only.
            let _ = run_nft(&format!("flush chain ip {} prerouting\n", table));
        }
    } else {
        // Rebuild prerouting chain from the surviving entries.
        let script = build_prerouting_script(net, &nft_remaining);
        if let Err(e) = run_nft(&script) {
            log::warn!("nft rebuild prerouting (non-fatal): {}", e);
        }
    }
}

// ── Userspace port proxy ──────────────────────────────────────────────────────

/// Start port proxies for each port mapping.
///
/// nftables DNAT rules in the PREROUTING chain handle traffic from external
/// hosts, but traffic originating from localhost bypasses PREROUTING entirely
/// (it goes through OUTPUT). Docker solves this with `docker-proxy` — a
/// userspace relay. This is Pelagos's equivalent for both TCP and UDP.
///
/// TCP uses a tokio multi-threaded runtime: all accept loops and relay tasks
/// run as async tasks distributed across `min(available_parallelism, 4)` worker
/// threads.  O(1) threads relative to connection count.
///
/// UDP still uses std threads: one thread per port plus one thread per active
/// client session (30-second idle eviction).
///
/// Returns `(tcp_runtime, udp_stop, udp_threads)`. The caller stores all three in
/// `NetworkSetup`; teardown calls `rt.shutdown_background()` for TCP, sets the
/// stop flag for UDP, and joins the per-port UDP threads.
fn start_port_proxies(
    container_ip: Ipv4Addr,
    forwards: &[(u16, u16, PortProto)],
) -> (
    Option<tokio::runtime::Runtime>,
    Option<Arc<AtomicBool>>,
    Vec<std::thread::JoinHandle<()>>,
) {
    let tcp_forwards: Vec<(u16, u16)> = forwards
        .iter()
        .filter(|(_, _, p)| matches!(p, PortProto::Tcp | PortProto::Both))
        .map(|&(h, c, _)| (h, c))
        .collect();

    let udp_forwards: Vec<(u16, u16)> = forwards
        .iter()
        .filter(|(_, _, p)| matches!(p, PortProto::Udp | PortProto::Both))
        .map(|&(h, c, _)| (h, c))
        .collect();

    let tcp_runtime = if !tcp_forwards.is_empty() {
        Some(start_tcp_proxies_async(container_ip, &tcp_forwards))
    } else {
        None
    };

    let (udp_stop, udp_threads) = if !udp_forwards.is_empty() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for (host_port, container_port) in udp_forwards {
            let stop_clone = Arc::clone(&stop);
            handles.push(std::thread::spawn(move || {
                start_udp_proxy(host_port, container_ip, container_port, stop_clone);
            }));
        }
        (Some(stop), handles)
    } else {
        (None, Vec::new())
    };

    (tcp_runtime, udp_stop, udp_threads)
}

/// Build a tokio multi-threaded runtime and spawn one async accept-loop task
/// per TCP port mapping.  Worker threads: `min(available_parallelism, 4)`.
///
/// The caller owns the returned `Runtime`.  Dropping it (or calling
/// `shutdown_background`) cancels all accept loops and in-flight relay tasks
/// and terminates the worker threads.
fn start_tcp_proxies_async(
    container_ip: Ipv4Addr,
    tcp_forwards: &[(u16, u16)],
) -> tokio::runtime::Runtime {
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(4);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_io()
        .enable_time()
        .thread_name("pelagos-tcp-proxy")
        .build()
        .expect("tokio tcp proxy runtime");

    for &(host_port, container_port) in tcp_forwards {
        let target = SocketAddr::from((container_ip, container_port));
        rt.spawn(tcp_accept_loop(host_port, target));
    }

    rt
}

/// Async accept loop for a single TCP port mapping.
///
/// Binds a `TcpListener` on `0.0.0.0:{host_port}` and spawns a `tcp_relay`
/// task for each accepted connection.  Runs until the runtime is dropped.
async fn tcp_accept_loop(host_port: u16, target: SocketAddr) {
    let listener =
        match tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], host_port))).await {
            Ok(l) => l,
            Err(e) => {
                log::warn!(
                    "tcp proxy: cannot bind 0.0.0.0:{}: {} (nftables DNAT still active)",
                    host_port,
                    e
                );
                return;
            }
        };

    log::debug!("tcp proxy: 0.0.0.0:{} -> {}", host_port, target);

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                tokio::spawn(tcp_relay(stream, target));
            }
            Err(e) => {
                log::warn!("tcp proxy accept error on port {}: {}", host_port, e);
                break;
            }
        }
    }
}

/// Async bidirectional TCP relay between a client socket and a container target.
///
/// Connects to `target` with a 5-second timeout, then runs
/// `copy_bidirectional` until either side closes or the runtime is dropped.
async fn tcp_relay(mut client: tokio::net::TcpStream, target: SocketAddr) {
    let mut upstream = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(target),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            log::debug!("tcp proxy: cannot connect to {}: {}", target, e);
            return;
        }
        Err(_) => {
            log::debug!("tcp proxy: connect to {} timed out", target);
            return;
        }
    };

    if let Err(e) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
        log::debug!("tcp proxy relay error: {}", e);
    }
}

/// UDP relay for a single port mapping.
///
/// Binds a listening socket on `0.0.0.0:{host_port}`. For each unique client
/// address, a dedicated outbound socket is created and connected to
/// `{container_ip}:{container_port}`. Replies from the container are forwarded
/// back to the originating client. Sessions idle for more than 30 seconds are
/// evicted from the session table.
fn start_udp_proxy(
    host_port: u16,
    container_ip: Ipv4Addr,
    container_port: u16,
    stop: Arc<AtomicBool>,
) {
    let inbound = match UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], host_port))) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            log::warn!(
                "udp proxy: cannot bind 0.0.0.0:{}: {} (nftables DNAT still active)",
                host_port,
                e
            );
            return;
        }
    };
    inbound
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("set_read_timeout");

    let target = SocketAddr::from((container_ip, container_port));
    log::debug!("udp proxy: 0.0.0.0:{} -> {}", host_port, target);

    // session_map: client_addr -> (outbound_socket, last_activity)
    type SessionMap = HashMap<SocketAddr, (Arc<UdpSocket>, Instant)>;
    let sessions: Arc<Mutex<SessionMap>> = Arc::new(Mutex::new(HashMap::new()));

    let mut buf = [0u8; 65535];
    // Collect reply-thread handles so we can join them when the proxy stops.
    let mut reply_handles: Vec<std::thread::JoinHandle<()>> = Vec::new();

    while !stop.load(Ordering::Relaxed) {
        match inbound.recv_from(&mut buf) {
            Ok((n, client_addr)) => {
                let data = buf[..n].to_vec();

                // Return both the outbound socket and an optional new reply handle.
                let (outbound, spawned) = {
                    let mut map = sessions.lock().unwrap();
                    // Evict sessions idle > 30 seconds.
                    map.retain(|_, (_, last)| last.elapsed() < Duration::from_secs(30));

                    if let Some((sock, last)) = map.get_mut(&client_addr) {
                        *last = Instant::now();
                        (Arc::clone(sock), None)
                    } else {
                        // New client: create a dedicated outbound socket.
                        let sock = match UdpSocket::bind("0.0.0.0:0") {
                            Ok(s) => Arc::new(s),
                            Err(e) => {
                                log::warn!("udp proxy: outbound bind failed: {}", e);
                                continue;
                            }
                        };
                        if let Err(e) = sock.connect(target) {
                            log::warn!("udp proxy: connect to {} failed: {}", target, e);
                            continue;
                        }
                        map.insert(client_addr, (Arc::clone(&sock), Instant::now()));

                        // Spawn reply-forwarding thread for this session.
                        let reply_sock = Arc::clone(&sock);
                        let inbound_ref = Arc::clone(&inbound);
                        let stop2 = Arc::clone(&stop);
                        let handle = std::thread::spawn(move || {
                            let mut rbuf = [0u8; 65535];
                            reply_sock
                                .set_read_timeout(Some(Duration::from_millis(100)))
                                .ok();
                            while !stop2.load(Ordering::Relaxed) {
                                match reply_sock.recv(&mut rbuf) {
                                    Ok(m) => {
                                        let _ = inbound_ref.send_to(&rbuf[..m], client_addr);
                                    }
                                    Err(ref e)
                                        if e.kind() == io::ErrorKind::WouldBlock
                                            || e.kind() == io::ErrorKind::TimedOut => {}
                                    Err(_) => break,
                                }
                            }
                        });

                        (sock, Some(handle))
                    }
                };

                if let Some(h) = spawned {
                    reply_handles.push(h);
                    // Prune already-finished handles to bound memory use.
                    reply_handles.retain(|h| !h.is_finished());
                }

                if let Err(e) = outbound.send(&data) {
                    log::debug!("udp proxy: forward to {} failed: {}", target, e);
                }
            }
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
            }
            Err(e) => {
                if !stop.load(Ordering::Relaxed) {
                    log::warn!("udp proxy recv_from error on port {}: {}", host_port, e);
                }
                break;
            }
        }
    }

    // Join all remaining reply threads.  They observe the same stop flag and
    // exit within one read timeout (100 ms) of it being set.
    for handle in reply_handles {
        let _ = handle.join();
    }
}

// ── N5: pasta user-mode networking ───────────────────────────────────────────

/// Spawn pasta attached to an already-running container's network namespace.
///
/// Called in the *parent*, immediately after `spawn()` returns (child has exec'd).
/// pasta receives the container's netns via `/proc/{child_pid}/ns/net`.
///
/// pasta runs as a background process; call [`teardown_pasta_network`] after
/// the container exits to kill it and reap the process.
pub fn setup_pasta_network(
    child_pid: u32,
    port_forwards: &[(u16, u16, PortProto)],
) -> io::Result<PastaSetup> {
    let mut args: Vec<String> = vec![];

    // Invocation form depends on whether we are running as root or not.
    //
    // ROOT mode — use `pasta --netns /proc/{pid}/ns/net --runas 0 [OPTIONS]`:
    //   When pasta receives a PID it always opens /proc/{pid}/ns/user to enter the
    //   container's user namespace before entering the network namespace.  On kernels
    //   that restrict user namespace operations (e.g. Alpine linux-lts with
    //   CONFIG_USER_NS restrictions, or sysctl kernel.unprivileged_userns_clone=0),
    //   this open fails with EPERM and pasta exits with status 1 before creating the
    //   TAP interface.
    //
    //   The `--netns PATH` form avoids the user namespace entirely: because no
    //   `--userns` is given, pasta uses `--netns-only` semantics and joins the
    //   network namespace directly without touching the user namespace file.
    //   `--runas 0` keeps pasta running as root so no privilege-drop dance occurs.
    //
    // ROOTLESS mode — use `pasta [OPTIONS] PID`:
    //   The container runs in a user namespace created by pelagos.  pasta must enter
    //   that user namespace (via the PID form) before it can enter the owned network
    //   namespace.  The --netns form would require a separate --userns argument and
    //   is unnecessarily complex; the PID form handles this automatically.
    // SAFETY: geteuid() is always safe to call.
    let running_as_root = unsafe { libc::geteuid() } == 0;

    for &(host, container, proto) in port_forwards {
        if matches!(proto, PortProto::Tcp | PortProto::Both) {
            args.push("-t".to_string());
            args.push(format!("{}:{}", host, container));
        }
        if matches!(proto, PortProto::Udp | PortProto::Both) {
            args.push("-u".to_string());
            args.push(format!("{}:{}", host, container));
        }
    }
    // Tell pasta to configure IP address and routes inside the container's netns.
    // Without this flag pasta only creates the TAP; the container would need to run
    // a DHCP client (udhcpc) before the interface has an IP or default route.
    args.push("--config-net".to_string());
    // Run in the foreground (don't fork into the background).
    //
    // pasta's default is to daemonise: the spawned process forks and the parent
    // exits immediately with status 0.  This breaks pelagos in two ways:
    //
    //  1. try_wait() sees the parent exit with success and incorrectly reports
    //     "pasta exited before TAP appeared" — a false-positive failure.
    //
    //  2. The relay child is orphaned: teardown_pasta_network kills the parent
    //     (already dead), leaving the child running after the container exits.
    //
    // With --foreground pasta stays as a single process: try_wait() is
    // accurate, kill() in teardown reaches the relay, and the stderr thread
    // correctly collects output when pasta exits for any reason.
    args.push("--foreground".to_string());
    // NOTE: do NOT pass --quiet here. pasta writes error messages to either stdout
    // or stderr depending on the error path and version; suppressing either channel
    // silently discards the actual failure message.
    // In root mode, pasta cannot open /proc/<pid>/ns/net directly:
    //
    //   v0.36.0: pasta <PID> form — EPERM opening /proc/<pid>/ns/user (privilege-drop)
    //   v0.37.0: pasta --netns /proc/<pid>/ns/net — EPERM opening /proc/<pid>/ns/net
    //            (Yama LSM ptrace_scope=1 blocks cross-process /proc/<pid>/ns/ access)
    //   fd-passing (/proc/self/fd/N): rejected by pasta with ENXIO — pasta cannot
    //            open namespace files via /proc/self/fd symlinks (pasta limitation)
    //
    // Solution (v0.38.0): bind-mount the container's netns file onto a path that pasta
    // CAN open.  pelagos performs the bind mount as root (CAP_SYS_ADMIN) BEFORE
    // spawning pasta; the resulting bind-mounted file lives on tmpfs (/run/pelagos/)
    // and is openable by pasta without any /proc/<pid>/ns/ access.
    //
    // The bind-mount path is stored in PastaSetup.ns_bind_mount and unmounted in
    // teardown_pasta_network after pasta is killed.
    let ns_bind_mount = if running_as_root {
        let ns_dir = std::path::Path::new("/run/pelagos/pasta-ns");
        std::fs::create_dir_all(ns_dir)?;
        let mount_path = ns_dir.join(format!("{}", child_pid));
        // Create an empty regular file as the bind-mount target.
        std::fs::write(&mount_path, b"")
            .map_err(|e| io::Error::new(e.kind(), format!("create netns mount point: {}", e)))?;
        let src = std::ffi::CString::new(format!("/proc/{}/ns/net", child_pid)).unwrap();
        let dst = std::ffi::CString::new(mount_path.as_os_str().as_encoded_bytes()).unwrap();
        let fstype = std::ffi::CString::new("").unwrap();
        // SAFETY: all pointers are valid CStrings; MS_BIND is a safe mount flag.
        let rc = unsafe {
            libc::mount(
                src.as_ptr(),
                dst.as_ptr(),
                fstype.as_ptr(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        };
        if rc == -1 {
            let _ = std::fs::remove_file(&mount_path);
            return Err(io::Error::new(
                io::Error::last_os_error().kind(),
                format!(
                    "mount --bind /proc/{}/ns/net {}: {}",
                    child_pid,
                    mount_path.display(),
                    io::Error::last_os_error()
                ),
            ));
        }
        args.push("--netns".to_string());
        args.push(mount_path.to_string_lossy().into_owned());
        args.push("--runas".to_string());
        args.push("0".to_string());
        Some(mount_path)
    } else {
        // Rootless mode: PID form — pasta joins the container's user namespace
        // before the network namespace (required when the container has a user ns).
        // PID must come last (positional argument).
        args.push(child_pid.to_string());
        None
    };

    let mut process = SysCmd::new("pasta")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped()) // captured for diagnostics
        .stderr(std::process::Stdio::piped()) // captured for diagnostics
        .spawn()
        .map_err(|e| {
            // Clean up bind mount if spawn fails.
            if let Some(ref p) = ns_bind_mount {
                unsafe {
                    libc::umount2(
                        p.as_os_str().as_encoded_bytes().as_ptr() as *const libc::c_char,
                        libc::MNT_DETACH,
                    )
                };
                let _ = std::fs::remove_file(p);
            }
            io::Error::other(format!("failed to start pasta (is it installed?): {}", e))
        })?;

    // Drain pasta's stdout and stderr asynchronously on a single thread to prevent
    // either pipe buffer from filling and stalling pasta.  pasta may write its error
    // messages to stdout, stderr, or both depending on the error path and version,
    // so we must capture both.  The thread exits when pasta closes both pipes
    // (i.e. when pasta exits).  We join it in teardown to collect the output.
    let output_thread = {
        use std::io::Read;
        let mut stdout_pipe = process.stdout.take().expect("stdout pipe");
        let mut stderr_pipe = process.stderr.take().expect("stderr pipe");
        Some(std::thread::spawn(move || {
            // Spawn a sub-thread for stderr so both pipes drain concurrently.
            let stderr_thread = std::thread::spawn(move || {
                let mut s = String::new();
                let _ = stderr_pipe.read_to_string(&mut s);
                s
            });
            let mut stdout_out = String::new();
            let _ = stdout_pipe.read_to_string(&mut stdout_out);
            let stderr_out = stderr_thread.join().unwrap_or_default();
            // Merge stdout+stderr into a single string for unified logging.
            let mut combined = stdout_out;
            if !stderr_out.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr_out);
            }
            combined
        }))
    };

    // Wait for pasta to configure the network interface inside the container's netns.
    // pasta runs asynchronously; without waiting the container's first syscalls (e.g.
    // connect, sendto) race against pasta still setting up the TAP.
    //
    // We poll /proc/{pid}/net/dev until a non-loopback interface appears (success),
    // pasta exits before that happens (failure), or a 5-second timeout fires.
    wait_for_pasta_network(child_pid, &mut process);

    Ok(PastaSetup {
        process,
        output_thread,
        ns_bind_mount,
    })
}

/// Poll `/proc/{pid}/net/dev` until pasta has created a non-loopback interface.
///
/// This resolves the race between pasta's async network setup and the container
/// process starting to use the network.  Returns as soon as the interface is
/// visible, pasta exits unexpectedly, or a 5-second timeout fires.
///
/// When pasta exits before the TAP appears its exit status is logged at `warn!`
/// level; the caller's stderr thread will collect pasta's output and log it at
/// teardown.  On timeout the warning includes the container PID so the operator
/// can inspect `/proc/{pid}/net/dev` and pasta's process state manually.
fn wait_for_pasta_network(child_pid: u32, process: &mut std::process::Child) {
    let net_dev = format!("/proc/{}/net/dev", child_pid);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        // Detect pasta exiting before the TAP interface appears — almost always
        // means pasta encountered an error (permission denied, missing /dev/net/tun,
        // bad PID, etc.).  The exit status is the only clue available here; stderr
        // output is collected by the reader thread and logged in teardown.
        if let Ok(Some(status)) = process.try_wait() {
            log::warn!(
                "pasta exited (status: {}) before TAP interface appeared in \
                 /proc/{}/net/dev — network setup failed; stdout/stderr output \
                 will be logged at teardown",
                status,
                child_pid
            );
            return;
        }

        if let Ok(contents) = std::fs::read_to_string(&net_dev) {
            // /proc/{pid}/net/dev has one line per interface; lo is always present.
            // A second interface means pasta has created the TAP.
            let has_non_lo = contents
                .lines()
                .skip(2) // header lines
                .any(|line| {
                    let iface = line.trim().split(':').next().unwrap_or("").trim();
                    !iface.is_empty() && iface != "lo"
                });
            if has_non_lo {
                log::debug!(
                    "pasta: TAP interface appeared in /proc/{}/net/dev",
                    child_pid
                );
                return;
            }
        }

        if std::time::Instant::now() >= deadline {
            log::warn!(
                "pasta network setup timeout (5s) — no non-loopback interface in \
                 /proc/{}/net/dev; container will proceed without pasta networking",
                child_pid
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Kill the pasta relay process and collect its stdout+stderr output for diagnostics.
///
/// Kills pasta (best-effort), reaps the process, then joins the output reader
/// thread.  Any output pasta wrote to stdout or stderr is logged at `warn!` level
/// so operators can diagnose setup failures without needing `RUST_LOG=debug`.
pub fn teardown_pasta_network(setup: &mut PastaSetup) {
    let _ = setup.process.kill();
    let _ = setup.process.wait();
    // Join the output reader thread now that pasta's pipes are closed.
    if let Some(thread) = setup.output_thread.take() {
        match thread.join() {
            Ok(out) if !out.trim().is_empty() => {
                log::warn!("pasta output:\n{}", out.trim());
            }
            Ok(_) => {
                log::debug!("pasta output: (empty)");
            }
            Err(_) => {
                log::debug!("pasta output reader thread panicked");
            }
        }
    }
    // Remove the bind-mounted netns file created in root mode.
    if let Some(ref p) = setup.ns_bind_mount {
        // SAFETY: pointer is valid for the duration of this call.
        let rc = unsafe {
            libc::umount2(
                p.as_os_str().as_encoded_bytes().as_ptr() as *const libc::c_char,
                libc::MNT_DETACH,
            )
        };
        if rc == -1 {
            log::warn!(
                "pasta netns bind-mount umount2 failed for {}: {}",
                p.display(),
                io::Error::last_os_error()
            );
        }
        let _ = std::fs::remove_file(p);
    }
}

/// Returns true if `pasta` is on PATH and responds to `--version`.
pub fn is_pasta_available() -> bool {
    SysCmd::new("pasta")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

/// Like `run` but suppresses stderr (for best-effort cleanup loops).
fn run_quiet(cmd: &str, args: &[&str]) -> io::Result<()> {
    use std::process::Stdio as ProcStdio;
    let status = SysCmd::new(cmd)
        .args(args)
        .stderr(ProcStdio::null())
        .status()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Ipv4Net tests ────────────────────────────────────────────────────

    #[test]
    fn test_ipv4net_parse_valid() {
        let net = Ipv4Net::from_cidr("10.88.1.0/24").unwrap();
        assert_eq!(net.addr, Ipv4Addr::new(10, 88, 1, 0));
        assert_eq!(net.prefix_len, 24);
    }

    #[test]
    fn test_ipv4net_parse_invalid() {
        assert!(Ipv4Net::from_cidr("not-a-cidr").is_err());
        assert!(Ipv4Net::from_cidr("10.0.0.0/33").is_err());
        assert!(Ipv4Net::from_cidr("10.0.0.0").is_err());
        assert!(Ipv4Net::from_cidr("999.0.0.0/24").is_err());
    }

    #[test]
    fn test_ipv4net_network_broadcast() {
        let net = Ipv4Net::from_cidr("10.88.1.0/24").unwrap();
        assert_eq!(net.network(), Ipv4Addr::new(10, 88, 1, 0));
        assert_eq!(net.broadcast(), Ipv4Addr::new(10, 88, 1, 255));
    }

    #[test]
    fn test_ipv4net_gateway_hosts() {
        let net = Ipv4Net::from_cidr("172.19.0.0/24").unwrap();
        assert_eq!(net.gateway(), Ipv4Addr::new(172, 19, 0, 1));
        assert_eq!(net.host_min(), Ipv4Addr::new(172, 19, 0, 2));
        assert_eq!(net.host_max(), Ipv4Addr::new(172, 19, 0, 254));
    }

    #[test]
    fn test_ipv4net_contains() {
        let net = Ipv4Net::from_cidr("10.88.1.0/24").unwrap();
        assert!(net.contains(Ipv4Addr::new(10, 88, 1, 5)));
        assert!(net.contains(Ipv4Addr::new(10, 88, 1, 254)));
        assert!(!net.contains(Ipv4Addr::new(10, 88, 2, 1)));
        assert!(!net.contains(Ipv4Addr::new(192, 168, 1, 1)));
    }

    #[test]
    fn test_ipv4net_overlaps() {
        let a = Ipv4Net::from_cidr("10.88.0.0/16").unwrap();
        let b = Ipv4Net::from_cidr("10.88.1.0/24").unwrap();
        assert!(a.overlaps(&b));
        assert!(b.overlaps(&a));

        let c = Ipv4Net::from_cidr("10.89.0.0/16").unwrap();
        assert!(!a.overlaps(&c));
    }

    #[test]
    fn test_ipv4net_no_overlap_disjoint() {
        let a = Ipv4Net::from_cidr("10.0.0.0/24").unwrap();
        let b = Ipv4Net::from_cidr("10.0.1.0/24").unwrap();
        assert!(!a.overlaps(&b));
    }

    #[test]
    fn test_ipv4net_cidr_string() {
        let net = Ipv4Net::from_cidr("10.88.1.0/24").unwrap();
        assert_eq!(net.cidr_string(), "10.88.1.0/24");
        assert_eq!(net.gateway_cidr(), "10.88.1.1/24");
    }

    #[test]
    fn test_ipv4net_slash16() {
        let net = Ipv4Net::from_cidr("10.0.0.0/16").unwrap();
        assert_eq!(net.network(), Ipv4Addr::new(10, 0, 0, 0));
        assert_eq!(net.broadcast(), Ipv4Addr::new(10, 0, 255, 255));
        assert_eq!(net.gateway(), Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(net.host_min(), Ipv4Addr::new(10, 0, 0, 2));
        assert_eq!(net.host_max(), Ipv4Addr::new(10, 0, 255, 254));
    }

    // ── NetworkDef tests ─────────────────────────────────────────────────

    #[test]
    fn test_network_def_serde_roundtrip() {
        let net = NetworkDef {
            name: "frontend".to_string(),
            subnet: Ipv4Net::from_cidr("10.88.1.0/24").unwrap(),
            gateway: Ipv4Addr::new(10, 88, 1, 1),
            bridge_name: "rm-frontend".to_string(),
        };
        let json = serde_json::to_string(&net).unwrap();
        let parsed: NetworkDef = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "frontend");
        assert_eq!(parsed.subnet, net.subnet);
        assert_eq!(parsed.gateway, net.gateway);
        assert_eq!(parsed.bridge_name, "rm-frontend");
    }

    #[test]
    fn test_network_def_nft_table_name() {
        let net = NetworkDef {
            name: "frontend".to_string(),
            subnet: Ipv4Net::from_cidr("10.88.1.0/24").unwrap(),
            gateway: Ipv4Addr::new(10, 88, 1, 1),
            bridge_name: "rm-frontend".to_string(),
        };
        assert_eq!(net.nft_table_name(), "pelagos-frontend");
    }

    // ── NetworkMode tests ────────────────────────────────────────────────

    #[test]
    fn test_network_mode_is_bridge() {
        assert!(!NetworkMode::None.is_bridge());
        assert!(!NetworkMode::Loopback.is_bridge());
        assert!(NetworkMode::Bridge.is_bridge());
        assert!(NetworkMode::BridgeNamed("frontend".into()).is_bridge());
        assert!(!NetworkMode::Pasta.is_bridge());
    }

    #[test]
    fn test_veth_names_for_network_unique() {
        let (h1, p1) = super::veth_names_for("rem-123-0");
        let (h2, p2) = super::veth_names_for_network("rem-123-0", "frontend");
        let (h3, p3) = super::veth_names_for_network("rem-123-0", "backend");
        // Primary and secondary must differ
        assert_ne!(h1, h2);
        assert_ne!(p1, p2);
        // Different networks must differ
        assert_ne!(h2, h3);
        assert_ne!(p2, p3);
        // Names must be within IFNAMSIZ (15 chars)
        assert!(h1.len() <= 15);
        assert!(h2.len() <= 15);
        assert!(h3.len() <= 15);
    }

    // ── PortProto tests ──────────────────────────────────────────────────

    #[test]
    fn test_port_proto_parse() {
        assert_eq!(PortProto::parse("tcp"), PortProto::Tcp);
        assert_eq!(PortProto::parse("udp"), PortProto::Udp);
        assert_eq!(PortProto::parse("both"), PortProto::Both);
        // Defaults to Tcp for unknown values (Docker compat).
        assert_eq!(PortProto::parse(""), PortProto::Tcp);
        assert_eq!(PortProto::parse("sctp"), PortProto::Tcp);
    }

    #[test]
    fn test_port_proto_as_str() {
        assert_eq!(PortProto::Tcp.as_str(), "tcp");
        assert_eq!(PortProto::Udp.as_str(), "udp");
        assert_eq!(PortProto::Both.as_str(), "both");
    }

    #[test]
    fn test_parse_port_forward_line_with_proto() {
        // Old format (ip:hp:cp:proto) — ns_name is empty, fields shift right by 1.
        let r = parse_port_forward_line("192.168.1.5:8080:80:udp").unwrap();
        assert_eq!(r.0, ""); // old format → empty ns_name
        assert_eq!(r.1, "192.168.1.5".parse::<std::net::Ipv4Addr>().unwrap());
        assert_eq!(r.2, 8080);
        assert_eq!(r.3, 80);
        assert_eq!(r.4, PortProto::Udp);
    }

    #[test]
    fn test_parse_port_forward_line_backwards_compat() {
        // Old format lines without a proto field must default to Tcp.
        let r = parse_port_forward_line("10.0.0.2:443:443").unwrap();
        assert_eq!(r.0, ""); // old format → empty ns_name
        assert_eq!(r.4, PortProto::Tcp);
    }

    #[test]
    fn test_parse_port_forward_line_both_proto() {
        // Old format.
        let r = parse_port_forward_line("172.19.0.2:53:53:both").unwrap();
        assert_eq!(r.4, PortProto::Both);
    }

    #[test]
    fn test_parse_port_forward_line_new_format() {
        // New format (ns_name:ip:hp:cp:proto).
        let r = parse_port_forward_line("rem-1234-0:10.0.0.3:8080:80:tcp").unwrap();
        assert_eq!(r.0, "rem-1234-0");
        assert_eq!(r.1, "10.0.0.3".parse::<std::net::Ipv4Addr>().unwrap());
        assert_eq!(r.2, 8080);
        assert_eq!(r.3, 80);
        assert_eq!(r.4, PortProto::Tcp);
    }

    #[test]
    fn test_build_prerouting_script_tcp_only() {
        use std::net::Ipv4Addr;
        let net = NetworkDef {
            name: "test".to_string(),
            subnet: Ipv4Net::from_cidr("172.19.0.0/24").unwrap(),
            gateway: Ipv4Addr::new(172, 19, 0, 1),
            bridge_name: "pelagos0".to_string(),
        };
        let ip = Ipv4Addr::new(172, 19, 0, 2);
        let entries = vec![(ip, 8080u16, 80u16, PortProto::Tcp)];
        let script = build_prerouting_script(&net, &entries);
        assert!(script.contains("tcp dport 8080 dnat to 172.19.0.2:80"));
        assert!(!script.contains("udp"));
    }

    #[test]
    fn test_build_prerouting_script_udp_only() {
        use std::net::Ipv4Addr;
        let net = NetworkDef {
            name: "test".to_string(),
            subnet: Ipv4Net::from_cidr("172.19.0.0/24").unwrap(),
            gateway: Ipv4Addr::new(172, 19, 0, 1),
            bridge_name: "pelagos0".to_string(),
        };
        let ip = Ipv4Addr::new(172, 19, 0, 2);
        let entries = vec![(ip, 5353u16, 53u16, PortProto::Udp)];
        let script = build_prerouting_script(&net, &entries);
        assert!(script.contains("udp dport 5353 dnat to 172.19.0.2:53"));
        assert!(!script.contains("tcp dport"));
    }

    #[test]
    fn test_build_prerouting_script_both() {
        use std::net::Ipv4Addr;
        let net = NetworkDef {
            name: "test".to_string(),
            subnet: Ipv4Net::from_cidr("172.19.0.0/24").unwrap(),
            gateway: Ipv4Addr::new(172, 19, 0, 1),
            bridge_name: "pelagos0".to_string(),
        };
        let ip = Ipv4Addr::new(172, 19, 0, 2);
        let entries = vec![(ip, 53u16, 53u16, PortProto::Both)];
        let script = build_prerouting_script(&net, &entries);
        assert!(script.contains("tcp dport 53 dnat to 172.19.0.2:53"));
        assert!(script.contains("udp dport 53 dnat to 172.19.0.2:53"));
    }

    #[test]
    fn test_network_mode_bridge_network_name() {
        assert_eq!(NetworkMode::Bridge.bridge_network_name(), Some("pelagos0"));
        assert_eq!(
            NetworkMode::BridgeNamed("test".into()).bridge_network_name(),
            Some("test")
        );
        assert_eq!(NetworkMode::None.bridge_network_name(), None);
        assert_eq!(NetworkMode::Loopback.bridge_network_name(), None);
    }

    // ── Async TCP proxy unit tests ────────────────────────────────────────────
    //
    // These tests exercise start_tcp_proxies_async, tcp_accept_loop, and
    // tcp_relay directly against a localhost echo server — no root or container
    // required.

    /// Spin up a std-thread TCP echo server on an OS-assigned port.
    /// Returns the bound SocketAddr and a stop flag; set the flag to shut it down.
    fn spawn_echo_server() -> (SocketAddr, Arc<AtomicBool>) {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("echo server bind");
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                            .ok();
                        let mut buf = vec![0u8; 4096];
                        // Echo all bytes back then close.
                        while let Ok(n) = stream.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            let _ = stream.write_all(&buf[..n]);
                        }
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        (addr, stop)
    }

    /// Verify that the async TCP proxy correctly relays data in both directions.
    ///
    /// Starts a localhost echo server, creates a proxy runtime pointing to it,
    /// connects through the proxy, sends a payload, reads the echo back, and
    /// asserts the round-trip is correct.  No root or rootfs required.
    #[test]
    fn test_tcp_proxy_bidirectional_relay() {
        use std::io::{Read, Write};
        let (server_addr, server_stop) = spawn_echo_server();
        let container_ip: Ipv4Addr = server_addr.ip().to_string().parse().unwrap();
        let container_port = server_addr.port();

        let rt = start_tcp_proxies_async(container_ip, &[(0, container_port)]);
        // Give the accept loop a moment to bind.
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Find which port the proxy bound by probing — we passed host_port=0 so
        // the OS chose one; discover it by looking at what's listening.
        // Instead, use a fixed port to avoid the discovery problem.
        drop(rt);
        server_stop.store(true, Ordering::Relaxed);

        // Redo with a fixed host port.
        let proxy_port: u16 = 19290;
        let (server_addr2, server_stop2) = spawn_echo_server();
        let container_ip2: Ipv4Addr = server_addr2.ip().to_string().parse().unwrap();
        let container_port2 = server_addr2.port();

        let rt2 = start_tcp_proxies_async(container_ip2, &[(proxy_port, container_port2)]);
        std::thread::sleep(std::time::Duration::from_millis(100));

        let payload = b"hello-from-client";
        let mut response = vec![0u8; payload.len()];
        {
            let mut conn =
                std::net::TcpStream::connect(format!("127.0.0.1:{}", proxy_port)).unwrap();
            conn.set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .ok();
            conn.write_all(payload).unwrap();
            conn.shutdown(std::net::Shutdown::Write).unwrap();
            conn.read_exact(&mut response).unwrap();
        }

        drop(rt2);
        server_stop2.store(true, Ordering::Relaxed);

        assert_eq!(
            &response, payload,
            "proxy should relay bytes unchanged in both directions"
        );
    }

    /// Verify that simultaneous connections through the proxy are all served.
    ///
    /// Starts a localhost echo server, creates the proxy, opens 8 connections
    /// from separate threads simultaneously, each sending a unique payload and
    /// reading the echo.  All must succeed, confirming the tokio runtime
    /// schedules concurrent relay tasks correctly.
    #[test]
    fn test_tcp_proxy_concurrent_relay() {
        use std::io::{Read, Write};
        let proxy_port: u16 = 19291;
        let (server_addr, server_stop) = spawn_echo_server();
        let container_ip: Ipv4Addr = server_addr.ip().to_string().parse().unwrap();
        let container_port = server_addr.port();

        let rt = start_tcp_proxies_async(container_ip, &[(proxy_port, container_port)]);
        std::thread::sleep(std::time::Duration::from_millis(100));

        const N: usize = 8;
        let handles: Vec<_> = (0..N)
            .map(|i| {
                std::thread::spawn(move || -> bool {
                    let payload = format!("payload-{:02}", i);
                    let mut conn =
                        match std::net::TcpStream::connect(format!("127.0.0.1:{}", proxy_port)) {
                            Ok(c) => c,
                            Err(_) => return false,
                        };
                    conn.set_read_timeout(Some(std::time::Duration::from_secs(3)))
                        .ok();
                    if conn.write_all(payload.as_bytes()).is_err() {
                        return false;
                    }
                    conn.shutdown(std::net::Shutdown::Write).ok();
                    let mut buf = vec![0u8; payload.len()];
                    if conn.read_exact(&mut buf).is_err() {
                        return false;
                    }
                    buf == payload.as_bytes()
                })
            })
            .collect();

        let results: Vec<bool> = handles
            .into_iter()
            .map(|h| h.join().unwrap_or(false))
            .collect();
        drop(rt);
        server_stop.store(true, Ordering::Relaxed);

        let failures: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, &ok)| !ok)
            .map(|(i, _)| i)
            .collect();
        assert!(
            failures.is_empty(),
            "concurrent relay failed for connections: {:?}",
            failures
        );
    }

    /// Verify that dropping the runtime releases the listener port.
    ///
    /// Creates the proxy, drops the runtime, then asserts the port can be
    /// rebound — confirming shutdown_background() cancels the accept loop task.
    #[test]
    fn test_tcp_proxy_runtime_cleanup() {
        let proxy_port: u16 = 19292;
        let (server_addr, server_stop) = spawn_echo_server();
        let container_ip: Ipv4Addr = server_addr.ip().to_string().parse().unwrap();
        let container_port = server_addr.port();

        let rt = start_tcp_proxies_async(container_ip, &[(proxy_port, container_port)]);
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Port should be occupied.
        assert!(
            std::net::TcpListener::bind(format!("0.0.0.0:{}", proxy_port)).is_err(),
            "port should be bound while runtime is alive"
        );

        rt.shutdown_background();
        std::thread::sleep(std::time::Duration::from_millis(200));
        server_stop.store(true, Ordering::Relaxed);

        // Port should now be free.
        assert!(
            std::net::TcpListener::bind(format!("0.0.0.0:{}", proxy_port)).is_ok(),
            "port should be released after runtime shutdown"
        );
    }
}
