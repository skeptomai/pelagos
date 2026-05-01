use crate::{
    pelagos_state::{self, ContainerState},
    state::{self, AppState},
    types::{ContainerCreateBody, PendingContainer},
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

// ── List ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    pub all: Option<String>,
    #[serde(default)]
    pub filters: Option<String>,
}

pub async fn list(Query(q): Query<ListQuery>) -> (StatusCode, Json<Value>) {
    let show_all = q
        .all
        .as_deref()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let filters = parse_filters(q.filters.as_deref());

    let mut items: Vec<Value> = Vec::new();

    for c in pelagos_state::list_states() {
        if !show_all && !c.is_running() {
            continue;
        }
        if !filters.labels.is_empty() && !matches_labels(&c.labels, &filters.labels) {
            continue;
        }
        if !filters.names.is_empty() && !matches_name(&c.name, &filters.names) {
            continue;
        }
        if !filters.statuses.is_empty()
            && !filters.statuses.iter().any(|s| s == c.docker_status_str())
        {
            continue;
        }
        items.push(container_summary_json(&c));
    }

    for p in state::list_pending() {
        if !show_all {
            continue;
        }
        if !filters.labels.is_empty() && !matches_labels(&p.labels, &filters.labels) {
            continue;
        }
        if !filters.names.is_empty() && !matches_name(&p.name, &filters.names) {
            continue;
        }
        if !filters.statuses.is_empty() && !filters.statuses.contains(&"created".to_string()) {
            continue;
        }
        items.push(pending_summary_json(&p));
    }

    (StatusCode::OK, Json(Value::Array(items)))
}

// ── Create ────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct CreateQuery {
    pub name: Option<String>,
}

pub async fn create(
    Query(q): Query<CreateQuery>,
    Json(body): Json<ContainerCreateBody>,
) -> (StatusCode, Json<Value>) {
    let name = match q.name {
        Some(n) if !n.is_empty() => n,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"message": "container name is required"})),
            );
        }
    };

    if pelagos_state::read_state(&name).is_ok() || state::load_pending(&name).is_ok() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"message": format!("container '{}' already exists", name)})),
        );
    }

    let pending = PendingContainer::from_create(name.clone(), body);
    if let Err(e) = state::save_pending(&pending) {
        log::error!("save pending {}: {}", name, e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string()})),
        );
    }

    log::info!("created container: {}", name);
    (
        StatusCode::CREATED,
        Json(json!({"Id": name, "Warnings": []})),
    )
}

// ── Inspect ───────────────────────────────────────────────────────────────────

pub async fn inspect(Path(id): Path<String>) -> (StatusCode, Json<Value>) {
    // Running/exited container
    if let Ok(c) = pelagos_state::read_state(&id) {
        return (StatusCode::OK, Json(container_inspect_json(&c)));
    }
    // Pending (created, not started)
    if let Ok(p) = state::load_pending(&id) {
        return (StatusCode::OK, Json(pending_inspect_json(&p)));
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({"message": format!("container '{}' not found", id)})),
    )
}

// ── Start ─────────────────────────────────────────────────────────────────────

pub async fn start(
    Path(id): Path<String>,
    State(app): State<AppState>,
) -> (StatusCode, Json<Value>) {
    let pending = match state::load_pending(&id) {
        Ok(p) => p,
        Err(_) => {
            if pelagos_state::read_state(&id).is_ok() {
                return (StatusCode::NOT_MODIFIED, Json(json!({})));
            }
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": format!("container '{}' not found", id)})),
            );
        }
    };

    log::info!("starting container: {}", id);
    if let Err(e) = pelagos_state::run_container(app.pelagos_bin(), &pending).await {
        log::error!("start {}: {}", id, e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e})),
        );
    }

    state::remove_pending(&id);
    (StatusCode::NO_CONTENT, Json(json!({})))
}

// ── Stop ──────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct StopQuery {
    pub t: Option<u32>,
}

pub async fn stop(
    Path(id): Path<String>,
    Query(q): Query<StopQuery>,
    State(app): State<AppState>,
) -> (StatusCode, Json<Value>) {
    log::info!("stopping container: {}", id);
    match pelagos_state::stop_container(app.pelagos_bin(), &id, q.t).await {
        Ok(_) => (StatusCode::NO_CONTENT, Json(json!({}))),
        Err(e) => {
            if e.contains("not found") || e.contains("no such") {
                (StatusCode::NOT_FOUND, Json(json!({"message": e})))
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"message": e})),
                )
            }
        }
    }
}

