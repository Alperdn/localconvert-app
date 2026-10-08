//! The mechanism behind "which real executable backs an `EngineId`".
//!
//! Resolution order is fixed here and is the same for every consumer:
//!
//! 1. **Bundled** - `<engines root>/<engine dir>/<exe>`, where the root
//!    comes from the consumer's policy. The engine directory name and the
//!    executable's relative path come only from the closed `EngineId`
//!    enum, and the join is containment-checked anyway.
//! 2. **Configured** - a fixed, non-user-writable institutional override
//!    (e.g. placed by IT deployment tooling). Never read from anything a
//!    Tauri command, an HTTP request or the frontend can influence.
//! 3. **System-installed** - PATH and/or well-known install locations.
//!
//! What this module deliberately does NOT decide is *where* those tiers
//! live, or which of them a given product is allowed to use: that is
//! product policy, and it differs between the desktop app (which stages
//! engines next to its own executable and refuses a system LibreOffice
//! unless a developer opts in) and the server. Each consumer therefore
//! supplies a `ResolutionPolicy` of `fn` pointers. Because those are plain
//! function pointers chosen at the call site, there is no global, no
//! setter and no environment variable that can repoint resolution at
//! runtime.

use crate::engine_id::EngineId;
use crate::error::EngineError;
use std::path::{Path, PathBuf};

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
    /// Which resolution tier produced `path` - load-bearing for
    /// diagnostics ("using a bundled engine" vs. "using your system
    /// install") and asserted by this crate's own tests.
    pub tier: EngineTier,
}

/// Looks something up for one engine. A plain `fn` - not a closure or a
/// trait object - so a policy is a compile-time constant with no captured
/// state anything could mutate later.
pub type Locator = fn(EngineId) -> Option<PathBuf>;

/// One consumer's resolution policy. A `None` tier is simply skipped.
#[derive(Clone, Copy)]
pub struct ResolutionPolicy {
    /// The *engines root* this engine would be bundled under (not the
    /// executable): `bundled_path_in` appends the engine's own directory
    /// and executable name.
    pub bundled_root: Option<Locator>,
    pub configured: Option<Locator>,
    pub system: Option<Locator>,
}

impl ResolutionPolicy {
    /// Nothing resolves. The starting point a consumer adds tiers to.
    pub const EMPTY: ResolutionPolicy = ResolutionPolicy {
        bundled_root: None,
        configured: None,
        system: None,
    };

    /// Engines staged next to the running executable first, then whatever
    /// is on `PATH`. The policy a server process uses: it has no
    /// institutional override tier and no per-engine opt-outs.
    pub const BUNDLED_THEN_PATH: ResolutionPolicy = ResolutionPolicy {
        bundled_root: Some(exe_relative_engines_root),
        configured: None,
        system: Some(path_lookup),
    };
}

/// `<dir of the running executable>/engines` - the layout an installed
/// build ships. Harmlessly absent in a dev/test build, where it simply
/// makes the bundled tier a miss.
pub fn exe_relative_engines_root(_id: EngineId) -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.join("engines"))
}

