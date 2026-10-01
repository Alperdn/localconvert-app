//! Job-scoped temp directories and the single-slot job registry.
//!
//! Layout (ASCII only on purpose - some engines open paths via the ANSI code
//! page): `<os temp>/MEB-Donusturucu/temp/speech/<uuid>/`. The UUID is fresh
//! per job and never derived from user input, so simultaneous jobs cannot
//! collide and nothing external can influence where files are created.

use crate::engines::speech_error::{SpeechError, SpeechErrorCode};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

pub fn speech_temp_root() -> PathBuf {
    std::env::temp_dir().join("MEB-Donusturucu").join("temp").join("speech")
}

/// Owns one job directory; removes it on drop (success, error, cancel, panic-unwind).
pub struct SpeechJobDir {
    path: PathBuf,
}

impl SpeechJobDir {
    pub fn create() -> std::io::Result<Self> {
        Self::create_in(&speech_temp_root())
    }

    pub fn create_in(root: &Path) -> std::io::Result<Self> {
        let path = root.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the directory, retrying briefly: on Windows a just-killed
    /// child's handles, or an antivirus scan, can hold a file for a moment.
    pub fn cleanup(&self) {
        // Detach the model junction first so only the link - never the engine's files - can go.
        crate::engines::ascii_link::detach_links(&self.path);
        for _ in 0..5 {
            if !self.path.exists() || std::fs::remove_dir_all(&self.path).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

impl Drop for SpeechJobDir {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Startup sweep: removes job directories left by a crash / force-kill.
/// Called once from `lib.rs` setup, before any job can exist.
pub fn cleanup_stale_speech_dirs() {
    cleanup_stale_in(&speech_temp_root());
}

pub(crate) fn cleanup_stale_in(root: &Path) {
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            crate::engines::ascii_link::detach_links(&e.path());
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

struct ActiveJob {
    id: String,
    cancel: Arc<AtomicBool>,
}

/// Enforces `max_concurrent_jobs` (1) and routes cancel requests to the one
/// running job. Cancelling only ever flips that job's flag; the process
/// runner then kills exactly the child it spawned for that job.
#[derive(Default)]
pub struct JobRegistry {
    active: Mutex<Option<ActiveJob>>,
}

pub struct JobSlot {
    registry: Arc<JobRegistry>,
    pub id: String,
    pub cancel: Arc<AtomicBool>,
}

impl JobRegistry {
    pub fn try_begin(self: &Arc<Self>) -> Result<JobSlot, SpeechError> {
        let mut g = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_some() {
            return Err(SpeechError::new(SpeechErrorCode::SpeechBusy));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        *g = Some(ActiveJob { id: id.clone(), cancel: cancel.clone() });
        Ok(JobSlot { registry: self.clone(), id, cancel })
    }

    /// Returns true if `job_id` was the running job.
    pub fn cancel(&self, job_id: &str) -> bool {
        let g = self.active.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            Some(j) if j.id == job_id => {
                j.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }

    #[cfg(test)]
    pub fn is_busy(&self) -> bool {
        self.active.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
}

impl Drop for JobSlot {
    fn drop(&mut self) {
        let mut g = self.registry.active.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().map(|j| j.id == self.id).unwrap_or(false) {
            *g = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn test_root() -> PathBuf {
        std::env::temp_dir().join(format!("meb_speech_jobtest_{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn job_dirs_are_unique_ascii_uuid_children_of_the_root() {
        let root = test_root();
        let a = SpeechJobDir::create_in(&root).unwrap();
        let b = SpeechJobDir::create_in(&root).unwrap();
        assert_ne!(a.path(), b.path());
        assert_eq!(a.path().parent().unwrap(), root);
        let name = a.path().file_name().unwrap().to_string_lossy().to_string();
        assert!(uuid::Uuid::parse_str(&name).is_ok());
        assert!(speech_temp_root().to_string_lossy().contains("MEB-Donusturucu"));
        drop(a);
        drop(b);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn job_dir_is_removed_on_drop_including_contents() {
        let root = test_root();
        let p = {
            let d = SpeechJobDir::create_in(&root).unwrap();
            std::fs::write(d.path().join("norm.wav"), vec![0u8; 1024]).unwrap();
            std::fs::write(d.path().join("rec.pcm"), b"x").unwrap();
            d.path().to_path_buf()
        };
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stale_sweep_removes_leftovers_from_a_crashed_run() {
        let root = test_root();
        for _ in 0..3 {
            let d = root.join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("norm.wav"), b"leftover").unwrap();
        }
        cleanup_stale_in(&root);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn only_one_job_at_a_time_and_slot_frees_on_drop() {
        let reg = Arc::new(JobRegistry::default());
        let slot = reg.try_begin().unwrap();
        assert!(reg.is_busy());
        assert_eq!(reg.try_begin().err().unwrap().code, SpeechErrorCode::SpeechBusy);
        drop(slot);
        assert!(!reg.is_busy());
        assert!(reg.try_begin().is_ok());
    }

    #[test]
    fn cancel_targets_only_the_running_job() {
        let reg = Arc::new(JobRegistry::default());
        assert!(!reg.cancel("nope"));
        let slot = reg.try_begin().unwrap();
        assert!(!reg.cancel("someone-elses-id"));
        assert!(!slot.cancel.load(Ordering::SeqCst));
        assert!(reg.cancel(&slot.id));
        assert!(slot.cancel.load(Ordering::SeqCst));
        drop(slot);
        // a new job after a cancelled one starts with a clean flag (no poisoning)
        let next = reg.try_begin().unwrap();
        assert!(!next.cancel.load(Ordering::SeqCst));
    }
}
