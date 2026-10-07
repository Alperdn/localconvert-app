//! Server configuration. Every limit here is an INITIAL ENGINEERING DEFAULT
//! (docs/WEB_ARCHITECTURE_PROPOSAL.md §I), tunable later - not final policy.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    /// Must be a loopback address in this phase: there is no
    /// authentication yet, so the server refuses to listen on a network
    /// interface (see `from_env`).
    pub bind: SocketAddr,
    /// Root of all server-owned storage. Must be dedicated to this server
    /// (see `storage::Storage::open`).
    pub data_root: PathBuf,
    /// Upload cap for the image category (proposal §I.2: 50 MiB).
    pub max_upload_bytes: u64,
    /// Uploads being streamed at the same time, server-wide. A request that
    /// arrives when they are all busy is rejected with `SERVER_BUSY` rather
    /// than queued, so no connection waits on a full server.
    pub max_concurrent_uploads: usize,
    /// How long an upload body may stall between two chunks before the
    /// request is abandoned and its partial file deleted. This is an IDLE
    /// timeout, not a total one: a slow but progressing upload of any
    /// allowed size still succeeds.
    pub upload_idle_timeout: Duration,
    /// Cap on a produced output file.
    pub max_output_bytes: u64,
    /// Bytes of uploads a single session may hold at once.
    pub session_quota_bytes: u64,
    pub max_files_per_session: usize,
    /// Jobs of one session executing concurrently (proposal: 2).
    pub max_running_per_session: usize,
    /// Non-terminal (queued + running) jobs per session (proposal: 20).
    pub max_active_jobs_per_session: usize,
    /// Non-terminal jobs server-wide (proposal: 200).
    pub max_active_jobs_global: usize,
    /// Concurrent native-image workers server-wide (`light` class).
    pub worker_permits: usize,
    /// Unused uploads are deleted after this long.
    pub upload_ttl: Duration,
    /// Terminal jobs (and their outputs) are deleted this long after
    /// finishing. There is no user-visible history beyond it.
    pub job_ttl: Duration,
    /// Sessions with no files/jobs are forgotten after this idle time.
    pub session_idle_ttl: Duration,
    pub max_sessions: usize,
    pub janitor_interval: Duration,
    /// Adds `Secure` to the session cookie. Off for plain-http local dev.
    pub secure_cookie: bool,
}

impl Config {
    /// Local-development defaults rooted at `data_root`.
    pub fn development(data_root: PathBuf) -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        Config {
            bind: SocketAddr::from(([127, 0, 0, 1], 8787)),
            data_root,
            max_upload_bytes: 50 * MIB,
            max_concurrent_uploads: 16,
            upload_idle_timeout: Duration::from_secs(30),
            max_output_bytes: 256 * MIB,
            session_quota_bytes: 2 * 1024 * MIB,
            max_files_per_session: 100,
            max_running_per_session: 2,
            max_active_jobs_per_session: 20,
            max_active_jobs_global: 200,
            worker_permits: (cores / 2).clamp(1, 4),
            upload_ttl: Duration::from_secs(60 * 60),
            job_ttl: Duration::from_secs(60 * 60),
            session_idle_ttl: Duration::from_secs(12 * 60 * 60),
            max_sessions: 10_000,
            janitor_interval: Duration::from_secs(60),
            secure_cookie: false,
        }
    }

    /// Reads `MEB_BIND`, `MEB_DATA_ROOT`, `MEB_MAX_UPLOAD_MB`,
    /// `MEB_MAX_CONCURRENT_UPLOADS`, `MEB_UPLOAD_IDLE_TIMEOUT_SECS`,
    /// `MEB_SECURE_COOKIE`. Unset values use `development` defaults.
    pub fn from_env() -> Result<Self, String> {
        let data_root = match std::env::var_os("MEB_DATA_ROOT") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => std::env::temp_dir().join("meb-donustur-web").join("data"),
        };
        let mut config = Config::development(data_root);

        if let Ok(bind) = std::env::var("MEB_BIND") {
            config.bind = bind
                .parse()
                .map_err(|_| format!("MEB_BIND is not a valid socket address: {bind}"))?;
        }
        if !config.bind.ip().is_loopback() {
            return Err(
                "MEB_BIND must be a loopback address (127.0.0.1 / ::1): this development \
                 server has no authentication and must not be reachable from the network"
                    .to_string(),
            );
        }
        if let Ok(mb) = std::env::var("MEB_MAX_UPLOAD_MB") {
            let mb: u64 = mb
                .parse()
                .map_err(|_| "MEB_MAX_UPLOAD_MB must be a number".to_string())?;
            if !(1..=2048).contains(&mb) {
                return Err("MEB_MAX_UPLOAD_MB must be between 1 and 2048".to_string());
            }
            config.max_upload_bytes = mb * MIB;
        }
        if let Ok(n) = std::env::var("MEB_MAX_CONCURRENT_UPLOADS") {
            let n: usize = n
                .parse()
                .map_err(|_| "MEB_MAX_CONCURRENT_UPLOADS must be a number".to_string())?;
            if !(1..=1024).contains(&n) {
                return Err("MEB_MAX_CONCURRENT_UPLOADS must be between 1 and 1024".to_string());
            }
            config.max_concurrent_uploads = n;
        }
        if let Ok(secs) = std::env::var("MEB_UPLOAD_IDLE_TIMEOUT_SECS") {
            let secs: u64 = secs
                .parse()
                .map_err(|_| "MEB_UPLOAD_IDLE_TIMEOUT_SECS must be a number".to_string())?;
            if !(1..=3600).contains(&secs) {
                return Err("MEB_UPLOAD_IDLE_TIMEOUT_SECS must be between 1 and 3600".to_string());
            }
            config.upload_idle_timeout = Duration::from_secs(secs);
        }
        config.secure_cookie = std::env::var("MEB_SECURE_COOKIE")
            .map(|v| v == "1")
            .unwrap_or(false);
        Ok(config)
    }
}
