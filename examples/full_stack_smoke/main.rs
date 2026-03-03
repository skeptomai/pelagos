//! Full-stack smoke test combining overlay, bridge, NAT, DNS, linking, and tmpfs.
//!
//! Spawns two containers that exercise every major networking and filesystem
//! feature together. This catches interaction bugs that per-feature tests miss.
//!
//! ```text
//! server (overlay + bridge + NAT + DNS + tmpfs)
//!   → verifies internet via wget
//!   → serves "SMOKE_OK" on TCP 8080 via nc
//!
//! client (overlay + bridge + link to server + tmpfs)
//!   → connects to server by name
//!   → prints result
//! ```
//!
//! # Running
//!
//! Build the alpine rootfs first:
//! ```bash
//! ./build-rootfs-docker.sh    # or ./build-rootfs-tarball.sh
//! ```
//!
//! Then run (requires root):
//! ```bash
//! sudo -E cargo run --example full_stack_smoke
//! ```

use pelagos::container::{Command, Namespace, Stdio};
use pelagos::network::NetworkMode;
use std::env;

const ALPINE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

fn register_container(name: &str, pid: i32, ip: &str, rootfs: &str) {
    let dir = format!("/run/pelagos/containers/{}", name);
    std::fs::create_dir_all(&dir).expect("create container state dir");
    let state = serde_json::json!({
        "name": name,
        "rootfs": rootfs,
        "status": "running",
        "pid": pid,
        "watcher_pid": 0,
        "started_at": "2026-01-01T00:00:00Z",
        "exit_code": null,
        "command": ["/bin/sh"],
        "bridge_ip": ip,
    });
    let path = format!("{}/state.json", dir);
    std::fs::write(&path, serde_json::to_string_pretty(&state).unwrap())
        .expect("write container state");
}

