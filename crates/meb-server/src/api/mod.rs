//! HTTP routes. Handlers translate HTTP <-> registry/storage/worker calls;
//! they never build filesystem paths from request data and never contain
//! engine code.
//!
//! Two things are served: the API under `/api/v1`, and - when a build of
//! the frontend is there - that build under everything else. The order is
//! what makes it safe: the API is matched first and has its own 404, so an
//! unknown API path is a JSON error and never a page.

mod capabilities;
mod files;
mod jobs;
mod static_files;

use crate::error::ApiError;
use crate::session::session_middleware;
use crate::AppState;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::header::{CACHE_CONTROL, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS};
use axum::http::HeaderValue;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::Router;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/capabilities", get(capabilities::get_capabilities))
        // The upload handler streams the body itself and enforces its own
        // byte cap, so axum's buffered-body limit does not apply here.
        .route(
            "/files",
            post(files::upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/files/{id}", get(files::get_file))
        .route("/jobs", post(jobs::create_job))
        .route("/jobs/{id}", get(jobs::get_job).delete(jobs::delete_job))
        .route("/jobs/{id}/events", get(jobs::job_events))
        .route("/jobs/{id}/cancel", post(jobs::cancel_job))
        .route("/jobs/{id}/download", get(jobs::download))
        .fallback(|| async { ApiError::not_found() })
        .layer(middleware::from_fn_with_state(
            state.clone(),
            session_middleware,
        ));

    let mut app = Router::new()
        .nest("/api/v1", api)
        .route("/healthz", get(|| async { "ok" }));

    // The built frontend, under every path the API did not claim. Mounted
    // only when there really is a build to serve: otherwise a non-API
    // path is a plain 404, which is the honest answer for a server running
    // with no frontend (the development setup, where Vite serves it and
    // proxies `/api` here).
    match static_files::root(&state.config) {
        Some(root) => {
            tracing::info!(dir = %root.display(), "serving the frontend");
            app = app.fallback(any(static_files::serve));
        }
        None => {
            if let Some(configured) = state.config.static_dir.as_deref() {
                tracing::warn!(
                    dir = %configured.display(),
                    "static directory does not exist: serving the API only"
                );
            }
        }
    }

    app.layer(middleware::from_fn(security_headers))
        .with_state(state)
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await.into_response();
    let headers = response.headers_mut();
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    if !headers.contains_key(CACHE_CONTROL) {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}
