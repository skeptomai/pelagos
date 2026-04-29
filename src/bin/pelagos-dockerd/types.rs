//! Docker API request/response types for pelagos-dockerd.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Requests ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerCreateBody {
    pub image: String,
    #[serde(default)]
    pub cmd: Option<Vec<String>>,
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<Vec<String>>,
    #[serde(default)]
    pub labels: Option<HashMap<String, String>>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub host_config: Option<HostConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct HostConfig {
    #[serde(default)]
    pub network_mode: Option<String>,
    #[serde(default)]
    pub binds: Option<Vec<String>>,
    #[serde(default)]
    pub memory: Option<i64>,
    #[serde(default)]
    pub cap_add: Option<Vec<String>>,
    #[serde(default)]
    pub cap_drop: Option<Vec<String>>,
    #[serde(default)]
    pub security_opt: Option<Vec<String>>,
    #[serde(default)]
    pub privileged: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExecCreateBody {
    #[serde(default)]
    pub cmd: Vec<String>,
    #[serde(default)]
    pub attach_stdin: bool,
    #[serde(default)]
    pub attach_stdout: bool,
    #[serde(default)]
    pub attach_stderr: bool,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub env: Option<Vec<String>>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct ExecStartBody {
    #[serde(default)]
    pub detach: bool,
    #[serde(default)]
    pub tty: bool,
}

// ── Pending container (stored locally, not yet started) ─────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingContainer {
    pub name: String,
    pub image: String,
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    pub labels: HashMap<String, String>,
    pub network_mode: String,
    pub binds: Vec<String>,
    pub working_dir: Option<String>,
    pub user: Option<String>,
    pub hostname: Option<String>,
    pub memory: Option<i64>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
}

impl PendingContainer {
    pub fn from_create(name: String, body: ContainerCreateBody) -> Self {
        let hc = body.host_config.unwrap_or_default();
        let cmd = if let Some(ep) = body.entrypoint {
            let mut v = ep;
            if let Some(c) = body.cmd {
                v.extend(c);
            }
            v
        } else {
            body.cmd.unwrap_or_default()
        };

        Self {
            name,
            image: body.image,
            cmd,
            env: body.env.unwrap_or_default(),
            labels: body.labels.unwrap_or_default(),
            network_mode: hc.network_mode.unwrap_or_else(|| "bridge".to_string()),
            binds: hc.binds.unwrap_or_default(),
            working_dir: body.working_dir,
            user: body.user,
            hostname: body.hostname,
            memory: hc.memory,
            cap_add: hc.cap_add.unwrap_or_default(),
            cap_drop: hc.cap_drop.unwrap_or_default(),
        }
    }
}

// ── Exec session ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExecSession {
    pub container_name: String,
    pub cmd: Vec<String>,
    pub tty: bool,
    pub env: Vec<String>,
    pub working_dir: Option<String>,
    pub user: Option<String>,
}
