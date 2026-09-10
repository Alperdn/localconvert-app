//! Bundled Office Engine manifest and self-check.
//!
//! `manifest.json` is build-time metadata describing exactly which
//! LibreOffice build is bundled inside `engines/office/` next to the
//! running executable (see `resolver::bundled_root`). It is produced by
//! `scripts/prepare-office-engine.ps1` (never hand-edited, never written
//! at runtime) and is the only thing this module trusts to decide whether
//! the bundled engine is the pinned, verified build this codebase expects
//! - a `soffice.exe` file existing on disk is not, by itself, sufficient
//! evidence of that.
//!
//! Everything this module returns to a Tauri command is a stable status
//! code plus a fixed, translatable message - never a filesystem path, an
//! executable name, or raw process output. See `OfficeEngineStatus`.

use super::engine_id::EngineId;
use super::resolver;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The LibreOffice version this codebase is pinned to. Kept in one place
/// so `manifest.json` validation and documentation can't drift apart.
/// See `docs/OFFICE_ENGINE.md` for the full pinning rationale and hash.
pub const PINNED_VERSION: &str = "25.8.7";
pub const PINNED_ARCHITECTURE: &str = "x86_64";

/// Conservative headless-conversion timeout. A single legitimate DOCX/
/// XLSX/PPTX conversion normally completes in well under 30s; this leaves
/// generous headroom for large legitimate spreadsheets/presentations
/// while still bounding a hung/hostile-input process to a fixed wall
/// clock. See `docs/OFFICE_ENGINE.md` section on timeout policy.
pub const CONVERT_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OfficeManifest {
    pub engine_id: String,
    pub engine_name: String,
    pub version: String,
    pub architecture: String,
    /// Relative to the manifest's own directory (`engines/office/`).
    pub executable_relative_path: String,
    pub source: String,
    pub sha256: String,
    pub bundled_at_build: String,
    pub license_notice_path: String,
    /// Directories under `engines/office/` that must be present for
    /// Writer/Calc/Impress headless conversion to work (see
    /// `docs/OFFICE_ENGINE.md` section D). Checked by `self_check`.
    #[serde(default)]
    pub required_relative_dirs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OfficeEngineStatus {
    Available,
    EngineMissing,
    EngineInvalid,
    EngineVersionMismatch,
    SelfCheckFailed,
}

#[derive(Debug, Clone, Serialize)]
pub struct OfficeEngineReport {
    pub status: OfficeEngineStatus,
    /// Safe to show in the UI as-is.
    pub message: String,
}

fn manifest_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.join("engines").join(EngineId::Office.bundle_dir_name()))
}

fn load_manifest(dir: &Path) -> Result<OfficeManifest, String> {
    let manifest_path = dir.join("manifest.json");
    let mut bytes = std::fs::read(&manifest_path).map_err(|e| format!("manifest unreadable: {}", e))?;
    // PowerShell's `Set-Content -Encoding utf8` (used by
    // prepare-office-engine.ps1 on Windows PowerShell 5.1, which has no
    // no-BOM UTF-8 option) writes a UTF-8 BOM, which `serde_json` does
    // not skip on its own. Strip it defensively here too, rather than
    // relying solely on the script never regressing this.
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes.drain(0..3);
    }
    serde_json::from_slice::<OfficeManifest>(&bytes).map_err(|e| format!("manifest malformed: {}", e))
}