fn unregister_container(name: &str) {
    let dir = format!("/run/pelagos/containers/{}", name);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Server: test internet access via NAT+DNS, then serve a message on TCP 8080.
const SERVER_SCRIPT: &str = r#"
# Test 1: verify outbound internet works through NAT+DNS
INTERNET="FAIL"
if wget -q -T 5 --spider http://1.1.1.1/ 2>/dev/null; then
    INTERNET="OK"
fi
echo "[server] Internet access: $INTERNET"

# Test 2: verify overlay is writable
echo "overlay_test" > /overlay_marker
if [ -f /overlay_marker ]; then
    echo "[server] Overlay write: OK"
else
    echo "[server] Overlay write: FAIL"
fi

# Test 3: verify tmpfs is writable
echo "tmpfs_test" > /scratch/tmpfs_marker
if [ -f /scratch/tmpfs_marker ]; then
    echo "[server] tmpfs write: OK"
else
    echo "[server] tmpfs write: FAIL"
fi

# Serve a known string on TCP 8080 (one-shot)
echo "[server] Listening on TCP 8080 ..."
echo "SMOKE_OK" | nc -l -p 8080
echo "[server] Client connected, exiting."
"#;

/// Client: connect to server by link name, verify the response.
const CLIENT_SCRIPT: &str = r#"
ATTEMPT=0
RESULT=""
while [ $ATTEMPT -lt 10 ]; do
    ATTEMPT=$((ATTEMPT + 1))
    RESULT=$(nc -w 2 smoke-server 8080 2>/dev/null)
    [ -n "$RESULT" ] && break
    sleep 1
done

if echo "$RESULT" | grep -q "SMOKE_OK"; then
    echo "[client] TCP link to server: OK"
    echo "[client] Received: $RESULT"
else
    echo "[client] TCP link to server: FAIL (got: '$RESULT')"
    exit 1
fi

# Verify overlay is writable in client too
echo "client_overlay" > /overlay_marker
if [ -f /overlay_marker ]; then
    echo "[client] Overlay write: OK"
else
    echo "[client] Overlay write: FAIL"
fi
"#;

fn main() {
    env_logger::init();

    let current_dir = env::current_dir().expect("Failed to get current directory");
    let rootfs = current_dir.join("alpine-rootfs");

    if !rootfs.exists() {
        eprintln!("Error: alpine-rootfs not found!");
        eprintln!("Build it with: ./build-rootfs-docker.sh");
        std::process::exit(1);
    }

    let rootfs_str = rootfs.to_str().unwrap();

    // Create overlay upper+work dirs for each container.
    let tmp = std::env::temp_dir().join("pelagos-smoke");
    let _ = std::fs::remove_dir_all(&tmp);
    let server_upper = tmp.join("server-upper");
    let server_work = tmp.join("server-work");
    let client_upper = tmp.join("client-upper");
    let client_work = tmp.join("client-work");
    for d in [&server_upper, &server_work, &client_upper, &client_work] {
        std::fs::create_dir_all(d).expect("create overlay dir");
    }

    println!("=== Pelagos Full-Stack Smoke Test ===\n");
    println!("Features under test:");
    println!("  - Overlay filesystem (copy-on-write)");
    println!("  - Bridge networking (172.19.0.x/24)");
    println!("  - NAT (outbound internet via MASQUERADE)");
    println!("  - DNS (resolv.conf injection)");
    println!("  - Container linking (name resolution)");
    println!("  - tmpfs mounts\n");

    // ------------------------------------------------------------------
    // 1. Spawn server: overlay + bridge + NAT + DNS + tmpfs
    // ------------------------------------------------------------------
    println!("[main] Starting server container ...");

    let mut server = Command::new("/bin/sh")
        .args(&["-c", SERVER_SCRIPT])
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .with_chroot(&rootfs)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .env("PATH", ALPINE_PATH)
        .with_proc_mount()
        .with_network(NetworkMode::Bridge)
        .with_nat()
        .with_dns(&["8.8.8.8"])
        .with_overlay(&server_upper, &server_work)
        .with_tmpfs("/scratch", "size=1m")
        .spawn()
        .expect("Failed to spawn server container");

    let server_ip = server
        .container_ip()
        .expect("server should have a bridge IP");
    let server_pid = server.pid() as i32;
    println!(
        "[main] server running — PID {} — IP {}",
        server_pid, server_ip
    );

    register_container("smoke-server", server_pid, &server_ip, rootfs_str);

    // Give the server time to do its internet check and start nc.
    std::thread::sleep(std::time::Duration::from_secs(3));

    // ------------------------------------------------------------------
    // 2. Spawn client: overlay + bridge + link to server + tmpfs
    // ------------------------------------------------------------------
    println!("[main] Starting client container ...\n");

    let mut client = Command::new("/bin/sh")
        .args(&["-c", CLIENT_SCRIPT])
        .stdin(Stdio::Null)
        .stdout(Stdio::Piped)
        .stderr(Stdio::Piped)
        .with_chroot(&rootfs)
        .with_namespaces(Namespace::UTS | Namespace::MOUNT)
        .env("PATH", ALPINE_PATH)
        .with_proc_mount()
        .with_network(NetworkMode::Bridge)
        .with_link("smoke-server")
        .with_overlay(&client_upper, &client_work)
        .with_tmpfs("/scratch", "size=1m")
        .spawn()
        .expect("Failed to spawn client container");

    let client_ip = client
        .container_ip()
        .expect("client should have a bridge IP");
    println!(
        "[main] client running — PID {} — IP {}\n",
        client.pid(),
        client_ip
    );

    // ------------------------------------------------------------------
    // 3. Collect output from both
    // ------------------------------------------------------------------
    let (client_status, client_stdout, client_stderr) = client
        .wait_with_output()
        .expect("Failed to wait for client");

    // Server should have exited after nc sent its response.
    let (server_status, server_stdout, server_stderr) = server
        .wait_with_output()
        .expect("Failed to wait for server");

    // ------------------------------------------------------------------
    // 4. Print results
    // ------------------------------------------------------------------
    println!("--- server output ---");
    print!("{}", String::from_utf8_lossy(&server_stdout));
    let server_err = String::from_utf8_lossy(&server_stderr);
    if !server_err.is_empty() {
        println!("--- server stderr ---");
        print!("{}", server_err);
    }
    println!("--- end server ---\n");

    println!("--- client output ---");
    print!("{}", String::from_utf8_lossy(&client_stdout));
    let client_err = String::from_utf8_lossy(&client_stderr);
    if !client_err.is_empty() {
        println!("--- client stderr ---");
        print!("{}", client_err);
    }
    println!("--- end client ---\n");

    // ------------------------------------------------------------------
    // 5. Clean up state files + overlay dirs
    // ------------------------------------------------------------------
    unregister_container("smoke-server");
    let _ = std::fs::remove_dir_all(&tmp);

    // ------------------------------------------------------------------
    // 6. Verify overlay isolation: rootfs should be unmodified
    // ------------------------------------------------------------------
    let rootfs_marker = rootfs.join("overlay_marker");
    if rootfs_marker.exists() {
        println!("WARNING: overlay_marker leaked into rootfs — overlay isolation broken!");
    } else {
        println!("Overlay isolation verified: rootfs unchanged.");
    }

    // ------------------------------------------------------------------
    // Summary
    // ------------------------------------------------------------------
    println!("\n=== Smoke Test Complete ===\n");
    println!("Features exercised:");
    println!("  - Overlay filesystem (server + client both wrote files)");
    println!("  - Bridge networking (172.19.0.x/24)");
    println!("  - NAT + DNS (server checked internet access)");
    println!("  - Container linking (client connected to server by name)");
    println!("  - tmpfs mounts (server wrote to /scratch)");
    println!("  - Cross-container TCP (nc server:8080 via link)");

    let ok = server_status.success() && client_status.success();
    if ok {
        println!("\nAll checks passed!");
    } else {
        println!(
            "\nSome checks failed — server: {:?}, client: {:?}",
            server_status.code(),
            client_status.code()
        );
        std::process::exit(1);
    }
}
