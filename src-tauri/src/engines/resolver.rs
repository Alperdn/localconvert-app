//! The single place in the codebase that decides which real executable
//! backs an `EngineId`.
//!
//! Resolution order matches the long-term bundled-engine architecture even
//! though only the last tier does anything today:
//!
//! 1. **Bundled** - `<install dir>/engines/<engine>/<exe>`, resolved
//!    relative to the running executable. Not populated by this build yet
//!    (no engines are bundled), so this tier is currently always a miss -
//!    but the lookup itself is real and wired in, so bundling an engine
//!    later is a matter of dropping files in place, not touching this
//!    function's callers.
//! 2. **Configured** - a future fixed, non-user-writable institutional
//!    override (e.g. placed by IT deployment tooling). Not implemented
//!    yet: always `None`. Deliberately NOT read from anything a Tauri
//!    command or the frontend can influence.
//! 3. **System-installed** - PATH + common install locations, via the
//!    existing scanner in `tools.rs`. This is the only tier that can
//!    succeed today, and it is intentionally isolated behind this module
//!    so callers never call `tools::get_tool_path`/`which` directly for
//!    engines managed here.
//!
//! Once an engine is actually bundled, only `bundled_path` starts
//! returning `Some` - no caller of `resolve()` needs to change.

use super::engine_id::EngineId;
use super::error::EngineError;
use crate::tools;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineTier {
    Bundled,
    Configured,
    System,
}

#[derive(Debug, Clone)]
pub struct ResolvedEngine {
    pub id: EngineId,
    pub path: PathBuf,
    /// Which resolution tier produced `path` - not consumed by any caller
    /// yet, but load-bearing for future diagnostics (e.g. surfacing
    /// "using a bundled engine" vs. "using your system install" once a UI
    /// exists for it) and already exercised by `process.rs`'s tests.
    #[allow(dead_code)]
    pub tier: EngineTier,
}

/// Root directory a future bundled-engine distribution would live under.
/// Resolved relative to the running executable so it works both from an
/// installed location and, harmlessly (it simply won't exist), in dev.
fn bundled_root() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.join("engines"))
}

fn bundled_path(id: EngineId) -> Option<PathBuf> {
    let root = bundled_root()?;
    let dir = root.join(id.bundle_dir_name());

    // Defense in depth: the directory name is derived only from the fixed
    // `EngineId` enum, never from external input, so this can't actually
    // escape `root` today. Assert the invariant anyway rather than
    // trusting every future call site to preserve it forever.
    if !dir.starts_with(&root) {
        return None;
    }

    let candidate = dir.join(id.bundled_exe_name());
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

/// Reserved for a future admin/institutional override - e.g. read from a
/// fixed config file placed by IT deployment tooling at install time.
/// Never a path typed into the app by an end user, and never reachable
/// from a Tauri command. Not implemented yet: always `None`.
fn configured_path(_id: EngineId) -> Option<PathBuf> {
    None
}

fn system_path(id: EngineId) -> Option<PathBuf> {
    match id {
        EngineId::Ffprobe => {
            // ffprobe ships next to ffmpeg in every common distribution;
            // there is no independent "ffprobe" entry in tools.rs's
            // scanner, so derive it from wherever ffmpeg was actually
            // found instead of running a second, less accurate search
            // for a bare "ffprobe" name.
            let ffmpeg = system_path(EngineId::Ffmpeg)?;
            let file_name = ffmpeg.file_name()?.to_str()?;
            let ffprobe_name = if file_name.eq_ignore_ascii_case("ffmpeg.exe") {
                "ffprobe.exe"
            } else if file_name.eq_ignore_ascii_case("ffmpeg") {
                "ffprobe"
            } else {
                return None;
            };
            Some(ffmpeg.with_file_name(ffprobe_name))
        }
        _ => {
            let key = id.system_tool_key()?;
            let status = tools::check_tool_installed(key);
            if status.installed {
                status.path.map(PathBuf::from)
            } else {
                None
            }
        }
    }
}

/// Resolves `id` to a concrete, spawnable path, or a structured
/// `EngineError::not_available` if none of the three tiers found one.
pub fn resolve(id: EngineId) -> Result<ResolvedEngine, EngineError> {
    if let Some(path) = bundled_path(id) {
        return Ok(ResolvedEngine {
            id,
            path,
            tier: EngineTier::Bundled,
        });
    }
    if let Some(path) = configured_path(id) {
        return Ok(ResolvedEngine {
            id,
            path,
            tier: EngineTier::Configured,
        });
    }
    if let Some(path) = system_path(id) {
        return Ok(ResolvedEngine {
            id,
            path,
            tier: EngineTier::System,
        });
    }
    Err(EngineError::not_available(id))
}

/// Cheap availability check for the capability model - avoids building a
/// full `ResolvedEngine` when the caller only needs a yes/no.
pub fn is_available(id: EngineId) -> bool {
    resolve(id).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_path_never_escapes_the_engines_root() {
        for id in EngineId::ALL {
            if let Some(root) = bundled_root() {
                let dir = root.join(id.bundle_dir_name());
                assert!(
                    dir.starts_with(&root),
                    "{:?} bundle dir escaped the engines root",
                    id
                );
            }
        }
    }

    #[test]
    fn office_missing_on_this_machine_yields_structured_not_available() {
        // Phase 1 policy for this repo: the dev/CI machine running this
        // suite never has LibreOffice installed, so this exercises the
        // real "missing" path rather than a mock. If some future
        // environment *does* have soffice on PATH, skip the negative
        // assertion instead of failing the suite over an environment fact
        // this test isn't about.
        if system_path(EngineId::Office).is_some() {
            return;
        }
        let err = resolve(EngineId::Office).unwrap_err();
        assert_eq!(err.code(), "OFFICE_ENGINE_NOT_AVAILABLE");
    }

    #[test]
    fn ffprobe_resolution_never_does_an_independent_path_search() {
        // If ffmpeg itself isn't found, ffprobe must not be found either -
        // there is no separate "ffprobe" entry in tools.rs's scanner that
        // could accidentally succeed on its own.
        if system_path(EngineId::Ffmpeg).is_none() {
            assert!(system_path(EngineId::Ffprobe).is_none());
        }
    }
}
