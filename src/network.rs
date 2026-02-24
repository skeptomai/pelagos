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
//! `ip netns list` and inspectable with `ip netns exec remora-foo ip addr`
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
use std::io::{self, Read, Seek, SeekFrom, Write as IoWrite};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::io::AsRawFd;
use std::process::Command as SysCmd;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

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

    /// nftables table name for this network (e.g. `"remora-frontend"`).
    pub fn nft_table_name(&self) -> String {
        format!("remora-{}", self.name)
    }
}

/// Bootstrap or load the default `remora0` network definition.
///
/// If a config file exists on disk, loads it. Otherwise creates the default
/// definition (`172.19.0.0/24`, bridge `remora0`) and persists it.
/// Also migrates old global state files to per-network directories if they exist.
pub fn bootstrap_default_network() -> io::Result<NetworkDef> {
    let config_path = crate::paths::network_config_dir("remora0").join("config.json");
    if config_path.exists() {
        return NetworkDef::load("remora0");
    }

    let net = NetworkDef {
        name: "remora0".to_string(),
        subnet: Ipv4Net {
            addr: Ipv4Addr::new(172, 19, 0, 0),
            prefix_len: 24,
        },
        gateway: Ipv4Addr::new(172, 19, 0, 1),
        bridge_name: "remora0".to_string(),
    };
    net.save()?;

    // Migrate old global IPAM file to per-network dir if it exists.
    // Only next_ip is migrated — nat_refcount and port_forwards tracked
    // state for the old `ip remora` nft table which no longer exists
    // (now `ip remora-remora0`), so stale values would poison refcounts.
    let rt = crate::paths::runtime_dir();
    let net_rt = crate::paths::network_runtime_dir("remora0");
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
/// For `"remora0"`, calls [`bootstrap_default_network`] to ensure it exists.
/// For other names, loads from config dir.
pub fn load_network_def(name: &str) -> io::Result<NetworkDef> {
    if name == "remora0" {
        bootstrap_default_network()
    } else {
        NetworkDef::load(name)
    }
}

// Legacy constants — kept for reference but internal code now uses NetworkDef.
/// Bridge name for the default network.
pub const BRIDGE_NAME: &str = "remora0";
/// Gateway IP for the default network.
pub const BRIDGE_GW: &str = "172.19.0.1";

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
    /// Full connectivity via the default `remora0` bridge (172.19.0.x/24).
    ///
    /// Normalized to `BridgeNamed("remora0")` internally by `with_network()`.
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
            NetworkMode::Bridge => Some("remora0"),
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
    pub port_forwards: Vec<(u16, u16)>,
    /// Userspace TCP proxy stop flag — set to `true` to stop all proxy threads.
    proxy_stop: Option<Arc<AtomicBool>>,
    /// Name of the network this setup belongs to (e.g. `"remora0"`, `"frontend"`).
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
            .field("proxy_active", &self.proxy_stop.is_some())
            .field("network_name", &self.network_name)
            .finish()
    }
}

