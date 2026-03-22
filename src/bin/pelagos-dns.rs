//! `pelagos-dns` — lightweight DNS daemon for container name resolution.
//!
//! Usage: `pelagos-dns --config-dir <dir>`
//!
//! Listens on gateway IPs from per-network config files, resolves container
//! names to their bridge IPs, and forwards unknown queries to upstream DNS.
//! Reloads configuration on SIGHUP. Auto-exits when all config files are empty.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ── DNS wire format constants ────────────────────────────────────────────────

const DNS_TYPE_A: u16 = 1;
const DNS_CLASS_IN: u16 = 1;
const DNS_FLAG_QR: u16 = 0x8000; // Response
const DNS_FLAG_AA: u16 = 0x0400; // Authoritative
const DNS_FLAG_RD: u16 = 0x0100; // Recursion desired
const DNS_FLAG_RA: u16 = 0x0080; // Recursion available
const DNS_RCODE_NXDOMAIN: u16 = 3;
const DNS_RCODE_SERVFAIL: u16 = 2;

// ── DNS packet types ─────────────────────────────────────────────────────────

struct DnsQuery {
    id: u16,
    qname: String,
    qtype: u16,
    qclass: u16,
    raw: Vec<u8>,
}

/// Parse a DNS query packet. Returns `None` if malformed.
fn parse_dns_query(buf: &[u8]) -> Option<DnsQuery> {
    if buf.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([buf[0], buf[1]]);
    let flags = u16::from_be_bytes([buf[2], buf[3]]);

    // Must be a query (QR=0)
    if flags & DNS_FLAG_QR != 0 {
        return None;
    }

    let qdcount = u16::from_be_bytes([buf[4], buf[5]]);
    if qdcount < 1 {
        return None;
    }

    // Parse QNAME (sequence of length-prefixed labels)
    let (qname, pos) = parse_qname(buf, 12)?;

    // Need at least 4 more bytes for QTYPE + QCLASS
    if pos + 4 > buf.len() {
        return None;
    }
    let qtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
    let qclass = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]);

    Some(DnsQuery {
        id,
        qname,
        qtype,
        qclass,
        raw: buf.to_vec(),
    })
}

/// Parse a DNS QNAME starting at `offset`. Returns (name, next_offset).
fn parse_qname(buf: &[u8], mut offset: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    loop {
        if offset >= buf.len() {
            return None;
        }
        let len = buf[offset] as usize;
        if len == 0 {
            offset += 1;
            break;
        }
        // Reject compression pointers — we only handle simple queries
        if len & 0xC0 != 0 {
            return None;
        }
        offset += 1;
        if offset + len > buf.len() {
            return None;
        }
        let label = std::str::from_utf8(&buf[offset..offset + len]).ok()?;
        labels.push(label.to_ascii_lowercase());
        offset += len;
    }
    Some((labels.join("."), offset))
}

/// Build a DNS A-record response for the given query and IP.
fn build_a_response(query: &DnsQuery, ip: Ipv4Addr) -> Vec<u8> {
    let mut resp = Vec::with_capacity(64);

    // Header
    resp.extend_from_slice(&query.id.to_be_bytes());
    let flags: u16 = DNS_FLAG_QR | DNS_FLAG_AA | DNS_FLAG_RD | DNS_FLAG_RA;
    resp.extend_from_slice(&flags.to_be_bytes());
    resp.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    resp.extend_from_slice(&1u16.to_be_bytes()); // ANCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT

    // Question section (echo back)
    encode_qname(&mut resp, &query.qname);
    resp.extend_from_slice(&query.qtype.to_be_bytes());
    resp.extend_from_slice(&query.qclass.to_be_bytes());

    // Answer section — pointer to QNAME at offset 12
    resp.extend_from_slice(&[0xC0, 0x0C]); // Name pointer
    resp.extend_from_slice(&DNS_TYPE_A.to_be_bytes());
    resp.extend_from_slice(&DNS_CLASS_IN.to_be_bytes());
    resp.extend_from_slice(&10u32.to_be_bytes()); // TTL = 10 seconds
    resp.extend_from_slice(&4u16.to_be_bytes()); // RDLENGTH
    resp.extend_from_slice(&ip.octets());

    resp
}

