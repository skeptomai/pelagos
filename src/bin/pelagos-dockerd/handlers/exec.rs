use crate::{
    pelagos_state,
    state::AppState,
    types::{ExecCreateBody, ExecSession, ExecStartBody},
};
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

/// POST /containers/{id}/exec — register an exec instance.
pub async fn create(
    Path(container_id): Path<String>,
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    let body: ExecCreateBody = serde_json::from_slice(&body).unwrap_or_default();
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
        tty: body.tty.unwrap_or(false),
        env: body.env.unwrap_or_default(),
        working_dir: body.working_dir,
        user: body.user,
    };
    state.add_exec(exec_id.clone(), session).await;

    log::debug!("registered exec {} for {}", exec_id, container_id);
    (StatusCode::CREATED, Json(json!({"Id": exec_id})))
}

/// POST /exec/{id}/start
///
/// - `detach: true`  — run in background, return 200 immediately
/// - `detach: false` — HTTP `101 Switching Protocols` upgrade, then stream
///   Docker-framed output (8-byte header per chunk) until the process exits
pub async fn start(
    State(state): State<AppState>,
    Path(exec_id): Path<String>,
    req: Request,
) -> Response<Body> {
    // Parse body before consuming the request
    let (mut parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, 4096).await.unwrap_or_default();
    let exec_body: ExecStartBody = serde_json::from_slice(&body_bytes).unwrap_or_default();

    let session = match state.get_exec(&exec_id).await {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": format!("exec '{}' not found", exec_id)})),
            )
                .into_response()
        }
    };

    log::info!(
        "exec {} on {}: {:?} detach={:?} tty={}",
        exec_id,
        session.container_name,
        session.cmd,
        exec_body.detach,
        session.tty,
    );

    if exec_body.detach.unwrap_or(false) {
        let bin = state.pelagos_bin().to_string();
        let state2 = state.clone();
        let id2 = exec_id.clone();
        tokio::spawn(async move {
            let code = run_exec_subprocess(&bin, &session).await.unwrap_or(-1);
            state2.complete_exec(id2, code).await;
        });
        return StatusCode::OK.into_response();
    }

    // Interactive — requires HTTP/1.1 upgrade
    let on_upgrade = parts.extensions.remove::<hyper::upgrade::OnUpgrade>();

    let bin = state.pelagos_bin().to_string();
    let state2 = state.clone();
    let id2 = exec_id.clone();

    if let Some(on_upgrade) = on_upgrade {
        let session2 = session.clone();
        tokio::spawn(async move {
            match on_upgrade.await {
                Ok(upgraded) => {
                    let io = TokioIo::new(upgraded);
                    let code = run_exec_on_io(io, &session2, &bin).await;
                    state2.complete_exec(id2, code).await;
                }
                Err(e) => {
                    log::error!("upgrade error for exec {}: {}", id2, e);
                    state2.complete_exec(id2, -1).await;
                }
            }
        });

        Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header("Upgrade", "tcp")
            .header("Connection", "Upgrade")
            .body(Body::empty())
            .unwrap()
    } else {
        // Fallback for clients that don't perform the HTTP upgrade handshake.
        // Run synchronously and return output in the response body.
        let code = run_exec_subprocess(&bin, &session).await.unwrap_or(-1);
        state.complete_exec(exec_id, code).await;
        StatusCode::OK.into_response()
    }
}

/// GET /exec/{id}/json
pub async fn inspect(
    Path(exec_id): Path<String>,
    State(state): State<AppState>,
) -> (StatusCode, Json<Value>) {
    if let Some(exit_code) = state.get_completed_exec(&exec_id).await {
        return (
            StatusCode::OK,
            Json(json!({
                "ID": exec_id,
                "Running": false,
                "ExitCode": exit_code,
                "ProcessConfig": {"entrypoint": "", "arguments": []}
            })),
        );
    }
    if let Some(s) = state.get_exec(&exec_id).await {
        return (
            StatusCode::OK,
            Json(json!({
                "ID": exec_id,
                "ContainerID": s.container_name,
                "Running": true,
                "ExitCode": null,
                "ProcessConfig": {
                    "entrypoint": s.cmd.first().cloned().unwrap_or_default(),
                    "arguments": s.cmd.get(1..).unwrap_or(&[]).to_vec()
                }
            })),
        );
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({"message": "exec not found"})),
    )
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Run exec in background (detach mode), return exit code.
async fn run_exec_subprocess(bin: &str, session: &ExecSession) -> Result<i64, String> {
    let out = build_exec_command(bin, session)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    Ok(out.status.code().unwrap_or(-1) as i64)
}

/// Run exec attached, streaming Docker-framed output over an I/O object.
/// Returns the process exit code.
async fn run_exec_on_io<IO>(io: IO, session: &ExecSession, bin: &str) -> i64
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut child = match build_exec_command(bin, session)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("exec spawn: {}", e);
            return -1;
        }
    };

    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let (_reader, mut writer) = tokio::io::split(io);

    let tty = session.tty;
    let mut buf_out = vec![0u8; 4096];
    let mut buf_err = vec![0u8; 4096];
    let mut stdout_done = false;
    let mut stderr_done = false;

    while !stdout_done || !stderr_done {
        tokio::select! {
            n = stdout.read(&mut buf_out), if !stdout_done => {
                match n {
                    Ok(0) | Err(_) => stdout_done = true,
                    Ok(n) => {
                        let data = &buf_out[..n];
                        if tty {
                            let _ = writer.write_all(data).await;
                        } else {
                            let _ = write_docker_frame(&mut writer, 1, data).await;
                        }
                    }
                }
            }
            n = stderr.read(&mut buf_err), if !stderr_done => {
                match n {
                    Ok(0) | Err(_) => stderr_done = true,
                    Ok(n) => {
                        let data = &buf_err[..n];
                        if tty {
                            let _ = writer.write_all(data).await;
                        } else {
                            let _ = write_docker_frame(&mut writer, 2, data).await;
                        }
                    }
                }
            }
        }
    }

    let status = child.wait().await.ok();
    status.and_then(|s| s.code()).unwrap_or(-1) as i64
}

fn build_exec_command(bin: &str, session: &ExecSession) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(bin);
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
    cmd.arg("--").arg(&session.container_name);
    for arg in &session.cmd {
        cmd.arg(arg);
    }
    cmd
}

/// Write an 8-byte Docker multiplexed-stream frame header followed by data.
/// Frame format: [stream_type, 0, 0, 0, len_b3, len_b2, len_b1, len_b0] + payload
async fn write_docker_frame<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    stream_type: u8,
    data: &[u8],
) -> std::io::Result<()> {
    let len = data.len() as u32;
    let header = [
        stream_type,
        0,
        0,
        0,
        (len >> 24) as u8,
        (len >> 16) as u8,
        (len >> 8) as u8,
        len as u8,
    ];
    w.write_all(&header).await?;
    w.write_all(data).await
}
