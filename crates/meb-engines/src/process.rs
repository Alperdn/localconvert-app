//! Centralized process spawning for the engine layer.
//!
//! This is the ONLY place a resolved engine binary is turned into a
//! `std::process::Command` for anything routed through `engines::*`.
//! Every invariant below is enforced once, here, rather than re-derived at
//! each call site:
//!
//! - No shell is ever involved. `Command::new(path)` execs the resolved
//!   binary directly - there is no `cmd /c`, `sh -c`, or
//!   `powershell -Command` anywhere in this function or anything it calls.
//! - The executable identity is exactly the path `resolver::resolve`
//!   returned - never a string built from caller- or frontend-supplied
//!   input.
//! - Caller-supplied data only ever appears as argument *values*, passed
//!   as an array (`Command::args`), never interpolated into a command
//!   string.
//! - The process's working directory is always the caller's isolated job
//!   directory, never the engine's own install/bundle folder - so a
//!   compromised or unexpected file sitting next to the engine binary
//!   can't be picked up as an implicit relative-path input.

use super::error::EngineError;
use super::resolver::ResolvedEngine;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

/// Environment variables that can redirect an engine's behavior toward
/// attacker-controlled config/data (a plugin path, a fake tessdata
/// directory, a substituted delegate library, ...) if inherited from an
/// untrusted ambient environment. Stripped before every engine spawn,
/// regardless of which engine it is - full environment minimization is
/// deferred until engines are actually bundled (see `resolver.rs`), at
/// which point PATH itself stops being relevant for these binaries too.
const ENV_DENYLIST: &[&str] = &[
    // ggml can load extra backend DLLs from this path.
    "GGML_BACKEND_PATH",
    // ffmpeg writes a log report to this location/config.
    "FFREPORT",
    "TESSDATA_PREFIX",
    "MAGICK_CONFIGURE_PATH",
    "MAGICK_HOME",
    "GS_LIB",
    "FONTCONFIG_PATH",
    "PYTHONPATH",
];

/// Builds (but does not run) the `Command` for a resolved engine.
pub fn build_command(resolved: &ResolvedEngine, args: &[String], job_dir: &Path) -> Command {
    build_command_ex(resolved, args, job_dir, false)
}

/// Same as `build_command`; `low_priority` starts the child at below-normal
/// CPU priority (used for long CPU-bound work such as speech recognition so
/// the UI and the rest of the machine stay responsive).
pub fn build_command_ex(
    resolved: &ResolvedEngine,
    args: &[String],
    job_dir: &Path,
    low_priority: bool,
) -> Command {
    let mut cmd = Command::new(&resolved.path);
    cmd.args(args);
    cmd.current_dir(job_dir);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    for var in ENV_DENYLIST {
        cmd.env_remove(var);
    }

    #[cfg(windows)]
    cmd.creation_flags(if low_priority {
        CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
    } else {
        CREATE_NO_WINDOW
    });
    #[cfg(not(windows))]
    let _ = low_priority;

    cmd
}

/// How a cancellable run ended. Never carries a path; `stderr_tail` is for
/// development diagnostics only (never shown to users).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEnd {
    Success,
    /// Exited non-zero (`None` = terminated without a code).
    Failed(Option<i32>),
    Cancelled,
    TimedOut,
    SpawnFailed,
}

#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub end: ProcessEnd,
    pub stdout: String,
    pub stderr_tail: String,
}

pub struct CancellableRun<'a> {
    pub timeout: Option<Duration>,
    pub cancel: &'a std::sync::atomic::AtomicBool,
    pub low_priority: bool,
    /// Called for every stderr line (split on CR or LF) from a reader thread.
    pub on_stderr_line: Option<&'a (dyn Fn(&str) + Sync)>,
}

const STDOUT_CAP: usize = 8 * 1024 * 1024;
const STDERR_TAIL_CAP: usize = 64 * 1024;