/// Runtime state for a pasta-backed container; holds the pasta process for teardown.
pub struct PastaSetup {
    process: std::process::Child,
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
    port_forwards: Vec<(u16, u16)>,
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
        enable_port_forwards(&net_def, container_ip, &port_forwards)?;
    }

    // 9. Start userspace TCP proxy for port forwards (handles localhost traffic
    //    that nftables DNAT in PREROUTING cannot intercept).
    let proxy_stop = if !port_forwards.is_empty() {
        Some(start_port_proxies(container_ip, &port_forwards))
    } else {
        None
    };

    Ok(NetworkSetup {
        veth_host,
        ns_name: ns_name.to_string(),
        container_ip,
        nat_enabled: nat,
        port_forwards,
        proxy_stop,
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
pub fn teardown_network(setup: &NetworkSetup) {
    // Stop userspace TCP proxies first so they don't try to connect to a
    // vanishing container IP after the veth is deleted.
    if let Some(ref stop) = setup.proxy_stop {
        stop.store(true, Ordering::Relaxed);
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
        disable_port_forwards(&net_def, setup.container_ip, &setup.port_forwards);
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
        proxy_stop: None,
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
        // `ip remora` table that previous versions created.
        if net.name == "remora0" {
            let _ = run_nft_quiet("delete table ip remora\n");
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

/// Parse one line from [`crate::paths::port_forwards_file()`]: `{ip}:{host_port}:{container_port}`.
fn parse_port_forward_line(line: &str) -> Option<(Ipv4Addr, u16, u16)> {
    let mut parts = line.splitn(3, ':');
    let ip: Ipv4Addr = parts.next()?.parse().ok()?;
    let host_port: u16 = parts.next()?.parse().ok()?;
    let container_port: u16 = parts.next()?.parse().ok()?;
    Some((ip, host_port, container_port))
}

/// Read all port-forward entries from an already-flocked file.
fn read_port_forwards_locked(file: &mut std::fs::File) -> io::Result<Vec<(Ipv4Addr, u16, u16)>> {
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

/// Count active port-forward entries (reads without locking — for teardown checks only).
fn read_port_forwards_count(network_name: &str) -> usize {
    let content =
        match std::fs::read_to_string(crate::paths::network_port_forwards_file(network_name)) {
            Ok(c) => c,
            Err(_) => return 0,
        };
    content.lines().filter(|l| !l.trim().is_empty()).count()
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
fn build_prerouting_script(net: &NetworkDef, entries: &[(Ipv4Addr, u16, u16)]) -> String {
    let table = net.nft_table_name();
    let mut s = format!(
        "add table ip {table}\n\
         add chain ip {table} prerouting {{ type nat hook prerouting priority -100; }}\n\
         flush chain ip {table} prerouting\n",
    );
    for (ip, host_port, container_port) in entries {
        s.push_str(&format!(
            "add rule ip {} prerouting tcp dport {} dnat to {}:{}\n",
            table, host_port, ip, container_port
        ));
    }
    s
}

/// Add port-forward entries to the state file and install nftables DNAT rules.
///
/// Uses `flock(LOCK_EX)` on the per-network port-forwards file to serialise
/// concurrent spawns. The network's nft table / prerouting chain are created
/// idempotently, so this is safe whether NAT is enabled or not.
fn enable_port_forwards(
    net: &NetworkDef,
    container_ip: Ipv4Addr,
    forwards: &[(u16, u16)],
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

    let mut entries = read_port_forwards_locked(&mut file)?;

    for &(host_port, container_port) in forwards {
        entries.push((container_ip, host_port, container_port));
    }

    // Overwrite file with all entries.
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    for (ip, hp, cp) in &entries {
        writeln!(file, "{}:{}:{}", ip, hp, cp)?;
    }

    // Install nftables DNAT rules.
    let script = build_prerouting_script(net, &entries);
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
/// Errors are non-fatal (logged via `log::warn!`).
fn disable_port_forwards(net: &NetworkDef, container_ip: Ipv4Addr, forwards: &[(u16, u16)]) {
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

    // Remove all entries belonging to this container.
    let remaining: Vec<(Ipv4Addr, u16, u16)> = entries
        .into_iter()
        .filter(|(ip, hp, _cp)| !(*ip == container_ip && forwards.iter().any(|&(h, _)| h == *hp)))
        .collect();

    // Write remaining entries back.
    if let Err(e) = file.seek(SeekFrom::Start(0)).and_then(|_| file.set_len(0)) {
        log::warn!("port forwards file truncate (non-fatal): {}", e);
        return;
    }
    for (ip, hp, cp) in &remaining {
        let _ = writeln!(file, "{}:{}:{}", ip, hp, cp);
    }

    // flock released; now update nftables.
    drop(file);

    let table = net.nft_table_name();
    if remaining.is_empty() {
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
        let script = build_prerouting_script(net, &remaining);
        if let Err(e) = run_nft(&script) {
            log::warn!("nft rebuild prerouting (non-fatal): {}", e);
        }
    }
}

// ── Userspace TCP port proxy ─────────────────────────────────────────────────

/// Start background TCP proxy threads for each port mapping.
///
/// nftables DNAT rules in the PREROUTING chain handle traffic from external
/// hosts, but traffic originating from localhost bypasses PREROUTING entirely
/// (it goes through OUTPUT). Docker solves this with `docker-proxy` — a
/// userspace TCP relay. This is Remora's equivalent.
///
/// Each port mapping gets a `TcpListener` on `0.0.0.0:{host_port}`. Accepted
/// connections are relayed bidirectionally to `{container_ip}:{container_port}`.
/// Returns a stop flag that must be set to `true` on teardown.
fn start_port_proxies(container_ip: Ipv4Addr, forwards: &[(u16, u16)]) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));

    for &(host_port, container_port) in forwards {
        let stop = Arc::clone(&stop);
        let target = SocketAddr::from((container_ip, container_port));

        std::thread::spawn(move || {
            let listener = match TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], host_port))) {
                Ok(l) => l,
                Err(e) => {
                    log::warn!(
                        "port proxy: cannot bind 0.0.0.0:{}: {} (nftables DNAT still active)",
                        host_port,
                        e
                    );
                    return;
                }
            };
            // Non-blocking accept so we can check the stop flag periodically.
            listener
                .set_nonblocking(true)
                .expect("set_nonblocking failed");

            log::debug!("port proxy: 0.0.0.0:{} -> {}", host_port, target);

            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((client, _addr)) => {
                        let target = target;
                        let stop = Arc::clone(&stop);
                        std::thread::spawn(move || {
                            proxy_relay(client, target, &stop);
                        });
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(e) => {
                        if !stop.load(Ordering::Relaxed) {
                            log::warn!("port proxy accept error on port {}: {}", host_port, e);
                        }
                        break;
                    }
                }
            }
        });
    }

    stop
}

