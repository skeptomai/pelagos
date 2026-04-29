use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{json, Value};
use tokio::process::Command;
use uuid::Uuid;
use crate::{
    pelagos_state,
    state::AppState,
    types::{ExecCreateBody, ExecSession, ExecStartBody},
};

/// POST /containers/{id}/exec — create an exec instance.
pub async fn create(
    Path(container_id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<ExecCreateBody>,
) -> (StatusCode, Json<Value>) {
    // Verify container exists
    if pelagos_state::read_state(&container_id).is_err() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"message": format!("container '{}' not found", container_id)})),
        );
    }

    let exec_id = Uuid::new_v4().to_string();
    let session = ExecSession {
        container_name: container_id.clone(),
        cmd: body.cmd,
        tty: body.tty,
        env: body.env.unwrap_or_default(),
        working_dir: body.working_dir,
        user: body.user,
    };
    state.add_exec(exec_id.clone(), session).await;

    log::debug!("created exec {} for container {}", exec_id, container_id);
    (StatusCode::CREATED, Json(json!({"Id": exec_id})))
}

/// POST /exec/{id}/start — run the exec instance.
pub async fn start(
    Path(exec_id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<ExecStartBody>,
) -> (StatusCode, Json<Value>) {
    let session = match state.get_exec(&exec_id).await {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": format!("exec '{}' not found", exec_id)})),
            );
        }
    };

    log::info!(
        "exec {} on {}: {:?} (detach={})",
        exec_id,
        session.container_name,
        session.cmd,
        body.detach
    );

    let mut cmd = Command::new(state.pelagos_bin());
    cmd.arg("exec");

    if let Some(wd) = &session.working_dir {
        cmd.arg("--workdir").arg(wd);
    }
    if let Some(user) = &session.user {
        cmd.arg("--user").arg(user);
    }
    for e in &session.env {
        cmd.arg("--env").arg(e);
    }

    cmd.arg("--");
    cmd.arg(&session.container_name);
    for arg in &session.cmd {
        cmd.arg(arg);
    }

    match cmd.output().await {
        Ok(out) => {
            let exit_code = out.status.code().unwrap_or(1);
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            state.remove_exec(&exec_id).await;
            if body.detach {
                (StatusCode::OK, Json(json!({})))
            } else {
                (StatusCode::OK, Json(json!({
                    "ExitCode": exit_code,
                    "Stdout": stdout,
                    "Stderr": stderr
                })))
            }
        }
        Err(e) => {
            log::error!("exec {} failed: {}", exec_id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": e.to_string()})),
            )
        }
    }
}

/// GET /exec/{id}/json — inspect an exec instance.
pub async fn inspect(
    Path(exec_id): Path<String>,
    State(state): State<AppState>,
) -> (StatusCode, Json<Value>) {
    match state.get_exec(&exec_id).await {
        Some(s) => (StatusCode::OK, Json(json!({
            "ID": exec_id,
            "ContainerID": s.container_name,
            "Running": true,
            "ExitCode": null,
            "ProcessConfig": {
                "entrypoint": s.cmd.first().cloned().unwrap_or_default(),
                "arguments": s.cmd.get(1..).unwrap_or(&[]).to_vec()
            }
        }))),
        None => (StatusCode::NOT_FOUND, Json(json!({"message": "exec not found"}))),
    }
}