/// Build a NODATA response (NOERROR, zero answers).
///
/// Used when the name exists but has no records of the requested type.
/// This is distinct from NXDOMAIN which asserts the name itself doesn't exist.
fn build_nodata(query: &DnsQuery) -> Vec<u8> {
    let mut resp = Vec::with_capacity(32);

    // Header — rcode=0 (NOERROR), AA=1, zero answers
    resp.extend_from_slice(&query.id.to_be_bytes());
    let flags: u16 = DNS_FLAG_QR | DNS_FLAG_AA | DNS_FLAG_RD | DNS_FLAG_RA;
    resp.extend_from_slice(&flags.to_be_bytes());
    resp.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT

    // Question section (echo back)
    encode_qname(&mut resp, &query.qname);
    resp.extend_from_slice(&query.qtype.to_be_bytes());
    resp.extend_from_slice(&query.qclass.to_be_bytes());

    resp
}

/// Build a SERVFAIL response. Used when all upstream servers fail to respond.
/// Sending SERVFAIL (rcode=2) immediately lets the client fail fast rather than
/// waiting for its own retry timeout (up to 30 s for nslookup).
fn build_servfail(query: &DnsQuery) -> Vec<u8> {
    let mut resp = Vec::with_capacity(32);
    resp.extend_from_slice(&query.id.to_be_bytes());
    let flags: u16 = DNS_FLAG_QR | DNS_FLAG_RD | DNS_FLAG_RA | DNS_RCODE_SERVFAIL;
    resp.extend_from_slice(&flags.to_be_bytes());
    resp.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    encode_qname(&mut resp, &query.qname);
    resp.extend_from_slice(&query.qtype.to_be_bytes());
    resp.extend_from_slice(&query.qclass.to_be_bytes());
    resp
}

/// Build an NXDOMAIN response.
fn build_nxdomain(query: &DnsQuery) -> Vec<u8> {
    let mut resp = Vec::with_capacity(32);

    // Header
    resp.extend_from_slice(&query.id.to_be_bytes());
    let flags: u16 = DNS_FLAG_QR | DNS_FLAG_AA | DNS_FLAG_RD | DNS_FLAG_RA | DNS_RCODE_NXDOMAIN;
    resp.extend_from_slice(&flags.to_be_bytes());
    resp.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT

    // Question section (echo back)
    encode_qname(&mut resp, &query.qname);
    resp.extend_from_slice(&query.qtype.to_be_bytes());
    resp.extend_from_slice(&query.qclass.to_be_bytes());

    resp
}

/// Encode a domain name into DNS wire format (length-prefixed labels).
fn encode_qname(buf: &mut Vec<u8>, name: &str) {
    for label in name.split('.') {
        if label.is_empty() {
            continue;
        }
        buf.push(label.len() as u8);
        buf.extend_from_slice(label.as_bytes());
    }
    buf.push(0); // Root label
}

// ── Per-network config ───────────────────────────────────────────────────────

struct NetworkConfig {
    /// Gateway IP this network's DNS listens on.
    listen_ip: Ipv4Addr,
    /// Upstream DNS servers for forwarding.
    upstream: Vec<Ipv4Addr>,
    /// Container name → IP mappings.
    entries: HashMap<String, Ipv4Addr>,
}

