//! Axum router and handler modules for the Docker Engine API.

use crate::state::AppState;
use axum::{extract::Request, middleware, response::Response, routing, Router};

async fn log_requests(req: Request, next: axum::middleware::Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    log::info!("--> {} {}", method, uri);
    let resp = next.run(req).await;
    log::info!("<-- {} {}", method, resp.status());
    resp
}

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
        // Container archive (fallback, returns 404)
        .route("/containers/:id/archive", routing::get(containers::archive))
        // Volumes
        .route("/volumes", routing::get(containers::list_volumes))
        .route("/volumes/:name", routing::delete(containers::remove_volume))
        // Exec
        .route("/containers/:id/exec", routing::post(exec::create))
        .route("/exec/:id/start", routing::post(exec::start))
        .route("/exec/:id/json", routing::get(exec::inspect))
        .with_state(state)
        .layer(middleware::from_fn(log_requests))
}

async fn ping() -> &'static str {
    "OK"
}
