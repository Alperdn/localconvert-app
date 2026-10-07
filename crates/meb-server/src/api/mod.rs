//! HTTP routes. Handlers translate HTTP <-> registry/storage/worker calls;
//! they never build filesystem paths from request data and never contain
//! engine code.

mod capabilities;
mod files;
mod jobs;

use crate::error::ApiError;
use crate::session::session_middleware;
use crate::AppState;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::header::{CACHE_CONTROL, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS};
use axum::http::HeaderValue;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
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

    Router::new()
        .nest("/api/v1", api)
        .route("/healthz", get(|| async { "ok" }))
        .layer(middleware::from_fn(security_headers))
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
