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
/// Reference count for active NAT containers; protected by flock.
const NAT_REFCOUNT_FILE: &str = "/run/remora/nat_refcount";
/// Active port-forward entries; protected by flock. One line per entry:
/// `{container_ip}:{host_port}:{container_port}`
const PORT_FORWARDS_FILE: &str = "/run/remora/port_forwards";

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
    /// User-mode networking via `pasta` — rootless-compatible, full internet access.
    ///
    /// pasta creates a TAP interface inside the container's network namespace and
    /// relays packets to/from the host using ordinary userspace sockets, requiring
    /// no kernel privileges. Works for both root and rootless containers.
    Pasta,
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
    /// Whether NAT (MASQUERADE) was enabled for this container.
    pub nat_enabled: bool,
    /// Port forwards configured for this container: `(host_port, container_port)`.
    pub port_forwards: Vec<(u16, u16)>,
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
        .stderr(std::process::Stdio::null())
        .status();

    // Assign gateway IP (ignore error if already assigned)
    let _ = SysCmd::new("ip")
        .args(["addr", "add", BRIDGE_CIDR, "dev", BRIDGE_NAME])
        .stderr(std::process::Stdio::null())
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
        .truncate(false)
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
    nat: bool,
    port_forwards: Vec<(u16, u16)>,
) -> io::Result<NetworkSetup> {
    ensure_bridge()?;

    let container_ip = allocate_ip()?;
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

    let ip_cidr = format!("{}/24", container_ip);

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
        &["-n", ns_name, "route", "add", "default", "via", BRIDGE_GW],
    )?;

    // 6. Attach host-side veth to bridge and bring it up
    run("ip", &["link", "set", &veth_host, "master", BRIDGE_NAME])?;
    run("ip", &["link", "set", &veth_host, "up"])?;

    // 7. Optionally enable NAT (MASQUERADE) for internet access.
    if nat {
        enable_nat()?;
    }

    // 8. Optionally install port-forward (DNAT) rules.
    if !port_forwards.is_empty() {
        enable_port_forwards(container_ip, &port_forwards)?;
    }

    Ok(NetworkSetup {
        veth_host,
        ns_name: ns_name.to_string(),
        container_ip,
        nat_enabled: nat,
        port_forwards,
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
    if let Err(e) = run("ip", &["link", "del", &setup.veth_host]) {
        log::warn!("network teardown veth (non-fatal): {}", e);
    }
    if let Err(e) = run("ip", &["netns", "del", &setup.ns_name]) {
        log::warn!("network teardown netns (non-fatal): {}", e);
    }
    if !setup.port_forwards.is_empty() {
        disable_port_forwards(setup.container_ip, &setup.port_forwards);
    }
    if setup.nat_enabled {
        disable_nat();
    }
}

// ── N3: NAT / MASQUERADE ─────────────────────────────────────────────────────

/// nftables script that installs MASQUERADE + FORWARD rules for the remora subnet.
///
/// Uses `add` so the commands are idempotent if the table already exists
/// (e.g. if a previous run crashed with the refcount > 0).
///
/// The forward chain is required because the host's default FORWARD policy may
/// be DROP (common on systems with a firewall). Without it, ICMP (ping) may
/// work but TCP/UDP traffic is silently dropped.
const NFT_ADD_SCRIPT: &str = "\
add table ip remora
add chain ip remora postrouting { type nat hook postrouting priority 100; }
add rule ip remora postrouting ip saddr 172.19.0.0/24 oifname != \"remora0\" masquerade
add chain ip remora forward { type filter hook forward priority 0; }
add rule ip remora forward ip saddr 172.19.0.0/24 accept
add rule ip remora forward ip daddr 172.19.0.0/24 accept
";

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

/// Increment the NAT refcount; install nftables rules when going 0 → 1.
///
/// Uses `flock(LOCK_EX)` on [`NAT_REFCOUNT_FILE`] to serialise concurrent
/// spawns. IP forwarding is written to `/proc/sys/net/ipv4/ip_forward`
/// once (never disabled on teardown — other software may rely on it).
fn enable_nat() -> io::Result<()> {
    std::fs::create_dir_all(REMORA_RUN_DIR)?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(NAT_REFCOUNT_FILE)?;

    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };

    let mut content = String::new();
    file.read_to_string(&mut content)?;
    let count: u32 = content.trim().parse().unwrap_or(0);

    if count == 0 {
        // Enable IP forwarding.
        std::fs::write("/proc/sys/net/ipv4/ip_forward", b"1\n")?;
        // Install the nftables MASQUERADE rule set.
        run_nft(NFT_ADD_SCRIPT)?;
        // Also insert iptables FORWARD rules for compatibility with hosts
        // running UFW, Docker, or other iptables-based firewalls that set
        // the FORWARD chain policy to DROP. Without these, TCP/UDP packets
        // from the remora subnet are silently dropped even though nftables
        // MASQUERADE is in place (ICMP/ping may still work via conntrack).
        let _ = run(
            "iptables",
            &["-I", "FORWARD", "-s", "172.19.0.0/24", "-j", "ACCEPT"],
        );
        let _ = run(
            "iptables",
            &["-I", "FORWARD", "-d", "172.19.0.0/24", "-j", "ACCEPT"],
        );
    }

    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    write!(file, "{}", count + 1)?;
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
fn disable_nat() {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(NAT_REFCOUNT_FILE);

    let mut file = match file {
        Ok(f) => f,
        Err(e) => {
            log::warn!("NAT refcount open (non-fatal): {}", e);
            return;
        }
    };

    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };

    let mut content = String::new();
    if let Err(e) = file.read_to_string(&mut content) {
        log::warn!("NAT refcount read (non-fatal): {}", e);
        return;
    }
    let count: u32 = content.trim().parse().unwrap_or(0);

    if count <= 1 {
        if let Err(e) = file
            .seek(SeekFrom::Start(0))
            .and_then(|_| file.set_len(0))
            .and_then(|_| {
                write!(file, "0")?;
                Ok(())
            })
        {
            log::warn!("NAT refcount write (non-fatal): {}", e);
        }
        // flock on refcount released; now decide what to do with the table.
        drop(file);

        // Remove the iptables FORWARD rules added by enable_nat().
        let _ = run(
            "iptables",
            &["-D", "FORWARD", "-s", "172.19.0.0/24", "-j", "ACCEPT"],
        );
        let _ = run(
            "iptables",
            &["-D", "FORWARD", "-d", "172.19.0.0/24", "-j", "ACCEPT"],
        );

        if read_port_forwards_count() == 0 {
            // No active port forwards either — remove the entire table.
            if let Err(e) = run_nft("delete table ip remora\n") {
                log::warn!("nft delete table ip remora (non-fatal): {}", e);
            }
        } else {
            // Port forwards still active — remove MASQUERADE but keep the table.
            let _ = run_nft("flush chain ip remora postrouting\n");
        }
    } else if let Err(e) = file
        .seek(SeekFrom::Start(0))
        .and_then(|_| file.set_len(0))
        .and_then(|_| {
            write!(file, "{}", count - 1)?;
            Ok(())
        })
    {
        log::warn!("NAT refcount write (non-fatal): {}", e);
    }
}