/// Runs an engine with a cancel flag and optional timeout. Both pipes are
/// drained on their own threads (no pipe-buffer deadlock for chatty tools).
/// On cancel/timeout the child is killed and reaped before returning, so
/// nothing is left running and any files it held open are released.
pub fn run_cancellable(
    resolved: &ResolvedEngine,
    args: &[String],
    job_dir: &Path,
    opts: CancellableRun<'_>,
) -> ProcessResult {
    use std::io::Read;
    use std::sync::atomic::Ordering;

    let mut child = match build_command_ex(resolved, args, job_dir, opts.low_priority).spawn() {
        Ok(c) => c,
        Err(e) => {
            return ProcessResult {
                end: ProcessEnd::SpawnFailed,
                stdout: String::new(),
                stderr_tail: e.to_string(),
            }
        }
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let start = Instant::now();

    std::thread::scope(|scope| {
        let out_h = scope.spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = stdout_pipe {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = p.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    if buf.len() < STDOUT_CAP {
                        buf.extend_from_slice(&chunk[..n.min(STDOUT_CAP - buf.len())]);
                    }
                }
            }
            String::from_utf8_lossy(&buf).to_string()
        });
        let cb = opts.on_stderr_line;
        let err_h = scope.spawn(move || {
            let mut tail = String::new();
            let mut line: Vec<u8> = Vec::new();
            if let Some(mut p) = stderr_pipe {
                let mut chunk = [0u8; 4096];
                while let Ok(n) = p.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    for &b in &chunk[..n] {
                        if b == b'\n' || b == b'\r' {
                            if !line.is_empty() {
                                let l = String::from_utf8_lossy(&line).to_string();
                                if let Some(f) = cb {
                                    f(&l);
                                }
                                tail.push_str(&l);
                                tail.push('\n');
                                if tail.len() > STDERR_TAIL_CAP {
                                    let cut = tail.len() - STDERR_TAIL_CAP;
                                    let cut = (cut..tail.len()).find(|i| tail.is_char_boundary(*i)).unwrap_or(0);
                                    tail.drain(..cut);
                                }
                                line.clear();
                            }
                        } else if line.len() < 16 * 1024 {
                            line.push(b);
                        }
                    }
                }
            }
            tail
        });

        let end = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    break if status.success() {
                        ProcessEnd::Success
                    } else {
                        ProcessEnd::Failed(status.code())
                    };
                }
                Ok(None) => {
                    if opts.cancel.load(Ordering::SeqCst) {
                        let _ = child.kill();
                        let _ = child.wait();
                        break ProcessEnd::Cancelled;
                    }
                    if let Some(t) = opts.timeout {
                        if start.elapsed() >= t {
                            let _ = child.kill();
                            let _ = child.wait();
                            break ProcessEnd::TimedOut;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break ProcessEnd::Failed(None);
                }
            }
        };
        ProcessResult {
            end,
            stdout: out_h.join().unwrap_or_default(),
            stderr_tail: err_h.join().unwrap_or_default(),
        }
    })
}

/// Runs an engine to completion and maps failure into a structured,
/// path-free error. Stdout/stderr are captured only in memory for the
/// duration of this call and are never written to disk or logged
/// persistently - only attached as `technical_detail` on failure, which
/// `EngineError::Display` never surfaces.
#[allow(dead_code)]
pub fn run(resolved: &ResolvedEngine, args: &[String], job_dir: &Path) -> Result<(), EngineError> {
    let output = build_command(resolved, args, job_dir).output().map_err(|e| {
        EngineError::process_failed(
            resolved.id,
            format!("{} could not be started.", resolved.id.display_name()),
        )
        .with_detail(e.to_string())
    })?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(EngineError::process_failed(
            resolved.id,
            format!(
                "{} reported an error while processing this file.",
                resolved.id.display_name()
            ),
        )
        .with_detail(stderr))
    }
}

