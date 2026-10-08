//! Serving the built frontend.
//!
//! Everything the API did not claim arrives here. Three cases, in order:
//!
//! 1. The path names a file in the static directory - serve it.
//! 2. The path looks like a CLIENT-SIDE ROUTE (no file extension) - serve
//!    `index.html`, so a deep link the browser asks for directly works the
//!    same as one reached by clicking. This is the SPA rewrite.
//! 3. The path looks like a FILE and is not there - 404.
//!
//! Case 3 is the part a plain "fall back to index.html for everything"
//! setup gets wrong: a mistyped or stale `/assets/app-a1b2c3.js` would be
//! answered with an HTML page and a 200, and the browser would report a
//! syntax error in a script instead of a missing file. Telling the two
//! apart by whether the last segment has an extension is a heuristic, but
//! it is the one that matches how a bundler names things.
//!
//! Path safety is `ServeDir`'s: it resolves the request path against the
//! root and refuses anything that escapes it (`..`, absolute paths,
//! encoded separators), so no request can name a file outside the
//! directory the operator configured.
//!
//! Caching is decided here rather than left to the file server, because it
//! depends on what the file IS, not on where it came from - see
//! `cache_control_for`.

use crate::error::ApiError;
use crate::AppState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use std::path::{Path, PathBuf};
use tower::ServiceExt;
use tower_http::services::ServeDir;

/// The document a client-side route is answered with.
const INDEX: &str = "index.html";

/// A build output file whose name contains its own content hash can be
/// cached forever: a new build has a new name. This is the directory the
/// bundler puts them in (Vite's default, and the project's).
const HASHED_ASSET_DIR: &str = "/assets/";

/// Cache policies. A year is the practical "forever" (RFC 9111 caps it),
/// and `immutable` tells a browser not even to revalidate.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Everything else in the build: named without a hash, so it may change
/// under the same URL. Cached, but never for long.
const SHORT_LIVED: &str = "public, max-age=3600";
/// The document that names every hashed asset. If this were cached, a
/// deployed update would keep pointing at files that no longer exist, so
/// it is revalidated on every load.
const NO_CACHE: &str = "no-cache, must-revalidate";

/// The directory to serve, if there is one to serve.
///
/// A configured directory that does not exist is not an error: the
/// frontend may simply not be built in this checkout. The caller decides
/// what to say about it; here it just means "nothing to serve".
pub fn root(config: &crate::Config) -> Option<PathBuf> {
    let dir = config.static_dir.as_deref()?;
    dir.is_dir().then(|| dir.to_path_buf())
}

pub async fn serve(State(state): State<AppState>, request: Request) -> Response {
    let Some(root) = root(&state.config) else {
        // The router only mounts this handler when there is a directory,
        // so this is the case where it disappeared while running.
        return ApiError::not_found().into_response();
    };

    let path = request.uri().path().to_string();
    let method = request.method().clone();

    // `ServeDir` is infallible: a missing file is a 404 RESPONSE, not an
    // error, which is what the SPA rewrite below keys off.
    let served = match ServeDir::new(&root).oneshot(request).await {
        Ok(response) => response,
        Err(_) => return ApiError::not_found().into_response(),
    };

    if served.status() != StatusCode::NOT_FOUND {
        let mut response = served.map(Body::new);
        set_cache_control(&mut response, cache_control_for(&path));
        return response;
    }

    // Not a file. A client-side route is answered with the document; a
    // missing file is answered as missing.
    let is_readable_method = matches!(method, Method::GET | Method::HEAD);
    if !is_readable_method || !looks_like_a_route(&path) {
        return ApiError::not_found().into_response();
    }
    index(&root, method).await
}

/// The SPA document, with the body omitted for a `HEAD`.
async fn index(root: &Path, method: Method) -> Response {
    let Ok(bytes) = tokio::fs::read(root.join(INDEX)).await else {
        // A static directory with no index.html is a broken deployment,
        // not a routable application.
        return ApiError::not_found().into_response();
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    let mut response = Response::new(body);
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    set_cache_control(&mut response, NO_CACHE);
    response
}

/// Whether a path is a client-side route rather than a file.
///
/// The test is on the LAST segment only: `/raporlar/2026` is a route,
/// `/assets/app-a1b2c3.js` is a file, and `/belge.v2/ayarlar` is a route
/// even though an earlier segment has a dot in it.
fn looks_like_a_route(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or("");
    !last.contains('.')
}

/// How long a served path may be cached.
fn cache_control_for(path: &str) -> &'static str {
    if path == "/" || path.ends_with(&format!("/{INDEX}")) {
        NO_CACHE
    } else if path.starts_with(HASHED_ASSET_DIR) {
        IMMUTABLE
    } else {
        SHORT_LIVED
    }
}

/// Sets the policy, replacing whatever the file server chose. The
/// security-header middleware only fills `Cache-Control` in when it is
/// absent, so this value is what reaches the client.
fn set_cache_control(response: &mut Response, value: &'static str) {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(value));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_is_told_apart_from_a_file_by_its_last_segment() {
        for route in [
            "/",
            "/raporlar",
            "/raporlar/2026",
            "/ogrenci/kayit/yeni",
            // A dot in an earlier segment does not make the last one a
            // file.
            "/belge.v2/ayarlar",
        ] {
            assert!(looks_like_a_route(route), "{route} is not treated as a route");
        }
        for file in [
            "/index.html",
            "/favicon.ico",
            "/assets/app-a1b2c3.js",
            "/assets/style.css",
            "/raporlar/ozet.pdf",
        ] {
            assert!(!looks_like_a_route(file), "{file} is treated as a route");
        }
    }

    #[test]
    fn hashed_assets_are_cached_forever_and_the_document_never_is() {
        // The whole point of the split: the document names the hashed
        // files, so caching IT would pin a deployed update to assets that
        // no longer exist.
        assert_eq!(cache_control_for("/"), NO_CACHE);
        assert_eq!(cache_control_for("/index.html"), NO_CACHE);
        assert_eq!(cache_control_for("/assets/app-a1b2c3.js"), IMMUTABLE);
        assert_eq!(cache_control_for("/assets/logo-9f8e7d.svg"), IMMUTABLE);
        // Not hash-named, so it may change under the same URL.
        assert_eq!(cache_control_for("/favicon.ico"), SHORT_LIVED);
        assert_eq!(cache_control_for("/manifest.webmanifest"), SHORT_LIVED);
        // Nothing is cached forever unless its name can be trusted to
        // change with its content.
        for path in ["/", "/index.html", "/favicon.ico", "/sw.js"] {
            assert_ne!(cache_control_for(path), IMMUTABLE, "{path}");
        }
    }

    #[test]
    fn nothing_is_served_without_a_directory_that_exists() {
        let mut config = crate::Config::development(std::env::temp_dir().join("meb_static_cfg"));
        // Configured but absent: nothing to serve, and not an error.
        config.static_dir = Some(std::env::temp_dir().join("meb_static_absent_dir"));
        assert!(root(&config).is_none());
        // Explicitly disabled.
        config.static_dir = None;
        assert!(root(&config).is_none());
        // Present: served.
        let dir = std::env::temp_dir().join(format!(
            "meb_static_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        config.static_dir = Some(dir.clone());
        assert_eq!(root(&config).as_deref(), Some(dir.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
