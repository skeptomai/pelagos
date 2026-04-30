use crate::{pelagos_state, state::AppState};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct CreateQuery {
    #[serde(rename = "fromImage")]
    pub from_image: Option<String>,
    pub tag: Option<String>,
}

/// POST /images/create?fromImage=...&tag=...
pub async fn create(
    Query(q): Query<CreateQuery>,
    State(app): State<AppState>,
) -> (StatusCode, Json<Value>) {
    let Some(from_image) = q.from_image else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "fromImage is required"})),
        );
    };
    let image = if let Some(tag) = q.tag.filter(|t| !t.is_empty()) {
        format!("{}:{}", from_image, tag)
    } else {
        from_image
    };

    log::info!("pulling image: {}", image);
    match pelagos_state::pull_image(app.pelagos_bin(), &image).await {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output);
            // Emit Docker-style progress lines
            let mut lines = Vec::new();
            for line in text.lines() {
                if !line.is_empty() {
                    lines.push(format!(
                        "{{\"status\":\"{}\",\"progressDetail\":{{}}}}",
                        escape_json_str(line)
                    ));
                }
            }
            lines.push(format!(
                "{{\"status\":\"Status: Downloaded newer image for {}\",\"progressDetail\":{{}}}}",
                image
            ));
            let body = lines.join("\n");
            (
                StatusCode::OK,
                Json(json!({"status": "pull complete", "raw": body})),
            )
        }
        Err(e) => {
            log::error!("pull {} failed: {}", image, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": e})),
            )
        }
    }
}

/// GET /images/{name}/json
pub async fn inspect(
    Path(name): Path<String>,
    State(app): State<AppState>,
) -> (StatusCode, Json<Value>) {
    let name = urlencoding_decode(&name);
    let images = match pelagos_state::list_images_json(app.pelagos_bin()).await {
        Ok(v) => v,
        Err(e) => {
            log::warn!("list images failed: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": e})),
            );
        }
    };

    let matched = images.iter().find(|img| {
        img.get("reference")
            .and_then(|r| r.as_str())
            .map(|r| image_matches(r, &name))
            .unwrap_or(false)
    });

    match matched {
        Some(img) => {
            let reference = img
                .get("reference")
                .and_then(|r| r.as_str())
                .unwrap_or(&name);
            let digest = img.get("digest").and_then(|d| d.as_str()).unwrap_or("");
            (
                StatusCode::OK,
                Json(json!({
                    "Id": format!("{}", digest),
                    "RepoTags": [reference],
                    "RepoDigests": [format!("{}@{}", reference, digest)],
                    "Size": 10000000,
                    "VirtualSize": 10000000,
                    "Os": "linux",
                    "Architecture": "arm64",
                    "Created": "2024-01-01T00:00:00Z"
                })),
            )
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": format!("image {} not found", name)})),
        ),
    }
}

fn image_matches(stored: &str, requested: &str) -> bool {
    if stored == requested {
        return true;
    }
    // "alpine" matches "alpine:latest"
    let stored_norm = normalize_ref(stored);
    let req_norm = normalize_ref(requested);
    stored_norm == req_norm
        || stored_norm.ends_with(&format!("/{}", req_norm))
        || req_norm.ends_with(&format!("/{}", stored_norm))
}

fn normalize_ref(r: &str) -> String {
    if r.contains(':') || r.contains('@') {
        r.to_string()
    } else {
        format!("{}:latest", r)
    }
}

fn urlencoding_decode(s: &str) -> String {
    // Axum already URL-decodes path segments, but handle the special case
    // where Docker clients encode "/" in image names as "%2F".
    s.replace("%2F", "/").replace("%3A", ":")
}

fn escape_json_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