/// The engine's executable inside `root`, or `None` if it is not there.
///
/// Defense in depth: the directory name and the relative executable path
/// are derived only from the fixed `EngineId` enum, never from external
/// input, so neither can actually escape `root` today. The invariant is
/// asserted anyway rather than trusting every future call site to
/// preserve it forever.
pub fn bundled_path_in(root: &Path, id: EngineId) -> Option<PathBuf> {
    let dir = root.join(id.bundle_dir_name());
    if !dir.starts_with(root) {
        return None;
    }
    let candidate = dir.join(id.bundled_exe_name());
    if !candidate.starts_with(root) {
        return None;
    }
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

/// `<engines root>/<engine dir>` for a bundled engine, existence not
/// checked - for callers that need the directory itself (a manifest, a
/// model file) rather than the executable.
pub fn bundled_engine_dir(id: EngineId, policy: &ResolutionPolicy) -> Option<PathBuf> {
    let root = (policy.bundled_root?)(id)?;
    let dir = root.join(id.bundle_dir_name());
    dir.starts_with(&root).then_some(dir)
}

/// The default system tier: a plain `PATH` search for the engine's
/// platform executable name. An engine with no system identity
/// (`system_exe_name() == None` - bundled-only, or resolved relative to
/// another engine) is never found this way.
pub fn path_lookup(id: EngineId) -> Option<PathBuf> {
    let name = id.system_exe_name()?;
    which::which(name).ok()
}

/// Resolves `id` through `policy`'s tiers in order, or a structured,
/// path-free `EngineError::not_available` if none of them found one.
pub fn resolve_with(
    id: EngineId,
    policy: &ResolutionPolicy,
) -> Result<ResolvedEngine, EngineError> {
    if let Some(root_of) = policy.bundled_root {
        if let Some(path) = root_of(id).and_then(|root| bundled_path_in(&root, id)) {
            return Ok(ResolvedEngine {
                id,
                path,
                tier: EngineTier::Bundled,
            });
        }
    }
    if let Some(configured) = policy.configured {
        if let Some(path) = configured(id) {
            return Ok(ResolvedEngine {
                id,
                path,
                tier: EngineTier::Configured,
            });
        }
    }
    if let Some(system) = policy.system {
        if let Some(path) = system(id) {
            return Ok(ResolvedEngine {
                id,
                path,
                tier: EngineTier::System,
            });
        }
    }
    Err(EngineError::not_available(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_lookup_never_escapes_the_engines_root() {
        let root = Path::new(if cfg!(windows) { "C:/engines" } else { "/engines" });
        for id in EngineId::ALL {
            let dir = root.join(id.bundle_dir_name());
            assert!(
                dir.starts_with(root),
                "{id:?} bundle dir escaped the engines root"
            );
            let exe = dir.join(id.bundled_exe_name());
            assert!(
                exe.starts_with(root),
                "{id:?} bundled executable escaped the engines root"
            );
        }
    }

    #[test]
    fn an_empty_policy_resolves_nothing() {
        for id in EngineId::ALL {
            let err = resolve_with(id, &ResolutionPolicy::EMPTY).unwrap_err();
            assert_eq!(
                err.code(),
                format!("{}_ENGINE_NOT_AVAILABLE", id.code_prefix())
            );
        }
    }

    #[test]
    fn tiers_are_tried_in_order_and_report_which_one_won() {
        fn configured(_: EngineId) -> Option<PathBuf> {
            Some(PathBuf::from("configured-engine"))
        }
        fn system(_: EngineId) -> Option<PathBuf> {
            Some(PathBuf::from("system-engine"))
        }
        // The bundled tier is a miss here (no such directory exists), so
        // configured must win over system - and system must win once
        // configured is absent.
        let policy = ResolutionPolicy {
            bundled_root: Some(|_| Some(PathBuf::from("no-such-engines-root"))),
            configured: Some(configured),
            system: Some(system),
        };
        let resolved = resolve_with(EngineId::Ghostscript, &policy).unwrap();
        assert_eq!(resolved.tier, EngineTier::Configured);
        assert_eq!(resolved.path, PathBuf::from("configured-engine"));

        let system_only = ResolutionPolicy {
            configured: None,
            ..policy
        };
        let resolved = resolve_with(EngineId::Ghostscript, &system_only).unwrap();
        assert_eq!(resolved.tier, EngineTier::System);
    }

    #[test]
    fn a_bundled_only_engine_is_never_found_on_path() {
        for id in [EngineId::Speech, EngineId::AudioFfmpeg, EngineId::AudioFfprobe] {
            assert!(id.system_exe_name().is_none());
            assert!(path_lookup(id).is_none());
        }
    }
}