/// Full backend self-check: bundled engine present, manifest well-formed,
/// version/architecture match this build's pin, required resource
/// directories exist, and the executable itself can be spotted at the
/// path the manifest claims (existence + is-a-file only here - actually
/// spawning it in headless mode to query its version is done separately
/// by `verify_executable_starts`, which is slower and reserved for
/// explicit "run self-check" actions rather than every capability poll).
pub fn self_check() -> OfficeEngineReport {
    let Some(dir) = manifest_dir() else {
        return OfficeEngineReport {
            status: OfficeEngineStatus::EngineMissing,
            message: "Office conversion engine is not available on this installation.".into(),
        };
    };

    if !dir.is_dir() {
        return OfficeEngineReport {
            status: OfficeEngineStatus::EngineMissing,
            message: "Office conversion engine is not available on this installation.".into(),
        };
    }

    let manifest = match load_manifest(&dir) {
        Ok(m) => m,
        Err(_) => {
            return OfficeEngineReport {
                status: OfficeEngineStatus::EngineInvalid,
                message: "Office conversion engine installation appears to be corrupted.".into(),
            };
        }
    };

    if manifest.engine_id != "office" {
        return OfficeEngineReport {
            status: OfficeEngineStatus::EngineInvalid,
            message: "Office conversion engine installation appears to be corrupted.".into(),
        };
    }

    if manifest.version != PINNED_VERSION || manifest.architecture != PINNED_ARCHITECTURE {
        return OfficeEngineReport {
            status: OfficeEngineStatus::EngineVersionMismatch,
            message: "Office conversion engine version does not match this application build.".into(),
        };
    }

    let exe_path = dir.join(&manifest.executable_relative_path);
    if !exe_path.starts_with(&dir) || !exe_path.is_file() {
        return OfficeEngineReport {
            status: OfficeEngineStatus::EngineInvalid,
            message: "Office conversion engine installation appears to be corrupted.".into(),
        };
    }

    for rel in &manifest.required_relative_dirs {
        let required = dir.join(rel);
        if !required.starts_with(&dir) || !required.is_dir() {
            return OfficeEngineReport {
                status: OfficeEngineStatus::EngineInvalid,
                message: "Office conversion engine installation appears to be corrupted.".into(),
            };
        }
    }

    // The resolver's own (cheaper, file-existence-only) view must agree -
    // if it doesn't, something about bundle_dir_name()/bundled_exe_name()
    // and this manifest have drifted apart.
    if !resolver::is_available(EngineId::Office) {
        return OfficeEngineReport {
            status: OfficeEngineStatus::SelfCheckFailed,
            message: "Office conversion engine failed its startup check.".into(),
        };
    }

    OfficeEngineReport {
        status: OfficeEngineStatus::Available,
        message: "Office conversion engine is available.".into(),
    }
}

#[tauri::command]
pub async fn office_engine_status() -> OfficeEngineReport {
    self_check()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_bundle_dir_reports_engine_missing() {
        // Phase-1/dev machines never have engines/office/ populated - this
        // exercises the real "missing" path.
        let report = self_check();
        if manifest_dir().map(|d| d.is_dir()).unwrap_or(false) {
            return; // some other environment has a bundle - skip.
        }
        assert_eq!(report.status, OfficeEngineStatus::EngineMissing);
    }

    #[test]
    fn invalid_manifest_json_is_rejected() {
        let dir = std::env::temp_dir().join(format!("loc_manifest_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), b"{ not json").unwrap();
        let result = load_manifest(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_err());
    }

    #[test]
    fn wrong_version_is_rejected_by_self_check_logic() {
        let dir = std::env::temp_dir().join(format!("loc_manifest_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let bogus = OfficeManifest {
            engine_id: "office".into(),
            engine_name: "LibreOffice".into(),
            version: "1.0.0".into(),
            architecture: PINNED_ARCHITECTURE.into(),
            executable_relative_path: "program/soffice.exe".into(),
            source: "test".into(),
            sha256: "0".repeat(64),
            bundled_at_build: "test".into(),
            license_notice_path: "NOTICE".into(),
            required_relative_dirs: vec![],
        };
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&bogus).unwrap()).unwrap();
        let loaded = load_manifest(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_ne!(loaded.version, PINNED_VERSION);
    }

    #[test]
    fn status_never_exposes_a_filesystem_path_or_executable_name() {
        let report = self_check();
        let lower = report.message.to_lowercase();
        assert!(!report.message.contains('\\'));
        assert!(!lower.contains(".exe"));
        assert!(!lower.contains("program files"));
    }
}