/// Bidirectional TCP relay between a client socket and a container target.
///
/// Uses two threads (one per direction). Terminates when either side closes
/// or the stop flag is set.
fn proxy_relay(client: TcpStream, target: SocketAddr, stop: &AtomicBool) {
    let upstream = match TcpStream::connect_timeout(&target, std::time::Duration::from_secs(5)) {
        Ok(s) => s,
        Err(e) => {
            log::debug!("port proxy: cannot connect to {}: {}", target, e);
            return;
        }
    };

    // Set read timeouts so threads check the stop flag periodically.
    let timeout = Some(std::time::Duration::from_millis(200));
    let _ = client.set_read_timeout(timeout);
    let _ = upstream.set_read_timeout(timeout);

    let client_r = client;
    let upstream_r = upstream;
    let client_w = match client_r.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };
    let upstream_w = match upstream_r.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };

    let stop_flag = stop as *const AtomicBool as usize; // share across threads
    let t1 = std::thread::spawn(move || {
        let stop = unsafe { &*(stop_flag as *const AtomicBool) };
        copy_until_done(client_r, upstream_w, stop);
    });
    let t2 = std::thread::spawn(move || {
        let stop = unsafe { &*(stop_flag as *const AtomicBool) };
        copy_until_done(upstream_r, client_w, stop);
    });

    let _ = t1.join();
    let _ = t2.join();
}

/// Copy bytes from `reader` to `writer` until EOF, error, or stop flag.
fn copy_until_done(mut reader: TcpStream, mut writer: TcpStream, stop: &AtomicBool) {
    let mut buf = [0u8; 8192];
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if std::io::Write::write_all(&mut writer, &buf[..n]).is_err() {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(_) => break,
        }
    }
    let _ = writer.shutdown(std::net::Shutdown::Write);
}

// ── N5: pasta user-mode networking ───────────────────────────────────────────

/// Spawn pasta attached to an already-running container's network namespace.
///
/// Called in the *parent*, immediately after `spawn()` returns (child has exec'd).
/// pasta receives the container's netns via `/proc/{child_pid}/ns/net`.
///
/// pasta runs as a background process; call [`teardown_pasta_network`] after
/// the container exits to kill it and reap the process.
pub fn setup_pasta_network(child_pid: u32, port_forwards: &[(u16, u16)]) -> io::Result<PastaSetup> {
    let mut args: Vec<String> = vec![];

    // Use the PID form: `pasta [OPTIONS] PID`.
    //
    // When pasta receives a PID it joins that process's *existing* user namespace
    // (via /proc/{pid}/ns/user) rather than creating a new one to drop privileges.
    // In root mode the container's user namespace is the host/initial namespace —
    // joining it is a no-op, so pasta retains full capabilities and can open the
    // container's /proc/{pid}/ns/net without "Permission denied".
    //
    // The --netns PATH form triggers a different code path where pasta *creates* a
    // new user namespace for the drop-to-nobody dance, and then lacks access to the
    // target netns file from within that new user namespace.

    for (host, container) in port_forwards {
        args.push("-t".to_string());
        args.push(format!("{}:{}", host, container));
    }
    // Tell pasta to configure IP address and routes inside the container's netns.
    // Without this flag pasta only creates the TAP; the container would need to run
    // a DHCP client (udhcpc) before the interface has an IP or default route.
    args.push("--config-net".to_string());
    args.push("--quiet".to_string());
    // PID must come last (positional argument).
    args.push(child_pid.to_string());

    let process = SysCmd::new("pasta")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            io::Error::other(format!("failed to start pasta (is it installed?): {}", e))
        })?;

    Ok(PastaSetup { process })
}

/// Kill the pasta relay process (best-effort; errors are non-fatal).
pub fn teardown_pasta_network(setup: &mut PastaSetup) {
    let _ = setup.process.kill();
    let _ = setup.process.wait();
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
        assert_eq!(net.nft_table_name(), "remora-frontend");
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

    #[test]
    fn test_network_mode_bridge_network_name() {
        assert_eq!(NetworkMode::Bridge.bridge_network_name(), Some("remora0"));
        assert_eq!(
            NetworkMode::BridgeNamed("test".into()).bridge_network_name(),
            Some("test")
        );
        assert_eq!(NetworkMode::None.bridge_network_name(), None);
        assert_eq!(NetworkMode::Loopback.bridge_network_name(), None);
    }
}