/// Parse a network config file. Returns `None` if empty or malformed.
///
/// Format:
/// ```text
/// <gateway_ip> <upstream1>,<upstream2>
/// <container_name> <container_ip>
/// ...
/// ```
fn parse_network_config(content: &str) -> Option<NetworkConfig> {
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());

    let header = lines.next()?;
    let mut parts = header.split_whitespace();
    let listen_ip: Ipv4Addr = parts.next()?.parse().ok()?;
    let upstream_str = parts.next().unwrap_or("8.8.8.8,1.1.1.1");
    let upstream: Vec<Ipv4Addr> = upstream_str
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();

    let mut entries = HashMap::new();
    for line in lines {
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(ip_str)) = (parts.next(), parts.next()) {
            if let Ok(ip) = ip_str.parse::<Ipv4Addr>() {
                entries.insert(name.to_string(), ip);
            }
        }
    }

    Some(NetworkConfig {
        listen_ip,
        upstream,
        entries,
    })
}

// ── Server state ─────────────────────────────────────────────────────────────

struct ListenSocket {
    socket: UdpSocket,
    gateway_ip: Ipv4Addr,
}

struct ServerState {
    config_dir: PathBuf,
    /// Per-network configs, keyed by network name.
    configs: HashMap<String, NetworkConfig>,
    /// Active listening sockets.
    sockets: Vec<ListenSocket>,
}

impl ServerState {
    fn new(config_dir: PathBuf) -> Self {
        ServerState {
            config_dir,
            configs: HashMap::new(),
            sockets: Vec::new(),
        }
    }

    /// Load all network config files and (re)bind sockets.
    fn reload(&mut self) {
        self.configs.clear();

        let entries = match std::fs::read_dir(&self.config_dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            // Skip the PID file and non-files
            if !path.is_file() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) if n != "pid" => n.to_string(),
                _ => continue,
            };

            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Some(config) = parse_network_config(&content) {
                    if !config.entries.is_empty() {
                        self.configs.insert(name, config);
                    }
                }
            }
        }

        // Rebind sockets: close old ones and open new ones for current configs.
        self.rebind_sockets();
    }

    /// Rebind UDP sockets to match current config listen IPs.
    fn rebind_sockets(&mut self) {
        // Collect desired listen addresses.
        let desired: HashMap<Ipv4Addr, &str> = self
            .configs
            .iter()
            .map(|(name, cfg)| (cfg.listen_ip, name.as_str()))
            .collect();

        // Remove sockets for IPs no longer needed.
        self.sockets.retain(|s| desired.contains_key(&s.gateway_ip));

        // Determine which IPs already have sockets.
        let bound: Vec<Ipv4Addr> = self.sockets.iter().map(|s| s.gateway_ip).collect();

        // Bind new sockets for IPs not yet bound.
        for &ip in desired.keys() {
            if bound.contains(&ip) {
                continue;
            }

            let bind_addr = SocketAddr::new(std::net::IpAddr::V4(ip), 53);
            match UdpSocket::bind(bind_addr) {
                Ok(sock) => {
                    let _ = sock.set_nonblocking(true);
                    self.sockets.push(ListenSocket {
                        socket: sock,
                        gateway_ip: ip,
                    });
                    eprintln!("pelagos-dns: listening on {}", bind_addr);
                }
                Err(e) => {
                    eprintln!("pelagos-dns: failed to bind {}: {}", bind_addr, e);
                }
            }
        }
    }

    /// Returns true if there are any container entries across all configs.
    fn has_entries(&self) -> bool {
        self.configs.values().any(|c| !c.entries.is_empty())
    }

    /// Look up a container name across all configs served by a given gateway IP.
    ///
    /// When multiple named networks share the same gateway (e.g. two compose projects
    /// both using 10.91.0.0/24), a single UDP socket listens on that gateway IP.
    /// Searching all matching configs ensures entries from any of those networks are
    /// found rather than returning NXDOMAIN because the HashMap in rebind_sockets
    /// collapsed them to one network name.
    fn lookup(&self, gateway: Ipv4Addr, name: &str) -> Option<Ipv4Addr> {
        self.configs
            .values()
            .filter(|cfg| cfg.listen_ip == gateway)
            .find_map(|cfg| cfg.entries.get(name).copied())
    }

    /// Get upstream servers for a gateway IP (first matching network).
    fn upstream(&self, gateway: Ipv4Addr) -> Vec<Ipv4Addr> {
        self.configs
            .values()
            .find(|cfg| cfg.listen_ip == gateway)
            .map(|c| c.upstream.clone())
            .unwrap_or_default()
    }
}

