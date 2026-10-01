//! Manifest for the bundled Speech Engine (whisper.cpp + one ggml model).
//!
//! `engines/speech/manifest.json` is produced by
//! `scripts/prepare-speech-engine.ps1` - never hand-edited, never written at
//! runtime. Everything model-specific (id, hash, size, supported languages)
//! comes from here, NOT from Rust constants, so replacing small-q5_1 with
//! medium-q5_0 / large-v3-turbo-q5_0 is a manifest+file change only.
//!
//! What this module trusts: nothing on disk until its SHA-256 matches the
//! manifest. What the manifest itself is pinned to in Rust: schema version,
//! engine id, architecture (see `check_dir`).

use super::bundle::{self, ManifestFile, VerifyError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 2;
pub const PINNED_ARCHITECTURE: &str = "x86_64";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SpeechModelInfo {
    pub id: String,
    pub file_relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub source_revision: String,
    #[serde(default)]
    pub multilingual: bool,
    /// Language codes the UI may offer. `"auto"` = automatic detection.
    pub languages: Vec<String>,
    #[serde(default)]
    pub license: String,
}

/// Where the app-local Visual C++ runtime came from (informational; the
/// files themselves are hash-verified via `runtime_files`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuntimeInfo {
    pub name: String,
    pub version: String,
    pub source: String,
    #[serde(default)]
    pub deployment: String,
    #[serde(default)]
    pub license_notice_path: String,
}

/// One app-local runtime DLL (deployed unmodified beside the engine exe).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuntimeFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SpeechManifest {
    pub schema_version: u32,
    pub engine_id: String,
    pub engine_name: String,
    pub engine_version: String,
    pub architecture: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub source_archive_sha256: String,
    pub executable_relative_path: String,
    pub bin_files: Vec<ManifestFile>,
    /// VC++ runtime DLLs the engine imports, deployed app-local. Verified like
    /// every other bundled file; absent/tampered => the engine is not available.
    pub runtime: RuntimeInfo,
    pub runtime_files: Vec<RuntimeFile>,
    pub model: SpeechModelInfo,
    #[serde(default)]
    pub license: String,
}

impl SpeechManifest {
    pub fn model_file(&self) -> ManifestFile {
        ManifestFile {
            path: self.model.file_relative_path.clone(),
            sha256: self.model.sha256.clone(),
            size: self.model.size_bytes,
        }
    }
    pub fn supports_language(&self, code: &str) -> bool {
        self.model.languages.iter().any(|l| l == code)
            && (code != "auto" || self.model.multilingual)
    }
}

/// Fine-grained outcome of validating a speech engine directory. Never
/// carries a path - see `speech::errors` / `SpeechEngineReport` for the
/// user-facing (Turkish) text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ManifestCheck {
    EngineMissing,
    ModelMissing,
    /// An app-local runtime DLL listed in the manifest is absent.
    RuntimeMissing,
    /// Manifest unreadable/malformed/wrong id/schema, or a listed file's
    /// hash/size differs, or a manifest path is unsafe.
    Corrupt,
    ArchitectureUnsupported,
}

pub fn load_manifest(dir: &Path) -> Result<SpeechManifest, ManifestCheck> {
    let bytes = std::fs::read(dir.join("manifest.json")).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ManifestCheck::EngineMissing
        } else {
            ManifestCheck::Corrupt
        }
    })?;
    serde_json::from_slice::<SpeechManifest>(&bundle::strip_bom(bytes)).map_err(|_| ManifestCheck::Corrupt)
}

