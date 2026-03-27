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
    let dir = containers_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
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