// ── Upstream forwarding ──────────────────────────────────────────────────────

/// Forward a raw DNS packet to upstream servers. Returns the response or None.
fn forward_upstream(raw: &[u8], upstream: &[Ipv4Addr]) -> Option<Vec<u8>> {
    for &server in upstream {
        let addr = SocketAddr::new(std::net::IpAddr::V4(server), 53);
        let sock = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(_) => continue,
        };
        let _ = sock.set_read_timeout(Some(Duration::from_secs(3)));

        if sock.send_to(raw, addr).is_err() {
            continue;
        }

        let mut buf = [0u8; 4096];
        // Retry on EINTR (e.g. SIGHUP delivered during the blocking recv).
        let result = loop {
            match sock.recv_from(&mut buf) {
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                other => break other,
            }
        };
        match result {
            Ok((n, _)) => return Some(buf[..n].to_vec()),
            Err(_) => continue,
        }
    }
    None
}

// ── SIGHUP handling ──────────────────────────────────────────────────────────

fn install_sighup_handler(flag: Arc<AtomicBool>) {
    // Use a pipe to signal from the signal handler to the main loop.
    // The flag is set in the handler; the main loop checks it periodically.
    unsafe {
        // Store the flag pointer in a static for the signal handler.
        RELOAD_FLAG.store(Arc::into_raw(flag) as *mut bool as usize, Ordering::SeqCst);
        libc::signal(
            libc::SIGHUP,
            sighup_handler as *const () as libc::sighandler_t,
        );
    }
}