/// Validates `dir` (an `engines/speech/`-shaped directory): manifest shape,
/// pinned identity, architecture, and the SHA-256 of every executable/DLL and
/// of the model. Pure with respect to `dir` so tests can point at fixtures.
pub fn check_dir(dir: &Path) -> Result<SpeechManifest, ManifestCheck> {
    if !dir.is_dir() {
        return Err(ManifestCheck::EngineMissing);
    }
    let m = load_manifest(dir)?;
    if m.engine_id != "speech" || m.schema_version != SCHEMA_VERSION {
        return Err(ManifestCheck::Corrupt);
    }
    if m.architecture != PINNED_ARCHITECTURE || !cfg!(target_arch = "x86_64") {
        return Err(ManifestCheck::ArchitectureUnsupported);
    }
    if m.model.languages.is_empty() || m.model.id.trim().is_empty() {
        return Err(ManifestCheck::Corrupt);
    }
    // The executable must be one of the hashed files - otherwise the
    // manifest would vouch for everything except the thing that runs.
    if !m.bin_files.iter().any(|f| f.path == m.executable_relative_path) {
        return Err(ManifestCheck::Corrupt);
    }
    match bundle::verify_files(dir, &m.bin_files) {
        Ok(()) => {}
        Err(VerifyError::Missing) => return Err(ManifestCheck::EngineMissing),
        Err(VerifyError::Corrupt) => return Err(ManifestCheck::Corrupt),
    }
    // App-local VC++ runtime: must be declared, live in bin/ next to the exe
    // (that is where the Windows loader finds them first), and hash-match.
    if m.runtime_files.is_empty() {
        return Err(ManifestCheck::Corrupt);
    }
    let runtime: Vec<ManifestFile> = m
        .runtime_files
        .iter()
        .map(|r| ManifestFile { path: r.path.clone(), sha256: r.sha256.clone(), size: r.size })
        .collect();
    for r in &runtime {
        let lower = r.path.to_lowercase();
        if !lower.starts_with("bin/") || !lower.ends_with(".dll") || lower[4..].contains('/') {
            return Err(ManifestCheck::Corrupt);
        }
    }
    match bundle::verify_files(dir, &runtime) {
        Ok(()) => {}
        Err(VerifyError::Missing) => return Err(ManifestCheck::RuntimeMissing),
        Err(VerifyError::Corrupt) => return Err(ManifestCheck::Corrupt),
    }
    match bundle::verify_files(dir, &[m.model_file()]) {
        Ok(()) => {}
        Err(VerifyError::Missing) => return Err(ManifestCheck::ModelMissing),
        Err(VerifyError::Corrupt) => return Err(ManifestCheck::Corrupt),
    }
    Ok(m)
}