// ── Kill ──────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[allow(dead_code)]
pub struct KillQuery {
    pub signal: Option<String>,
}

pub async fn kill(
    Path(id): Path<String>,
    Query(_q): Query<KillQuery>,
    State(app): State<AppState>,
) -> (StatusCode, Json<Value>) {
    log::info!("killing container: {}", id);
    match pelagos_state::stop_container(app.pelagos_bin(), &id, Some(0)).await {
        Ok(_) => (StatusCode::NO_CONTENT, Json(json!({}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e})),
        ),
    }
}

// ── Remove ────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[allow(dead_code)]
pub struct RemoveQuery {
    #[serde(default)]
    pub force: Option<String>,
    #[serde(default)]
    pub v: Option<String>,
}

pub async fn remove(
    Path(id): Path<String>,
    Query(q): Query<RemoveQuery>,
    State(app): State<AppState>,
) -> (StatusCode, Json<Value>) {
    let force = q
        .force
        .as_deref()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    state::remove_pending(&id);
    log::info!("removing container: {} (force={})", id, force);
    match pelagos_state::remove_container(app.pelagos_bin(), &id, force).await {
        Ok(_) => (StatusCode::NO_CONTENT, Json(json!({}))),
        Err(e) => {
            if e.contains("not found") || e.contains("no such") {
                (StatusCode::NOT_FOUND, Json(json!({"message": e})))
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"message": e})),
                )
            }
        }
    }
}

// ── Wait ──────────────────────────────────────────────────────────────────────

pub async fn wait(Path(id): Path<String>) -> (StatusCode, Json<Value>) {
    // Poll until container exits
    loop {
        match pelagos_state::read_state(&id) {
            Ok(c) if !c.is_running() => {
                return (
                    StatusCode::OK,
                    Json(json!({"StatusCode": c.exit_code.unwrap_or(0)})),
                );
            }
            Err(_) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"message": format!("container '{}' not found", id)})),
                );
            }
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

// ── Logs ──────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[allow(dead_code)]
pub struct LogsQuery {
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub follow: Option<String>,
    #[serde(default)]
    pub tail: Option<String>,
}

pub async fn logs(Path(id): Path<String>, Query(q): Query<LogsQuery>) -> Response<Body> {
    let c = match pelagos_state::read_state(&id) {
        Ok(c) => c,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": format!("container '{}' not found", id)})),
            )
                .into_response();
        }
    };

    let log_path = match c.stdout_log.clone() {
        Some(p) => p,
        None => {
            return (StatusCode::NO_CONTENT, Body::empty()).into_response();
        }
    };

    let follow = q
        .follow
        .as_deref()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let container_name = id.clone();

    if !follow {
        // Non-follow: read the whole log file and return it as a single framed body.
        let data = tokio::fs::read_to_string(&log_path)
            .await
            .unwrap_or_default();
        let mut body: Vec<u8> = Vec::new();
        for line in data.lines() {
            let mut payload = line.as_bytes().to_vec();
            payload.push(b'\n');
            body.extend_from_slice(&docker_frame_header(1, payload.len() as u32));
            body.extend_from_slice(&payload);
        }
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/octet-stream")
            .body(Body::from(body))
            .unwrap();
    }

    // follow=true: stream lines as they arrive until the container exits.
    let stream = futures_util::stream::unfold(
        (log_path, container_name, 0u64),
        move |(path, name, offset)| async move {
            loop {
                let data = tokio::fs::read(&path).await.unwrap_or_default();
                if data.len() as u64 > offset {
                    let new_bytes = &data[offset as usize..];
                    let new_offset = data.len() as u64;
                    let mut body: Vec<u8> = Vec::new();
                    for line in std::str::from_utf8(new_bytes).unwrap_or("").lines() {
                        let mut payload = line.as_bytes().to_vec();
                        payload.push(b'\n');
                        body.extend_from_slice(&docker_frame_header(1, payload.len() as u32));
                        body.extend_from_slice(&payload);
                    }
                    if !body.is_empty() {
                        return Some((Ok::<_, std::io::Error>(body), (path, name, new_offset)));
                    }
                }
                let still_running = pelagos_state::read_state(&name)
                    .map(|s| s.is_running())
                    .unwrap_or(false);
                if !still_running {
                    return None;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        },
    );

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn docker_frame_header(stream_type: u8, len: u32) -> Vec<u8> {
    vec![
        stream_type,
        0,
        0,
        0,
        (len >> 24) as u8,
        (len >> 16) as u8,
        (len >> 8) as u8,
        len as u8,
    ]
}

// ── Stats ─────────────────────────────────────────────────────────────────────

pub async fn stats(Path(id): Path<String>) -> (StatusCode, Json<Value>) {
    let c = match pelagos_state::read_state(&id) {
        Ok(c) => c,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": format!("container '{}' not found", id)})),
            );
        }
    };

    let (cpu_ns, mem_bytes) = read_cgroup_stats(c.cgroup_name.as_deref());

    (
        StatusCode::OK,
        Json(json!({
            "id": id,
            "cpu_stats": {
                "cpu_usage": {
                    "total_usage": cpu_ns
                },
                "system_cpu_usage": read_system_cpu_ns()
            },
            "memory_stats": {
                "usage": mem_bytes
            },
            "networks": {}
        })),
    )
}

