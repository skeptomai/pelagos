//! Axum router and handler modules for the Docker Engine API.

use axum::{Router, routing};
use crate::state::AppState;

mod containers;
mod exec;
mod images;
mod version;

pub fn router(state: AppState) -> Router {
    Router::new()
        // Ping / version / info
        .route("/_ping", routing::get(ping).head(ping))
        .route("/version", routing::get(version::version))
        .route("/info", routing::get(version::info))
        // Images — literal routes before parameterized ones
        .route("/images/create", routing::post(images::create))
        .route("/images/:name/json", routing::get(images::inspect))
        // Containers — literal /json before /:id
        .route("/containers/json", routing::get(containers::list))
        .route("/containers/create", routing::post(containers::create))
        .route("/containers/:id/json", routing::get(containers::inspect))
        .route("/containers/:id/start", routing::post(containers::start))
        .route("/containers/:id/stop", routing::post(containers::stop))
        .route("/containers/:id/kill", routing::post(containers::kill))
        .route("/containers/:id", routing::delete(containers::remove))
        .route("/containers/:id/wait", routing::post(containers::wait))
        .route("/containers/:id/logs", routing::get(containers::logs))
        .route("/containers/:id/stats", routing::get(containers::stats))
        // Exec
        .route("/containers/:id/exec", routing::post(exec::create))
        .route("/exec/:id/start", routing::post(exec::start))
        .route("/exec/:id/json", routing::get(exec::inspect))
        .with_state(state)
}

async fn ping() -> &'static str {
    "OK"
}