static RELOAD_FLAG: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn sighup_handler(_sig: libc::c_int) {
    let ptr = RELOAD_FLAG.load(Ordering::SeqCst);
    if ptr != 0 {
        let flag = unsafe { &*(ptr as *const AtomicBool) };
        flag.store(true, Ordering::SeqCst);
    }
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let config_dir = if let Some(pos) = args.iter().position(|a| a == "--config-dir") {
        args.get(pos + 1).map(PathBuf::from).unwrap_or_else(|| {
            eprintln!("pelagos-dns: --config-dir requires a path argument");
            std::process::exit(1);
        })
    } else {
        eprintln!("Usage: pelagos-dns --config-dir <dir>");
        std::process::exit(1);
    };

    // Write PID file.
    let pid_file = config_dir.join("pid");
    if let Err(e) = std::fs::write(&pid_file, format!("{}", unsafe { libc::getpid() })) {
        eprintln!("pelagos-dns: failed to write PID file: {}", e);
        std::process::exit(1);
    }

    // Install SIGHUP handler.
    let reload_flag = Arc::new(AtomicBool::new(false));
    install_sighup_handler(reload_flag.clone());

    // Initial config load.
    let mut state = ServerState::new(config_dir.clone());
    state.reload();

    if !state.has_entries() {
        eprintln!("pelagos-dns: no entries found, exiting");
        let _ = std::fs::remove_file(&pid_file);
        return;
    }

    eprintln!(
        "pelagos-dns: started with {} network(s)",
        state.configs.len()
    );

    let mut buf = [0u8; 4096];

    loop {
        // Check reload flag.
        if reload_flag.swap(false, Ordering::SeqCst) {
            state.reload();
            if !state.has_entries() {
                eprintln!("pelagos-dns: no entries remaining, exiting");
                break;
            }
            eprintln!("pelagos-dns: reloaded, {} network(s)", state.configs.len());
        }

        // Poll all sockets with a short timeout.
        let mut activity = false;
        // We iterate by index since we need shared access to state.
        for i in 0..state.sockets.len() {
            let recv = state.sockets[i].socket.recv_from(&mut buf);
            match recv {
                Ok((n, src)) => {
                    activity = true;
                    let gateway_ip = state.sockets[i].gateway_ip;
                    if let Some(response) = handle_query(&buf[..n], gateway_ip, &state) {
                        let _ = state.sockets[i].socket.send_to(&response, src);
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => {}
            }
        }

        if !activity {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // Cleanup PID file.
    let _ = std::fs::remove_file(&pid_file);
    eprintln!("pelagos-dns: stopped");
}

/// Handle a DNS query: resolve locally or forward upstream.
fn handle_query(packet: &[u8], gateway: Ipv4Addr, state: &ServerState) -> Option<Vec<u8>> {
    let query = parse_dns_query(packet)?;

    // Strip `.pelagos` suffix if present.
    let name = query.qname.strip_suffix(".pelagos").unwrap_or(&query.qname);

    // Strip any trailing dot.
    let name = name.strip_suffix('.').unwrap_or(name);

    // For bare names (no dots), handle entirely locally — never forward upstream.
    // This prevents blocking the main loop on AAAA queries for container names.
    if !name.contains('.') {
        if query.qtype == DNS_TYPE_A && query.qclass == DNS_CLASS_IN {
            if let Some(ip) = state.lookup(gateway, name) {
                return Some(build_a_response(&query, ip));
            }
        }
        // Name exists but has no record of the requested type (e.g. AAAA)?
        // Return NODATA (NOERROR + zero answers) — NOT NXDOMAIN.
        // NXDOMAIN means "name does not exist" which is a domain-level assertion.
        // If we return NXDOMAIN for the AAAA query while the A query succeeds,
        // parallel resolvers (like musl libc) may treat the NXDOMAIN as
        // authoritative proof that the name doesn't exist, discarding the A result.
        if state.lookup(gateway, name).is_some() {
            return Some(build_nodata(&query));
        }
        return Some(build_nxdomain(&query));
    }

    // Dotted name — try local lookup for A queries first.
    if query.qtype == DNS_TYPE_A && query.qclass == DNS_CLASS_IN {
        if let Some(ip) = state.lookup(gateway, name) {
            return Some(build_a_response(&query, ip));
        }
    }

    // Forward to upstream DNS.
    let upstream = state.upstream(gateway);
    if upstream.is_empty() {
        return Some(build_nxdomain(&query));
    }

    // Return SERVFAIL if all upstreams time out, so clients fail fast rather
    // than waiting for their own retry timeout (up to 30 s for nslookup).
    Some(forward_upstream(&query.raw, &upstream).unwrap_or_else(|| build_servfail(&query)))
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_a_query(name: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        // Header: ID=0x1234, flags=RD, QDCOUNT=1
        buf.extend_from_slice(&[0x12, 0x34]); // ID
        buf.extend_from_slice(&[0x01, 0x00]); // Flags: RD=1
        buf.extend_from_slice(&[0x00, 0x01]); // QDCOUNT
        buf.extend_from_slice(&[0x00, 0x00]); // ANCOUNT
        buf.extend_from_slice(&[0x00, 0x00]); // NSCOUNT
        buf.extend_from_slice(&[0x00, 0x00]); // ARCOUNT
                                              // QNAME
        encode_qname(&mut buf, name);
        // QTYPE = A, QCLASS = IN
        buf.extend_from_slice(&DNS_TYPE_A.to_be_bytes());
        buf.extend_from_slice(&DNS_CLASS_IN.to_be_bytes());
        buf
    }

    #[test]
    fn test_parse_valid_a_query() {
        let packet = make_a_query("mycontainer");
        let query = parse_dns_query(&packet).expect("should parse");
        assert_eq!(query.id, 0x1234);
        assert_eq!(query.qname, "mycontainer");
        assert_eq!(query.qtype, DNS_TYPE_A);
        assert_eq!(query.qclass, DNS_CLASS_IN);
    }

    #[test]
    fn test_parse_dotted_name() {
        let packet = make_a_query("mycontainer.pelagos");
        let query = parse_dns_query(&packet).expect("should parse");
        assert_eq!(query.qname, "mycontainer.pelagos");
    }

    #[test]
    fn test_parse_rejects_truncated() {
        assert!(parse_dns_query(&[0; 5]).is_none());
        assert!(parse_dns_query(&[0; 12]).is_none()); // Valid header but no question
    }

    #[test]
    fn test_parse_rejects_response() {
        let mut packet = make_a_query("test");
        // Set QR bit (response)
        packet[2] |= 0x80;
        assert!(parse_dns_query(&packet).is_none());
    }

    #[test]
    fn test_build_a_response() {
        let packet = make_a_query("redis");
        let query = parse_dns_query(&packet).unwrap();
        let response = build_a_response(&query, Ipv4Addr::new(172, 19, 0, 5));

        // Verify header
        assert_eq!(response[0], 0x12); // ID high
        assert_eq!(response[1], 0x34); // ID low
        assert_eq!(response[2] & 0x80, 0x80); // QR=1 (response)
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 1); // ANCOUNT=1

        // Verify the IP is in the response (last 4 bytes of answer)
        let len = response.len();
        assert_eq!(&response[len - 4..], &[172, 19, 0, 5]);
    }

    #[test]
    fn test_build_nodata() {
        let packet = make_a_query("app");
        let query = parse_dns_query(&packet).unwrap();
        let response = build_nodata(&query);

        // Verify RCODE = 0 (NOERROR) — name exists but no records of this type
        let flags = u16::from_be_bytes([response[2], response[3]]);
        assert_eq!(flags & 0x000F, 0); // NOERROR
        assert!(flags & DNS_FLAG_AA != 0); // Authoritative
                                           // ANCOUNT should be 0
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 0);
    }

    #[test]
    fn test_build_nxdomain() {
        let packet = make_a_query("nonexistent");
        let query = parse_dns_query(&packet).unwrap();
        let response = build_nxdomain(&query);

        // Verify RCODE = 3 (NXDOMAIN)
        let flags = u16::from_be_bytes([response[2], response[3]]);
        assert_eq!(flags & 0x000F, DNS_RCODE_NXDOMAIN);
        // ANCOUNT should be 0
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 0);
    }

    #[test]
    fn test_parse_qname_labels() {
        // Manually encode "app.pelagos": [3]app[7]pelagos[0]
        let mut buf = Vec::new();
        buf.push(3);
        buf.extend_from_slice(b"app");
        buf.push(7);
        buf.extend_from_slice(b"pelagos");
        buf.push(0);

        let (name, pos) = parse_qname(&buf, 0).expect("should parse");
        assert_eq!(name, "app.pelagos");
        assert_eq!(pos, buf.len());
    }

    #[test]
    fn test_config_parse_roundtrip() {
        let content = "\
172.19.0.1 8.8.8.8,1.1.1.1
redis 172.19.0.2
app 172.19.0.3
proxy 172.19.0.4
";
        let config = parse_network_config(content).expect("should parse");
        assert_eq!(config.listen_ip, Ipv4Addr::new(172, 19, 0, 1));
        assert_eq!(config.upstream.len(), 2);
        assert_eq!(config.upstream[0], Ipv4Addr::new(8, 8, 8, 8));
        assert_eq!(config.upstream[1], Ipv4Addr::new(1, 1, 1, 1));
        assert_eq!(config.entries.len(), 3);
        assert_eq!(config.entries["redis"], Ipv4Addr::new(172, 19, 0, 2));
        assert_eq!(config.entries["app"], Ipv4Addr::new(172, 19, 0, 3));
        assert_eq!(config.entries["proxy"], Ipv4Addr::new(172, 19, 0, 4));
    }

    #[test]
    fn test_config_parse_empty() {
        assert!(parse_network_config("").is_none());
        assert!(parse_network_config("   \n  ").is_none());
    }

    #[test]
    fn test_config_parse_header_only() {
        let config = parse_network_config("172.19.0.1 8.8.8.8\n");
        // Header only, no entries — returns Some but with empty entries
        let config = config.expect("should parse header");
        assert!(config.entries.is_empty());
    }
}
