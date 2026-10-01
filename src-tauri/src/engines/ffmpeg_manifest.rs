//! Manifest for the bundled minimal audio FFmpeg (`EngineId::AudioFfmpeg` /
//! `AudioFfprobe`), produced by `scripts/prepare-ffmpeg-engine.ps1` from the
//! build record of `scripts/build-ffmpeg-engine.ps1`.
//!
//! Same trust model as `speech_manifest`: the files exist AND hash to what the
//! manifest says AND the manifest carries the pinned identity. License mode is
//! part of the identity - a manifest claiming anything other than an
//! LGPL-only build is refused, so a GPL/nonfree build can never be picked up
//! by accident.

use super::bundle::{self, ManifestFile, VerifyError};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;
pub const PINNED_FFMPEG_VERSION: &str = "9.0.2";
pub const PINNED_ARCHITECTURE: &str = "x86_64";
pub const REQUIRED_LICENSE_MODE: &str = "LGPL-2.1-or-later";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FfmpegManifest {
    pub schema_version: u32,
    pub engine_id: String,
    pub ffmpeg_version: String,
    pub architecture: String,
    pub source_url: String,
    pub source_archive_sha256: String,
    pub configure_flags: String,
    pub ffmpeg_sha256: String,
    pub ffprobe_sha256: String,
    pub license_mode: String,
    pub required_files: Vec<ManifestFile>,
    #[serde(default)]
    pub build_date: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegCheck {
    Missing,
    Corrupt,
    VersionMismatch,
    ArchitectureUnsupported,
}

pub fn check_dir(dir: &Path) -> Result<FfmpegManifest, FfmpegCheck> {
    if !dir.is_dir() {
        return Err(FfmpegCheck::Missing);
    }
    let bytes = std::fs::read(dir.join("manifest.json")).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FfmpegCheck::Missing
        } else {
            FfmpegCheck::Corrupt
        }
    })?;
    let m: FfmpegManifest =
        serde_json::from_slice(&bundle::strip_bom(bytes)).map_err(|_| FfmpegCheck::Corrupt)?;

    if m.engine_id != "ffmpeg" || m.schema_version != SCHEMA_VERSION {
        return Err(FfmpegCheck::Corrupt);
    }
    if m.license_mode != REQUIRED_LICENSE_MODE {
        return Err(FfmpegCheck::Corrupt);
    }
    // Defense in depth: even if a manifest claims LGPL, refuse one whose
    // recorded configure line enables GPL/nonfree.
    for bad in ["--enable-gpl", "--enable-nonfree", "--enable-version3"] {
        if m.configure_flags.contains(bad) {
            return Err(FfmpegCheck::Corrupt);
        }
    }
    if m.architecture != PINNED_ARCHITECTURE || !cfg!(target_arch = "x86_64") {
        return Err(FfmpegCheck::ArchitectureUnsupported);
    }
    if m.ffmpeg_version != PINNED_FFMPEG_VERSION {
        return Err(FfmpegCheck::VersionMismatch);
    }
    // The two executables must be in required_files AND carry the same hash
    // as the dedicated top-level fields.
    let find = |p: &str| m.required_files.iter().find(|f| f.path == p);
    let (Some(ff), Some(fp)) = (find("bin/ffmpeg.exe"), find("bin/ffprobe.exe")) else {
        return Err(FfmpegCheck::Corrupt);
    };
    if !ff.sha256.eq_ignore_ascii_case(&m.ffmpeg_sha256) || !fp.sha256.eq_ignore_ascii_case(&m.ffprobe_sha256) {
        return Err(FfmpegCheck::Corrupt);
    }
    match bundle::verify_files(dir, &m.required_files) {
        Ok(()) => Ok(m),
        Err(VerifyError::Missing) => Err(FfmpegCheck::Missing),
        Err(VerifyError::Corrupt) => Err(FfmpegCheck::Corrupt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sha(b: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(b).iter().map(|x| format!("{:02x}", x)).collect()
    }

    fn fixture(mutate: impl FnOnce(&mut FfmpegManifest)) -> PathBuf {
        let d = std::env::temp_dir().join(format!("meb_ffmpeg_mf_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::write(d.join("bin/ffmpeg.exe"), b"ff").unwrap();
        std::fs::write(d.join("bin/ffprobe.exe"), b"fp").unwrap();
        let mut m = FfmpegManifest {
            schema_version: SCHEMA_VERSION,
            engine_id: "ffmpeg".into(),
            ffmpeg_version: PINNED_FFMPEG_VERSION.into(),
            architecture: PINNED_ARCHITECTURE.into(),
            source_url: "https://ffmpeg.org/releases/x".into(),
            source_archive_sha256: "0".repeat(64),
            configure_flags: "--disable-everything".into(),
            ffmpeg_sha256: sha(b"ff"),
            ffprobe_sha256: sha(b"fp"),
            license_mode: REQUIRED_LICENSE_MODE.into(),
            required_files: vec![
                ManifestFile { path: "bin/ffmpeg.exe".into(), sha256: sha(b"ff"), size: 2 },
                ManifestFile { path: "bin/ffprobe.exe".into(), sha256: sha(b"fp"), size: 2 },
            ],
            build_date: String::new(),
        };
        mutate(&mut m);
        std::fs::write(d.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        d
    }

    #[test]
    fn valid_passes_and_missing_is_missing() {
        let d = fixture(|_| {});
        assert!(check_dir(&d).is_ok());
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(check_dir(Path::new("Z:/nope")).unwrap_err(), FfmpegCheck::Missing);
    }

    #[test]
    fn tampered_binary_is_corrupt() {
        let d = fixture(|_| {});
        std::fs::write(d.join("bin/ffprobe.exe"), b"xx").unwrap();
        assert_eq!(check_dir(&d).unwrap_err(), FfmpegCheck::Corrupt);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn gpl_or_nonfree_builds_are_refused() {
        for flags in ["--enable-gpl", "--disable-x --enable-nonfree", "--enable-version3"] {
            let d = fixture(|m| m.configure_flags = flags.into());
            assert_eq!(check_dir(&d).unwrap_err(), FfmpegCheck::Corrupt, "{flags}");
            let _ = std::fs::remove_dir_all(&d);
        }
        let d = fixture(|m| m.license_mode = "GPL-2.0-or-later".into());
        assert_eq!(check_dir(&d).unwrap_err(), FfmpegCheck::Corrupt);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn wrong_version_and_arch_are_reported() {
        let d = fixture(|m| m.ffmpeg_version = "1.0".into());
        assert_eq!(check_dir(&d).unwrap_err(), FfmpegCheck::VersionMismatch);
        let _ = std::fs::remove_dir_all(&d);
        let d = fixture(|m| m.architecture = "aarch64".into());
        assert_eq!(check_dir(&d).unwrap_err(), FfmpegCheck::ArchitectureUnsupported);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn top_level_hashes_must_agree_with_required_files() {
        let d = fixture(|m| m.ffmpeg_sha256 = "1".repeat(64));
        assert_eq!(check_dir(&d).unwrap_err(), FfmpegCheck::Corrupt);
        let _ = std::fs::remove_dir_all(&d);
    }
}
