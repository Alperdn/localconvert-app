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

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Environment variables that can redirect an engine's behavior toward
/// attacker-controlled config/data (a plugin path, a fake tessdata
/// directory, a substituted delegate library, ...) if inherited from an
/// untrusted ambient environment. Stripped before every engine spawn,
/// regardless of which engine it is - full environment minimization is
/// deferred until engines are actually bundled (see `resolver.rs`), at
/// which point PATH itself stops being relevant for these binaries too.
const ENV_DENYLIST: &[&str] = &[
    "TESSDATA_PREFIX",
    "MAGICK_CONFIGURE_PATH",
    "MAGICK_HOME",
    "GS_LIB",
    "FONTCONFIG_PATH",
    "PYTHONPATH",
];

/// Builds (but does not run) the `Command` for a resolved engine.
pub fn build_command(resolved: &ResolvedEngine, args: &[String], job_dir: &Path) -> Command {
    let mut cmd = Command::new(&resolved.path);
    cmd.args(args);
    cmd.current_dir(job_dir);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    for var in ENV_DENYLIST {
        cmd.env_remove(var);
    }

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    cmd
}

/// Runs an engine to completion and maps failure into a structured,
/// path-free error. Stdout/stderr are captured only in memory for the
/// duration of this call and are never written to disk or logged
/// persistently - only attached as `technical_detail` on failure, which
/// `EngineError::Display` never surfaces.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::engine_id::EngineId;
    use crate::engines::resolver::EngineTier;
    use std::path::PathBuf;

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
