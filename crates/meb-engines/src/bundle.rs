//! Integrity verification for files inside a bundled engine directory.
//!
//! An engine "being present" is not enough to trust it (see
//! `office_manifest`): every executable, DLL and model listed in a
//! manifest is re-hashed against the SHA-256 the manifest carries. A hash
//! of a 190 MB model takes a noticeable fraction of a second, so results
//! are cached in-process keyed by (path, size, mtime) - a file that is
//! swapped or edited on disk changes its size/mtime and is re-hashed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// One file a manifest vouches for. `name`/`relative_path` is relative to
/// the engine directory and is validated by `safe_join`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ManifestFile {
    #[serde(alias = "name")]
    pub path: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// A listed file does not exist.
    Missing,
    /// Present but hash/size differs, or path is unsafe.
    Corrupt,
}

/// Joins `rel` onto `root`, refusing absolute paths, drive prefixes and any
/// `..` component - a manifest must never be able to point outside its own
/// engine directory.
pub fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute() || rel.is_empty() {
        return None;
    }
    for c in p.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            _ => return None,
        }
    }
    Some(root.join(p))
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

type Stamp = (u64, Option<SystemTime>);

lazy_static::lazy_static! {
    /// path -> (stamp, sha256 hex) of the last successful hash.
    static ref HASH_CACHE: Mutex<HashMap<PathBuf, (Stamp, String)>> = Mutex::new(HashMap::new());
}

/// SHA-256 of `path`, reusing the cached digest while size+mtime are unchanged.
pub fn sha256_file_cached(path: &Path) -> std::io::Result<String> {
    let meta = std::fs::metadata(path)?;
    let stamp: Stamp = (meta.len(), meta.modified().ok());
    if let Ok(cache) = HASH_CACHE.lock() {
        if let Some((s, h)) = cache.get(path) {
            if *s == stamp {
                return Ok(h.clone());
            }
        }
    }
    let digest = sha256_file(path)?;
    if let Ok(mut cache) = HASH_CACHE.lock() {
        cache.insert(path.to_path_buf(), (stamp, digest.clone()));
    }
    Ok(digest)
}

/// Verifies every listed file under `root`. Fails closed on the first
/// problem: missing -> `Missing`; anything else wrong -> `Corrupt`.
pub fn verify_files(root: &Path, files: &[ManifestFile]) -> Result<(), VerifyError> {
    for f in files {
        let Some(full) = safe_join(root, &f.path) else {
            return Err(VerifyError::Corrupt);
        };
        if !full.starts_with(root) {
            return Err(VerifyError::Corrupt);
        }
        let meta = match std::fs::metadata(&full) {
            Ok(m) if m.is_file() => m,
            Ok(_) => return Err(VerifyError::Corrupt),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(VerifyError::Missing),
            Err(_) => return Err(VerifyError::Corrupt),
        };
        if f.size != 0 && meta.len() != f.size {
            return Err(VerifyError::Corrupt);
        }
        match sha256_file_cached(&full) {
            Ok(h) if h.eq_ignore_ascii_case(&f.sha256) => {}
            _ => return Err(VerifyError::Corrupt),
        }
    }
    Ok(())
}

pub fn strip_bom(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes.drain(0..3);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("meb_bundle_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn sha256_of_known_input() {
        let d = tmp();
        std::fs::write(d.join("a"), b"abc").unwrap();
        assert_eq!(
            sha256_file(&d.join("a")).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn verify_detects_missing_corrupt_and_ok() {
        let d = tmp();
        std::fs::write(d.join("a"), b"abc").unwrap();
        let good = ManifestFile {
            path: "a".into(),
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            size: 3,
        };
        assert_eq!(verify_files(&d, &[good.clone()]), Ok(()));
        let mut bad = good.clone();
        bad.sha256 = "0".repeat(64);
        assert_eq!(verify_files(&d, &[bad]), Err(VerifyError::Corrupt));
        let mut missing = good.clone();
        missing.path = "nope".into();
        assert_eq!(verify_files(&d, &[missing]), Err(VerifyError::Missing));
        // modified after a successful verification -> cache must not hide it
        std::fs::write(d.join("a"), b"abcd").unwrap();
        assert_eq!(verify_files(&d, &[good]), Err(VerifyError::Corrupt));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn manifest_paths_cannot_escape_the_engine_dir() {
        let root = Path::new("C:/engines/speech");
        for evil in ["../x", "..\\x", "a/../../x", "/etc/passwd", "C:\\Windows\\x", ""] {
            assert!(safe_join(root, evil).is_none(), "{evil}");
        }
        assert!(safe_join(root, "bin/whisper-cli.exe").is_some());
    }
}
