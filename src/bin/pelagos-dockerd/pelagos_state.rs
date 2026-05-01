//! Read pelagos container state from disk and invoke the pelagos CLI.

use serde::Deserialize;
use std::collections::HashMap;
use tokio::process::Command;

const CONTAINERS_DIR: &str = "/run/pelagos/containers";

// ── ContainerState (subset) ─────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ContainerState {
    pub name: String,
    #[serde(rename = "status")]
    pub status: ContainerStatus,
    pub pid: i32,
    pub started_at: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub command: Vec<String>,
    #[serde(default)]
    pub stdout_log: Option<String>,
    #[serde(default)]
    pub stderr_log: Option<String>,
    #[serde(default)]
    pub bridge_ip: Option<String>,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default)]
    pub spawn_config: Option<SpawnConfig>,
    #[serde(default)]
    pub network_ns_name: Option<String>,
    #[serde(default)]
    pub cgroup_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerStatus {
    Running,
    Exited,
    #[serde(other)]
    Unknown,
}

impl ContainerState {
    pub fn is_running(&self) -> bool {
        self.status == ContainerStatus::Running
    }

    pub fn docker_status_str(&self) -> &str {
        match self.status {
            ContainerStatus::Running => "running",
            ContainerStatus::Exited => "exited",
            ContainerStatus::Unknown => "dead",
        }
    }

    pub fn image(&self) -> &str {
        self.spawn_config
            .as_ref()
            .and_then(|sc| sc.image.as_deref())
            .unwrap_or("")
    }

    pub fn network_mode(&self) -> &str {
        self.spawn_config
            .as_ref()
            .and_then(|sc| sc.network.first())
            .map(|s| s.as_str())
            .unwrap_or("bridge")
    }

    pub fn env(&self) -> Vec<String> {
        self.spawn_config
            .as_ref()
            .map(|sc| sc.env.clone())
            .unwrap_or_default()
    }

    pub fn binds(&self) -> Vec<String> {
        self.spawn_config
            .as_ref()
            .map(|sc| {
                let mut b = sc.bind.clone();
                b.extend(sc.bind_ro.iter().map(|s| format!("{}:ro", s)));
                b
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct SpawnConfig {
    #[serde(default)]
    pub image: Option<String>,
    pub exe: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub bind: Vec<String>,
    #[serde(default)]
    pub bind_ro: Vec<String>,
    #[serde(default)]
    pub network: Vec<String>,
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

// ── State file I/O ───────────────────────────────────────────────────────────

pub fn read_state(name: &str) -> std::io::Result<ContainerState> {
    let path = format!("{}/{}/state.json", CONTAINERS_DIR, name);
    let data = std::fs::read_to_string(&path)?;
    serde_json::from_str(&data).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn list_states() -> Vec<ContainerState> {
    let Ok(entries) = std::fs::read_dir(CONTAINERS_DIR) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            read_state(&name).ok()
        })
        .collect()
}

// ── CLI helpers ──────────────────────────────────────────────────────────────

pub async fn run_container(bin: &str, p: &crate::types::PendingContainer) -> Result<(), String> {
    let mut cmd = Command::new(bin);
    cmd.arg("run");
    cmd.arg("--name").arg(&p.name);
    cmd.arg("--detach");

    let net = translate_network_mode(&p.network_mode);
    cmd.arg("--network").arg(&net);

    for e in &p.env {
        cmd.arg("--env").arg(e);
    }
    for b in &p.binds {
        cmd.arg("--volume").arg(b);
    }
    for (k, v) in &p.labels {
        cmd.arg("--label").arg(format!("{}={}", k, v));
    }
    if let Some(wd) = &p.working_dir {
        cmd.arg("--workdir").arg(wd);
    }
    if let Some(user) = &p.user {
        cmd.arg("--user").arg(user);
    }
    if let Some(hostname) = &p.hostname {
        cmd.arg("--hostname").arg(hostname);
    }
    if let Some(mem) = p.memory {
        if mem > 0 {
            cmd.arg("--memory").arg(mem.to_string());
        }
    }
    for cap in &p.cap_add {
        cmd.arg("--cap-add").arg(cap);
    }
    for cap in &p.cap_drop {
        cmd.arg("--cap-drop").arg(cap);
    }

    cmd.arg("--");
    cmd.arg(&p.image);
    for arg in &p.cmd {
        cmd.arg(arg);
    }

    let out = cmd
        .output()
        .await
        .map_err(|e| format!("exec pelagos: {}", e))?;

    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub async fn stop_container(bin: &str, name: &str, timeout: Option<u32>) -> Result<(), String> {
    let mut cmd = Command::new(bin);
    cmd.arg("stop");
    if let Some(t) = timeout {
        cmd.arg("--time").arg(t.to_string());
    }
    cmd.arg(name);
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("exec pelagos: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub async fn remove_container(bin: &str, name: &str, force: bool) -> Result<(), String> {
    let mut cmd = Command::new(bin);
    cmd.arg("rm");
    if force {
        cmd.arg("--force");
    }
    cmd.arg(name);
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("exec pelagos: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Pull an image, returning stdout+stderr output for progress reporting.
pub async fn pull_image(bin: &str, image: &str) -> Result<Vec<u8>, String> {
    let out = Command::new(bin)
        .args(["image", "pull", image])
        .output()
        .await
        .map_err(|e| format!("exec pelagos: {}", e))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// List images as JSON (calls `pelagos image ls --json`).
pub async fn list_images_json(bin: &str) -> Result<Vec<serde_json::Value>, String> {
    let out = Command::new(bin)
        .args(["image", "ls", "--json"])
        .output()
        .await
        .map_err(|e| format!("exec pelagos: {}", e))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let parsed: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
    Ok(parsed)
}

fn translate_network_mode(mode: &str) -> String {
    match mode {
        "bridge" | "" => "bridge".to_string(),
        "none" => "loopback".to_string(),
        "host" => {
            log::warn!("network mode 'host' is not supported by pelagos; using no isolation");
            "none".to_string()
        }
        other => other.to_string(), // "container:NAME" passes through unchanged
    }
}
