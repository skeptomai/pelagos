//! `pelagos subscribe` — stream container state events as NDJSON.
//!
//! Emits a `Snapshot` immediately, then polls every 250ms and emits
//! `ContainerStarted` / `ContainerExited` diffs.  A `Heartbeat` is sent every
//! 5s when no other events occur.  Exits cleanly on SIGINT or SIGTERM.
//!
//! The NDJSON format matches `GuestEvent` from pelagos-guest exactly
//! (`#[serde(tag = "type", rename_all = "snake_case")]`) so `pelagos-tui`
//! can consume it with zero code changes.

use std::io::Write;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::containers_dir;

// ---------------------------------------------------------------------------
// Wire types (must match GuestEvent / ContainerSnapshot in pelagos-guest)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ContainerSnapshot {
    name: String,
    status: String,
    pid: i32,
    rootfs: String,
    started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    ports: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    Snapshot {
        containers: Vec<ContainerSnapshot>,
        vm_running: bool,
    },
    ContainerStarted {
        container: ContainerSnapshot,
    },
    ContainerExited {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    Heartbeat {
        ts: u64,
    },
}

// ---------------------------------------------------------------------------
// State reader
// ---------------------------------------------------------------------------

fn read_snapshots() -> Vec<ContainerSnapshot> {
    read_snapshots_from(&containers_dir())
}

