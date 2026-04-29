//! Shared daemon state: in-memory exec sessions + pending container configs.

use crate::types::{ExecSession, PendingContainer};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const PENDING_DIR: &str = "/run/pelagos-dockerd/pending";

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    exec_sessions: Mutex<HashMap<String, ExecSession>>,
    completed_execs: Mutex<HashMap<String, i64>>,
    pub pelagos_bin: String,
}

impl AppState {
    pub fn new() -> Self {
        Self::new_with_bin("pelagos".to_string())
    }

    pub fn new_with_bin(pelagos_bin: String) -> Self {
        let _ = std::fs::create_dir_all(PENDING_DIR);
        Self {
            inner: Arc::new(Inner {
                exec_sessions: Mutex::new(HashMap::new()),
                completed_execs: Mutex::new(HashMap::new()),
                pelagos_bin,
            }),
        }
    }

    pub fn pelagos_bin(&self) -> &str {
        &self.inner.pelagos_bin
    }

    pub async fn add_exec(&self, id: String, session: ExecSession) {
        self.inner.exec_sessions.lock().await.insert(id, session);
    }

    pub async fn get_exec(&self, id: &str) -> Option<ExecSession> {
        self.inner.exec_sessions.lock().await.get(id).cloned()
    }

    pub async fn remove_exec(&self, id: &str) {
        self.inner.exec_sessions.lock().await.remove(id);
    }

    pub async fn complete_exec(&self, id: String, exit_code: i64) {
        self.inner.exec_sessions.lock().await.remove(&id);
        self.inner.completed_execs.lock().await.insert(id, exit_code);
    }

    pub async fn get_completed_exec(&self, id: &str) -> Option<i64> {
        self.inner.completed_execs.lock().await.get(id).copied()
    }
}

// ── Pending container persistence ───────────────────────────────────────────

pub fn save_pending(c: &PendingContainer) -> std::io::Result<()> {
    let _ = std::fs::create_dir_all(PENDING_DIR);
    let path = format!("{}/{}.json", PENDING_DIR, c.name);
    let json = serde_json::to_string(c)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

pub fn load_pending(name: &str) -> std::io::Result<PendingContainer> {
    let path = format!("{}/{}.json", PENDING_DIR, name);
    let data = std::fs::read_to_string(&path)?;
    serde_json::from_str(&data).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn remove_pending(name: &str) {
    let _ = std::fs::remove_file(format!("{}/{}.json", PENDING_DIR, name));
}

pub fn list_pending() -> Vec<PendingContainer> {
    let Ok(entries) = std::fs::read_dir(PENDING_DIR) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|e| {
            std::fs::read_to_string(e.path())
                .ok()
                .and_then(|d| serde_json::from_str(&d).ok())
        })
        .collect()
}