fn read_cgroup_stats(cgroup: Option<&str>) -> (u64, u64) {
    let Some(cg) = cgroup else { return (0, 0) };
    let cpu = read_cpu_ns(cg);
    let mem = read_mem_bytes(cg);
    (cpu, mem)
}

fn read_cpu_ns(cg: &str) -> u64 {
    let path = format!("/sys/fs/cgroup/{}/cpu.stat", cg);
    let Ok(data) = std::fs::read_to_string(&path) else {
        return 0;
    };
    for line in data.lines() {
        if let Some(rest) = line.strip_prefix("usage_usec ") {
            if let Ok(usec) = rest.trim().parse::<u64>() {
                return usec * 1000; // to nanoseconds
            }
        }
    }
    0
}

fn read_mem_bytes(cg: &str) -> u64 {
    let path = format!("/sys/fs/cgroup/{}/memory.current", cg);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn read_system_cpu_ns() -> u64 {
    let Ok(data) = std::fs::read_to_string("/proc/stat") else {
        return 0;
    };
    let line = data.lines().next().unwrap_or("");
    let fields: Vec<&str> = line.split_whitespace().collect();
    // Fields 1..=8: user nice system idle iowait irq softirq steal
    let ticks: u64 = fields[1..]
        .iter()
        .take(8)
        .filter_map(|s| s.parse::<u64>().ok())
        .sum();
    // Convert jiffies (typically 100Hz) to nanoseconds
    ticks * 10_000_000
}

// ── JSON builders ─────────────────────────────────────────────────────────────

fn container_summary_json(c: &ContainerState) -> Value {
    let status_str = if c.is_running() {
        "Up".to_string()
    } else {
        format!("Exited ({})", c.exit_code.unwrap_or(0))
    };
    json!({
        "Id": c.name,
        "Names": [format!("/{}", c.name)],
        "Image": c.image(),
        "ImageID": "",
        "State": c.docker_status_str(),
        "Status": status_str,
        "Labels": c.labels,
        "Created": 0
    })
}

fn pending_summary_json(p: &PendingContainer) -> Value {
    json!({
        "Id": p.name,
        "Names": [format!("/{}", p.name)],
        "Image": p.image,
        "ImageID": "",
        "State": "created",
        "Status": "Created",
        "Labels": p.labels,
        "Created": 0
    })
}

fn container_inspect_json(c: &ContainerState) -> Value {
    let ip = c.bridge_ip.clone().unwrap_or_default();
    let mut networks = serde_json::Map::new();
    if !ip.is_empty() {
        networks.insert(
            "bridge".to_string(),
            json!({
                "IPAddress": ip,
                "Gateway": "172.19.0.1",
                "MacAddress": ""
            }),
        );
    }
    json!({
        "Id": c.name,
        "Name": format!("/{}", c.name),
        "State": {
            "Status": c.docker_status_str(),
            "Running": c.is_running(),
            "Paused": false,
            "Restarting": false,
            "Dead": false,
            "Pid": c.pid,
            "ExitCode": c.exit_code.unwrap_or(0),
            "StartedAt": c.started_at,
            "FinishedAt": if c.is_running() { "0001-01-01T00:00:00Z".to_string() } else { c.started_at.clone() }
        },
        "Config": {
            "Image": c.image(),
            "Cmd": c.command,
            "Env": c.env(),
            "Labels": c.labels,
            "WorkingDir": c.spawn_config.as_ref().and_then(|sc| sc.working_dir.as_deref()).unwrap_or("")
        },
        "HostConfig": {
            "NetworkMode": c.network_mode(),
            "Binds": c.binds()
        },
        "NetworkSettings": {
            "IPAddress": ip,
            "Networks": networks
        }
    })
}

fn pending_inspect_json(p: &PendingContainer) -> Value {
    json!({
        "Id": p.name,
        "Name": format!("/{}", p.name),
        "State": {
            "Status": "created",
            "Running": false,
            "Paused": false,
            "Restarting": false,
            "Dead": false,
            "Pid": 0,
            "ExitCode": 0,
            "StartedAt": "0001-01-01T00:00:00Z",
            "FinishedAt": "0001-01-01T00:00:00Z"
        },
        "Config": {
            "Image": p.image,
            "Cmd": p.cmd,
            "Env": p.env,
            "Labels": p.labels,
            "WorkingDir": p.working_dir.as_deref().unwrap_or("")
        },
        "HostConfig": {
            "NetworkMode": p.network_mode,
            "Binds": p.binds
        },
        "NetworkSettings": {
            "IPAddress": "",
            "Networks": {}
        }
    })
}

/// GET /volumes — list volumes (kubelet calls this to clean up emptyDir volumes)
pub async fn list_volumes() -> (StatusCode, Json<Value>) {
    (StatusCode::OK, Json(json!({"Volumes": [], "Warnings": []})))
}

/// DELETE /volumes/{name} — remove a volume
pub async fn remove_volume(Path(_name): Path<String>) -> StatusCode {
    StatusCode::NO_CONTENT
}

/// GET /containers/{id}/archive — download file from container (kubelet fallback path)
pub async fn archive(Path(id): Path<String>) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"message": format!("archive not supported for '{}'", id)})),
    )
}

