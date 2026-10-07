//! Minimal session ownership (NOT authentication).
//!
//! Each browser gets an opaque, server-generated session token in an
//! `HttpOnly; SameSite=Strict` cookie. The server maps the token (by its
//! SHA-256, so the raw token is never stored) to an internal `Owner`, and
//! every file/job is owned by exactly one `Owner`.
//!
//! `Owner` has a private field and no public constructor: the middleware in
//! this module is the only code that can produce one, and handlers can only
//! obtain it through the `FromRequestParts` extractor below. Nothing a
//! client sends - user ids, workspace ids, paths - can select or forge an
//! owner. When real authentication arrives, the authenticator will replace
//! the cookie lookup here and `Owner` will carry the authenticated identity;
//! no handler or storage code needs to change.
//!
//! CSRF: state-changing requests must carry `X-MEB-Request: 1`. A
//! cross-site form cannot set custom headers, and a cross-origin `fetch`
//! that tries to would need a CORS preflight this server never approves.

use crate::error::ApiError;
use crate::AppState;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const SESSION_COOKIE: &str = "meb_sid";
pub const CSRF_HEADER: &str = "x-meb-request";

/// The owner of files and jobs. Opaque outside this module.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Owner {
    /// Random, server-generated, 32 lowercase hex chars. Used as a storage
    /// directory name; it is NOT the cookie token and reveals nothing.
    key: String,
}

impl Owner {
    fn new_random() -> Self {
        Owner {
            key: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

struct SessionEntry {
    owner: Owner,
    last_seen: Instant,
}

pub struct SessionStore {
    by_token_hash: Mutex<HashMap<[u8; 32], SessionEntry>>,
    max_sessions: usize,
}

fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn is_well_formed_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl SessionStore {
    pub fn new(max_sessions: usize) -> Self {
        SessionStore {
            by_token_hash: Mutex::new(HashMap::new()),
            max_sessions,
        }
    }

    fn resolve(&self, token: &str) -> Option<Owner> {
        if !is_well_formed_token(token) {
            return None;
        }
        let mut map = self.by_token_hash.lock().ok()?;
        let entry = map.get_mut(&hash_token(token))?;
        entry.last_seen = Instant::now();
        Some(entry.owner.clone())
    }

    /// New session: 256 bits from two CSPRNG-backed v4 UUIDs (244 random
    /// bits), hex encoded.
    fn create(&self) -> Option<(String, Owner)> {
        let mut map = self.by_token_hash.lock().ok()?;
        if map.len() >= self.max_sessions {
            return None;
        }
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let owner = Owner::new_random();
        map.insert(
            hash_token(&token),
            SessionEntry {
                owner: owner.clone(),
                last_seen: Instant::now(),
            },
        );
        Some((token, owner))
    }

    /// Forgets sessions idle for longer than `idle` that `still_in_use`
    /// says hold no resources. Returns the forgotten owners.
    pub(crate) fn prune_idle(
        &self,
        idle: Duration,
        still_in_use: impl Fn(&Owner) -> bool,
    ) -> Vec<Owner> {
        let Ok(mut map) = self.by_token_hash.lock() else {
            return Vec::new();
        };
        let now = Instant::now();
        let mut removed = Vec::new();
        map.retain(|_, e| {
            let keep = now.duration_since(e.last_seen) < idle || still_in_use(&e.owner);
            if !keep {
                removed.push(e.owner.clone());
            }
            keep
        });
        removed
    }

    pub fn len(&self) -> usize {
        self.by_token_hash.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn session_token_from(parts_headers: &axum::http::HeaderMap) -> Option<String> {
    for value in parts_headers.get_all(COOKIE) {
        let Ok(value) = value.to_str() else { continue };
        for pair in value.split(';') {
            let pair = pair.trim();
            if let Some(token) = pair
                .strip_prefix(SESSION_COOKIE)
                .and_then(|r| r.strip_prefix('='))
            {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// Resolves (or creates) the session for every `/api` request, enforces the
/// CSRF header on state-changing methods, and makes the `Owner` available
/// to handlers.
pub async fn session_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let safe_method = matches!(*req.method(), Method::GET | Method::HEAD);
    if !safe_method {
        let ok = req
            .headers()
            .get(CSRF_HEADER)
            .map(|v| v.as_bytes() == b"1")
            .unwrap_or(false);
        if !ok {
            return ApiError::csrf_rejected().into_response();
        }
    }

    let existing = session_token_from(req.headers()).and_then(|t| state.sessions.resolve(&t));
    let (owner, new_token) = match existing {
        Some(owner) => (owner, None),
        None => match state.sessions.create() {
            Some((token, owner)) => (owner, Some(token)),
            None => return ApiError::server_busy().into_response(),
        },
    };

    req.extensions_mut().insert(owner);
    let mut response = next.run(req).await;

    if let Some(token) = new_token {
        let secure = if state.config.secure_cookie {
            "; Secure"
        } else {
            ""
        };
        let cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict{secure}");
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().append(SET_COOKIE, value);
        }
    }
    response
}

impl<S: Send + Sync> FromRequestParts<S> for Owner {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Owner>()
            .cloned()
            .ok_or_else(|| ApiError::internal("session", "route is missing the session middleware"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_resolve_only_to_their_own_owner() {
        let store = SessionStore::new(10);
        let (t1, o1) = store.create().unwrap();
        let (t2, o2) = store.create().unwrap();
        assert_ne!(o1, o2);
        assert_eq!(store.resolve(&t1), Some(o1));
        assert_eq!(store.resolve(&t2), Some(o2));
        assert_eq!(store.resolve(&"0".repeat(64)), None);
        assert_eq!(store.resolve("not-a-token"), None);
    }

    #[test]
    fn session_cap_is_enforced() {
        let store = SessionStore::new(1);
        assert!(store.create().is_some());
        assert!(store.create().is_none());
    }

    #[test]
    fn idle_sessions_without_resources_are_pruned() {
        let store = SessionStore::new(10);
        let (_t, owner) = store.create().unwrap();
        let removed = store.prune_idle(Duration::ZERO, |_| false);
        assert_eq!(removed, vec![owner]);
        assert!(store.is_empty());
    }
}