// ── N4: Port mapping (DNAT) ───────────────────────────────────────────────────

/// Parse one line from [`PORT_FORWARDS_FILE`]: `{ip}:{host_port}:{container_port}`.
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
fn read_port_forwards_count() -> usize {
    let content = match std::fs::read_to_string(PORT_FORWARDS_FILE) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    content.lines().filter(|l| !l.trim().is_empty()).count()
}

/// Read the NAT refcount without locking (for teardown checks only).
fn read_nat_refcount() -> u32 {
    let content = match std::fs::read_to_string(NAT_REFCOUNT_FILE) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    content.trim().parse().unwrap_or(0)
}

/// Build the nftables script that (re)installs all current DNAT rules.
///
/// Uses `add table` / `add chain` (idempotent) so it is safe to call even
/// when the table already exists (e.g. because NAT/MASQUERADE is active).
/// `flush chain prerouting` wipes the old rules before rewriting — this is
/// the flush-and-rebuild strategy that avoids needing to track rule handles.
fn build_prerouting_script(entries: &[(Ipv4Addr, u16, u16)]) -> String {
    let mut s = String::from(
        "add table ip remora\n\
         add chain ip remora prerouting { type nat hook prerouting priority -100; }\n\
         flush chain ip remora prerouting\n",
    );
    for (ip, host_port, container_port) in entries {
        s.push_str(&format!(
            "add rule ip remora prerouting tcp dport {} dnat to {}:{}\n",
            host_port, ip, container_port
        ));
    }
    s
}

/// Add port-forward entries to the state file and install nftables DNAT rules.
///
/// Uses `flock(LOCK_EX)` on [`PORT_FORWARDS_FILE`] to serialise concurrent
/// spawns. The `remora0` table / prerouting chain are created idempotently,
/// so this is safe whether NAT is enabled or not.
fn enable_port_forwards(container_ip: Ipv4Addr, forwards: &[(u16, u16)]) -> io::Result<()> {
    std::fs::create_dir_all(REMORA_RUN_DIR)?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(PORT_FORWARDS_FILE)?;

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
    let script = build_prerouting_script(&entries);
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
fn disable_port_forwards(container_ip: Ipv4Addr, forwards: &[(u16, u16)]) {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(PORT_FORWARDS_FILE);

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

    if remaining.is_empty() {
        // No more port forwards — check if NAT is also gone.
        if read_nat_refcount() == 0 {
            // Nothing using the table — remove it entirely.
            if let Err(e) = run_nft("delete table ip remora\n") {
                log::warn!("nft delete table (non-fatal): {}", e);
            }
        } else {
            // NAT still active — flush prerouting chain only.
            let _ = run_nft("flush chain ip remora prerouting\n");
        }
    } else {
        // Rebuild prerouting chain from the surviving entries.
        let script = build_prerouting_script(&remaining);
        if let Err(e) = run_nft(&script) {
            log::warn!("nft rebuild prerouting (non-fatal): {}", e);
        }
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
