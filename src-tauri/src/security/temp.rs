//! Isolated, per-job temporary working directories.
//!
//! Every conversion job gets its own directory under the OS temp
//! directory:
//!
//!     <temp>/localconvert/jobs/<uuid>/
//!
//! The external tool writes its output *there*, never directly into
//! the user's chosen output folder. Only after the tool reports
//! success is the finished file moved into place. This means:
//!
//! - A crashed or failed conversion never leaves partial/corrupt
//!   files in the user's real output folder - only in the ephemeral
//!   job directory, which gets cleaned up regardless of outcome.
//! - Two jobs converting same-named files can never collide before
//!   the final, deliberate move into the destination folder.
//!
//! Cleanup happens via `Drop`, so it runs whether the job finished,
//! failed, or the function returned early via `?` - "deleted after
//! success or failure whenever possible" without every call site
//! needing its own try/finally-style bookkeeping.

use std::path::{Path, PathBuf};

pub struct JobTempDir {
    path: PathBuf,
}

impl JobTempDir {
    /// Creates a new, uniquely-named job directory. The directory name
    /// is always a fresh UUID - never derived from caller-supplied
    /// text - so nothing external can influence where on disk it is
    /// created.
    pub fn new() -> std::io::Result<Self> {
        let root = std::env::temp_dir().join("localconvert").join("jobs");
        let path = root.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Builds a path for `filename` inside this job's directory. The
    /// filename is sanitized so it cannot escape the directory via a
    /// path separator or `..` component (see `security::file_validation`).
    pub fn file_path(&self, filename: &str) -> PathBuf {
        self.path
            .join(super::file_validation::sanitize_filename_component(filename))
    }

    /// Best-effort cleanup that can be called explicitly once a job's
    /// output has already been moved out, so the directory doesn't
    /// linger for the rest of the process lifetime. Safe to call
    /// before or in addition to the automatic `Drop` cleanup.
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl Drop for JobTempDir {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Moves `from` (inside a job's temp directory) to `to` (the user's
/// real output destination). Tries a plain rename first (instant,
/// atomic); falls back to copy-then-delete for the common case where
/// the temp directory and the destination are on different volumes
/// (e.g. temp on `C:`, output on a network share or a different
/// drive), where `rename` fails on every OS.
pub fn move_into_place(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to prepare output directory: {}", e))?;
    }

    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }

    std::fs::copy(from, to)
        .map_err(|e| format!("Failed to write final output file: {}", e))?;
    // Best-effort: the temp copy is inside the job directory, which
    // gets removed wholesale on cleanup/Drop anyway.
    let _ = std::fs::remove_file(from);
    Ok(())
}

/// Removes any job directories left behind by a previous run that was
/// force-killed or crashed before its `Drop` handler could run. Called
/// once at app startup (see `lib.rs`'s `.setup()`), before any new job
/// directories are created, so there's no risk of removing a directory
/// that's actually in use.
///
/// Best-effort: errors (permissions, a file mid-delete on Windows, etc.)
/// are swallowed rather than failing app startup over leftover temp data.
pub fn cleanup_stale_job_dirs() {
    let root = std::env::temp_dir().join("localconvert").join("jobs");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return; // Doesn't exist yet - nothing to clean up.
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_unique_isolated_dirs() {
        let a = JobTempDir::new().unwrap();
        let b = JobTempDir::new().unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a.path().exists());
        assert!(b.path().exists());
    }

    #[test]
    fn cleans_up_on_drop() {
        let path = {
            let job = JobTempDir::new().unwrap();
            let p = job.path().to_path_buf();
            std::fs::write(job.file_path("x.txt"), b"data").unwrap();
            assert!(p.exists());
            p
        };
        assert!(!path.exists(), "job directory should be removed after Drop");
    }

    #[test]
    fn file_path_rejects_traversal() {
        let job = JobTempDir::new().unwrap();
        let p = job.file_path("../../evil.txt");
        assert!(p.starts_with(job.path()));
    }

    #[test]
    fn move_into_place_works() {
        let job = JobTempDir::new().unwrap();
        let src = job.file_path("out.bin");
        std::fs::write(&src, b"hello").unwrap();

        let dest_dir = JobTempDir::new().unwrap(); // stand-in for an "output dir"
        let dest = dest_dir.path().join("final.bin");

        move_into_place(&src, &dest).unwrap();
        assert!(dest.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
    }
}