pub fn model_path(dir: &Path, m: &SpeechManifest) -> Option<PathBuf> {
    bundle::safe_join(dir, &m.model.file_relative_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(b: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(b).iter().map(|x| format!("{:02x}", x)).collect()
    }

    /// Builds a minimal fake engine dir and returns (dir, manifest).
    fn fixture(mutate: impl FnOnce(&mut SpeechManifest)) -> (PathBuf, SpeechManifest) {
        let d = std::env::temp_dir().join(format!("meb_speech_mf_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::create_dir_all(d.join("models")).unwrap();
        std::fs::write(d.join("bin/whisper-cli.exe"), b"exe").unwrap();
        std::fs::write(d.join("bin/vcruntime140.dll"), b"rt").unwrap();
        std::fs::write(d.join("models/m.bin"), b"model").unwrap();
        let mut m = SpeechManifest {
            schema_version: SCHEMA_VERSION,
            engine_id: "speech".into(),
            engine_name: "whisper.cpp".into(),
            engine_version: "v0".into(),
            architecture: PINNED_ARCHITECTURE.into(),
            source: String::new(),
            source_archive_sha256: String::new(),
            executable_relative_path: "bin/whisper-cli.exe".into(),
            bin_files: vec![ManifestFile { path: "bin/whisper-cli.exe".into(), sha256: sha(b"exe"), size: 3 }],
            runtime: RuntimeInfo {
                name: "test runtime".into(),
                version: "1".into(),
                source: "test".into(),
                deployment: "app-local".into(),
                license_notice_path: String::new(),
            },
            runtime_files: vec![RuntimeFile { path: "bin/vcruntime140.dll".into(), sha256: sha(b"rt"), size: 2, version: "1".into() }],
            model: SpeechModelInfo {
                id: "test".into(),
                file_relative_path: "models/m.bin".into(),
                sha256: sha(b"model"),
                size_bytes: 5,
                source: String::new(),
                source_revision: String::new(),
                multilingual: true,
                languages: vec!["tr".into(), "auto".into()],
                license: "MIT".into(),
            },
            license: "MIT".into(),
        };
        mutate(&mut m);
        std::fs::write(d.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        (d, m)
    }

    fn cleanup(d: PathBuf) {
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn valid_fixture_passes() {
        let (d, _) = fixture(|_| {});
        assert!(check_dir(&d).is_ok());
        cleanup(d);
    }

    #[test]
    fn missing_engine_dir_and_manifest_are_engine_missing() {
        assert_eq!(check_dir(Path::new("Z:/definitely/not/here")).unwrap_err(), ManifestCheck::EngineMissing);
        let d = std::env::temp_dir().join(format!("meb_speech_mf_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(check_dir(&d).unwrap_err(), ManifestCheck::EngineMissing);
        cleanup(d);
    }

    #[test]
    fn missing_model_is_distinct_from_missing_engine() {
        let (d, _) = fixture(|_| {});
        std::fs::remove_file(d.join("models/m.bin")).unwrap();
        assert_eq!(check_dir(&d).unwrap_err(), ManifestCheck::ModelMissing);
        cleanup(d);
    }

    #[test]
    fn tampered_model_or_exe_is_corrupt() {
        let (d, _) = fixture(|_| {});
        std::fs::write(d.join("models/m.bin"), b"MODEL").unwrap();
        assert_eq!(check_dir(&d).unwrap_err(), ManifestCheck::Corrupt);
        let (d2, _) = fixture(|_| {});
        std::fs::write(d2.join("bin/whisper-cli.exe"), b"EXE").unwrap();
        assert_eq!(check_dir(&d2).unwrap_err(), ManifestCheck::Corrupt);
        cleanup(d);
        cleanup(d2);
    }

    #[test]
    fn malformed_manifest_wrong_id_and_wrong_arch_are_rejected() {
        let (d, _) = fixture(|_| {});
        std::fs::write(d.join("manifest.json"), b"{ nope").unwrap();
        assert_eq!(check_dir(&d).unwrap_err(), ManifestCheck::Corrupt);
        let (d2, _) = fixture(|m| m.engine_id = "office".into());
        assert_eq!(check_dir(&d2).unwrap_err(), ManifestCheck::Corrupt);
        let (d3, _) = fixture(|m| m.architecture = "aarch64".into());
        assert_eq!(check_dir(&d3).unwrap_err(), ManifestCheck::ArchitectureUnsupported);
        cleanup(d);
        cleanup(d2);
        cleanup(d3);
    }

    #[test]
    fn manifest_path_traversal_is_rejected() {
        let (d, _) = fixture(|m| m.model.file_relative_path = "../outside.bin".into());
        assert_eq!(check_dir(&d).unwrap_err(), ManifestCheck::Corrupt);
        cleanup(d);
    }

    #[test]
    fn missing_runtime_dll_is_runtime_missing_not_available() {
        let (d, _) = fixture(|_| {});
        std::fs::remove_file(d.join("bin/vcruntime140.dll")).unwrap();
        assert_eq!(check_dir(&d).unwrap_err(), ManifestCheck::RuntimeMissing);
        cleanup(d);
    }

    #[test]
    fn tampered_runtime_dll_is_corrupt() {
        let (d, _) = fixture(|_| {});
        std::fs::write(d.join("bin/vcruntime140.dll"), b"XX").unwrap();
        assert_eq!(check_dir(&d).unwrap_err(), ManifestCheck::Corrupt);
        cleanup(d);
    }

    #[test]
    fn a_manifest_without_runtime_files_or_with_unsafe_runtime_paths_is_rejected() {
        let (d, _) = fixture(|m| m.runtime_files.clear());
        assert_eq!(check_dir(&d).unwrap_err(), ManifestCheck::Corrupt);
        cleanup(d);
        for bad in ["../vcruntime140.dll", "bin/sub/x.dll", "models/x.dll", "bin/x.exe"] {
            let (d, _) = fixture(|m| m.runtime_files[0].path = bad.into());
            assert_eq!(check_dir(&d).unwrap_err(), ManifestCheck::Corrupt, "{bad}");
            cleanup(d);
        }
    }

    #[test]
    fn manifest_must_hash_its_own_executable() {
        let (d, _) = fixture(|m| m.bin_files.clear());
        assert_eq!(check_dir(&d).unwrap_err(), ManifestCheck::Corrupt);
        cleanup(d);
    }

    #[test]
    fn language_support_comes_from_the_manifest_only() {
        let (d, m) = fixture(|m| m.model.languages = vec!["tr".into()]);
        assert!(m.supports_language("tr"));
        assert!(!m.supports_language("auto"));
        assert!(!m.supports_language("en"));
        cleanup(d);
    }

    #[test]
    fn bom_prefixed_manifest_loads() {
        let (d, _) = fixture(|_| {});
        let mut b = vec![0xEF, 0xBB, 0xBF];
        b.extend(std::fs::read(d.join("manifest.json")).unwrap());
        std::fs::write(d.join("manifest.json"), b).unwrap();
        assert!(check_dir(&d).is_ok());
        cleanup(d);
    }
}