struct Filters {
    labels: Vec<(String, String)>,
    names: Vec<String>,
    statuses: Vec<String>,
}

fn parse_filters(raw: Option<&str>) -> Filters {
    let mut f = Filters {
        labels: Vec::new(),
        names: Vec::new(),
        statuses: Vec::new(),
    };
    let Some(raw) = raw else { return f };
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return f;
    };

    if let Some(labels) = parsed.get("label").and_then(|v| v.as_array()) {
        f.labels = labels
            .iter()
            .filter_map(|v| v.as_str())
            .filter_map(|s| {
                let (k, v) = s.split_once('=')?;
                Some((k.to_string(), v.to_string()))
            })
            .collect();
    }
    if let Some(names) = parsed.get("name").and_then(|v| v.as_array()) {
        f.names = names
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.to_string())
            .collect();
    }
    if let Some(statuses) = parsed.get("status").and_then(|v| v.as_array()) {
        // Docker status values: "running","exited","created","paused","restarting","dead"
        // Map Docker status names to our docker_status_str() equivalents
        f.statuses = statuses
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.to_string())
            .collect();
    }
    f
}

fn matches_labels(
    container_labels: &std::collections::HashMap<String, String>,
    filters: &[(String, String)],
) -> bool {
    filters
        .iter()
        .all(|(k, v)| container_labels.get(k).map(|cv| cv == v).unwrap_or(false))
}

fn matches_name(container_name: &str, patterns: &[String]) -> bool {
    let full_name = format!("/{}", container_name);
    patterns.iter().any(|pat| {
        // Docker name filter supports regex; handle the common prefix case (^/name)
        // and substring match
        if let Some(prefix) = pat.strip_prefix('^') {
            full_name.starts_with(prefix) || container_name.starts_with(prefix)
        } else {
            full_name.contains(pat.as_str()) || container_name.contains(pat.as_str())
        }
    })
}