fn read_snapshots_from(containers_dir: &std::path::Path) -> Vec<ContainerSnapshot> {
    let Ok(entries) = std::fs::read_dir(containers_dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let state_path = entry.path().join("state.json");
        let Ok(data) = std::fs::read_to_string(&state_path) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };

        let name = match val.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };

        let status = val
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let pid = val.get("pid").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

        let rootfs = val
            .get("rootfs")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let started_at = val
            .get("started_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let exit_code = val
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .map(|c| c as i32);

        // ports are nested in spawn_config.publish
        let ports: Vec<String> = val
            .get("spawn_config")
            .and_then(|sc| sc.get("publish"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        out.push(ContainerSnapshot {
            name,
            status,
            pid,
            rootfs,
            started_at,
            exit_code,
            ports,
        });
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

fn diff(prev: &[ContainerSnapshot], current: &[ContainerSnapshot]) -> Vec<Event> {
    let mut events = Vec::new();

    // Appeared or transitioned to running.
    for c in current {
        match prev.iter().find(|p| p.name == c.name) {
            None => {
                events.push(Event::ContainerStarted {
                    container: c.clone(),
                });
                // If it never appeared running, also emit Exited immediately.
                if c.status != "running" {
                    events.push(Event::ContainerExited {
                        name: c.name.clone(),
                        exit_code: c.exit_code,
                    });
                }
            }
            Some(p) if p.status != "running" && c.status == "running" => {
                events.push(Event::ContainerStarted {
                    container: c.clone(),
                });
            }
            _ => {}
        }
    }

    // Disappeared or transitioned away from running.
    for p in prev {
        match current.iter().find(|c| c.name == p.name) {
            None => {
                events.push(Event::ContainerExited {
                    name: p.name.clone(),
                    exit_code: p.exit_code,
                });
            }
            Some(c) if p.status == "running" && c.status != "running" => {
                events.push(Event::ContainerExited {
                    name: c.name.clone(),
                    exit_code: c.exit_code,
                });
            }
            _ => {}
        }
    }

    events
}

// ---------------------------------------------------------------------------
// Emit helper
// ---------------------------------------------------------------------------

fn emit(event: &Event) -> std::io::Result<()> {
    let mut json = serde_json::to_string(event).map_err(std::io::Error::other)?;
    json.push('\n');
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    out.write_all(json.as_bytes())?;
    out.flush()
}

// ---------------------------------------------------------------------------
// Signal handling
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn handle_signal(_: libc::c_int) {
    // async-signal-safe: only stores to an AtomicBool.
    RUNNING.store(false, Ordering::SeqCst);
}

fn install_signal_handler() {
    unsafe {
        let handler = nix::sys::signal::SigHandler::Handler(handle_signal);
        let _ = nix::sys::signal::signal(nix::sys::signal::Signal::SIGINT, handler);
        let _ = nix::sys::signal::signal(nix::sys::signal::Signal::SIGTERM, handler);
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn cmd_subscribe() -> Result<(), Box<dyn std::error::Error>> {
    install_signal_handler();

    // Snapshot on connect.
    let mut prev = read_snapshots();
    emit(&Event::Snapshot {
        containers: prev.clone(),
        vm_running: true,
    })?;

    let heartbeat_interval = Duration::from_secs(5);
    let poll_interval = Duration::from_millis(250);
    let mut last_heartbeat = Instant::now();

    while RUNNING.load(Ordering::SeqCst) {
        std::thread::sleep(poll_interval);

        let current = read_snapshots();
        let events = diff(&prev, &current);
        for e in &events {
            emit(e)?;
        }
        prev = current;

        if last_heartbeat.elapsed() >= heartbeat_interval {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            emit(&Event::Heartbeat { ts })?;
            last_heartbeat = Instant::now();
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot(name: &str, status: &str, pid: i32, rootfs: &str) -> ContainerSnapshot {
        ContainerSnapshot {
            name: name.to_string(),
            status: status.to_string(),
            pid,
            rootfs: rootfs.to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            exit_code: None,
            ports: vec![],
        }
    }

    fn event_json(e: &Event) -> serde_json::Value {
        serde_json::from_str(&serde_json::to_string(e).unwrap()).unwrap()
    }

    // -----------------------------------------------------------------------
    // read_snapshots_from tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_read_snapshots_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = read_snapshots_from(tmp.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_read_snapshots_reads_state() {
        let tmp = tempfile::tempdir().unwrap();
        let ctr_dir = tmp.path().join("mycontainer");
        std::fs::create_dir_all(&ctr_dir).unwrap();
        let state = serde_json::json!({
            "name": "mycontainer",
            "status": "running",
            "pid": 1234,
            "rootfs": "/var/lib/pelagos/rootfs/alpine",
            "started_at": "2026-01-01T00:00:00Z"
        });
        std::fs::write(ctr_dir.join("state.json"), state.to_string()).unwrap();

        let result = read_snapshots_from(tmp.path());
        assert_eq!(result.len(), 1);
        let s = &result[0];
        assert_eq!(s.name, "mycontainer");
        assert_eq!(s.status, "running");
        assert_eq!(s.pid, 1234);
        assert_eq!(s.rootfs, "/var/lib/pelagos/rootfs/alpine");
    }

    #[test]
    fn test_read_snapshots_skips_bad_json() {
        let tmp = tempfile::tempdir().unwrap();
        let ctr_dir = tmp.path().join("broken");
        std::fs::create_dir_all(&ctr_dir).unwrap();
        std::fs::write(ctr_dir.join("state.json"), "not json at all {{{{").unwrap();

        let result = read_snapshots_from(tmp.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_read_snapshots_ports_from_spawn_config() {
        let tmp = tempfile::tempdir().unwrap();
        let ctr_dir = tmp.path().join("webserver");
        std::fs::create_dir_all(&ctr_dir).unwrap();
        let state = serde_json::json!({
            "name": "webserver",
            "status": "running",
            "pid": 42,
            "rootfs": "/rootfs",
            "started_at": "2026-01-01T00:00:00Z",
            "spawn_config": {
                "publish": ["8080:80", "9090:90"]
            }
        });
        std::fs::write(ctr_dir.join("state.json"), state.to_string()).unwrap();

        let result = read_snapshots_from(tmp.path());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].ports, vec!["8080:80", "9090:90"]);
    }

    // -----------------------------------------------------------------------
    // diff tests (via JSON serialization since Event is not pub)
    // -----------------------------------------------------------------------

    #[test]
    fn test_diff_empty() {
        let events = diff(&[], &[]);
        assert!(events.is_empty());
    }

    #[test]
    fn test_diff_new_running_container() {
        let current = vec![make_snapshot("alpha", "running", 100, "/rootfs")];
        let events = diff(&[], &current);
        assert_eq!(events.len(), 1);
        let v = event_json(&events[0]);
        assert_eq!(v["type"], "container_started");
        assert_eq!(v["container"]["name"], "alpha");
    }

    #[test]
    fn test_diff_container_exited() {
        let prev = vec![make_snapshot("beta", "running", 200, "/rootfs")];
        let current = vec![make_snapshot("beta", "exited", 200, "/rootfs")];
        let events = diff(&prev, &current);
        assert_eq!(events.len(), 1);
        let v = event_json(&events[0]);
        assert_eq!(v["type"], "container_exited");
        assert_eq!(v["name"], "beta");
    }

    #[test]
    fn test_diff_new_exited_container() {
        // A container that appears already exited → started then immediately exited.
        let current = vec![make_snapshot("gamma", "exited", 0, "/rootfs")];
        let events = diff(&[], &current);
        assert_eq!(events.len(), 2);
        let types: Vec<String> = events
            .iter()
            .map(|e| event_json(e)["type"].as_str().unwrap().to_string())
            .collect();
        assert!(types.iter().any(|t| t == "container_started"));
        assert!(types.iter().any(|t| t == "container_exited"));
    }

    #[test]
    fn test_diff_container_removed() {
        // Container disappears from the running state.
        let prev = vec![make_snapshot("delta", "running", 300, "/rootfs")];
        let events = diff(&prev, &[]);
        assert_eq!(events.len(), 1);
        let v = event_json(&events[0]);
        assert_eq!(v["type"], "container_exited");
        assert_eq!(v["name"], "delta");
    }
}