/// Same contract as `run`, but bounds the child process to `timeout` wall
/// clock. If the deadline is exceeded, the child is killed and reaped
/// (never left as a zombie/orphan) and the caller gets a structured
/// `EngineError::timeout` instead of hanging indefinitely.
///
/// This kills only the direct child process, not a full process tree. On
/// Windows, `soffice.exe` is itself the long-running process for a
/// headless `--convert-to` invocation in the common case, so this covers
/// the invocation this app makes - but it is not a general subprocess-tree
/// sandbox. See `docs/OFFICE_ENGINE.md` "Known limitations" for the
/// documented residual risk and the Windows Job Object hardening this
/// would need for a stronger guarantee.
pub fn run_with_timeout(
    resolved: &ResolvedEngine,
    args: &[String],
    job_dir: &Path,
    timeout: Duration,
) -> Result<(), EngineError> {
    // Not draining stdout/stderr while polling risks a pipe-buffer
    // deadlock for a process that writes a lot of output before exiting.
    // Acceptable here: headless `soffice --convert-to` produces minimal
    // stdout/stderr for the inputs this app sends it. A chattier future
    // engine on this path would need a background reader thread instead.
    let mut child = build_command(resolved, args, job_dir).spawn().map_err(|e| {
        EngineError::process_failed(
            resolved.id,
            format!("{} could not be started.", resolved.id.display_name()),
        )
        .with_detail(e.to_string())
    })?;

    let start = Instant::now();
    let poll_interval = Duration::from_millis(100);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut stderr = String::new();
                if let Some(mut s) = child.stderr.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut stderr);
                }
                return Err(EngineError::process_failed(
                    resolved.id,
                    format!(
                        "{} reported an error while processing this file.",
                        resolved.id.display_name()
                    ),
                )
                .with_detail(stderr));
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(EngineError::timeout(resolved.id));
                }
                std::thread::sleep(poll_interval);
            }
            Err(e) => {
                return Err(EngineError::process_failed(
                    resolved.id,
                    format!("{} could not be monitored.", resolved.id.display_name()),
                )
                .with_detail(e.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_id::EngineId;
    use crate::resolver::EngineTier;
    use std::path::PathBuf;

    #[test]
    #[cfg(windows)]
    fn run_with_timeout_kills_a_hung_process_and_reports_timeout() {
        // `ping` (present on every Windows install, no shell required to
        // resolve it - `Command::new` searches PATH itself) stands in for
        // a hung/malicious engine process here.
        let resolved = ResolvedEngine {
            id: EngineId::Office,
            path: PathBuf::from("ping"),
            tier: EngineTier::System,
        };
        let args = vec!["127.0.0.1".to_string(), "-n".to_string(), "30".to_string()];
        let job_dir = std::env::temp_dir();
        let start = std::time::Instant::now();
        let result = run_with_timeout(&resolved, &args, &job_dir, Duration::from_millis(300));
        assert!(start.elapsed() < Duration::from_secs(10), "timeout branch did not fire promptly");
        let err = result.unwrap_err();
        assert_eq!(err.code(), "OFFICE_TIMEOUT");
    }

    #[test]
    fn build_command_never_touches_a_shell() {
        // There is no way to assert "no shell" by inspecting a
        // `std::process::Command` directly (it doesn't expose that), so
        // this test documents and locks in the invariant structurally:
        // `build_command` must construct `Command::new` from the
        // resolved path alone, and the program name it reports back must
        // be exactly that path - never "cmd", "cmd.exe", "sh", or
        // "powershell".
        let resolved = ResolvedEngine {
            id: EngineId::Ffmpeg,
            path: PathBuf::from("ffmpeg"),
            tier: EngineTier::System,
        };
        let cmd = build_command(&resolved, &["-version".to_string()], Path::new("."));
        let program = cmd.get_program().to_string_lossy().to_lowercase();
        assert_eq!(program, "ffmpeg");
        assert!(!program.contains("cmd"));
        assert!(!program.contains("sh"));
        assert!(!program.contains("powershell"));
    }
}
