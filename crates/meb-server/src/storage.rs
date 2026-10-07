//! Server-owned filesystem layout (docs/WEB_ARCHITECTURE_PROPOSAL.md §D):
//!
//! ```text
//! <data_root>/
//!   .meb-data-root            marker: this directory belongs to meb-server
//!   .instance.lock            exclusive OS lock held for the server's lifetime
//!   staging/<uuid>.part       in-flight uploads, never under a session dir
//!   sessions/<owner key>/
//!     files/<file_id>/blob    validated upload (fixed name, no extension)
//!     jobs/<job_id>/
//!       in/source.<ext>       hard link/copy of the blob; ext from sniffed type
//!       work/                 engine working area
//!       out/result.<ext>      only validated output is ever moved here
//! ```
//!
//! Every path is built here from the canonical data root, fixed literal
//! segments and server-generated ids (`Owner::key`, `FileId`, `JobId`,
//! `Uuid`). No function accepts a string from a request, so no client value
//! can become a path component.
//!
//! Startup purge is safe because the instance lock guarantees no other live
//! server uses this root, and the job registry is in-memory: anything left
//! on disk belongs to a previous, dead process.

use crate::ids::{FileId, JobId};
use crate::session::Owner;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

const MARKER: &str = ".meb-data-root";
const LOCK: &str = ".instance.lock";
const STAGING: &str = "staging";
const SESSIONS: &str = "sessions";

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    /// The directory has content but no marker - refusing to purge it.
    NotADataRoot(PathBuf),
    /// Another live server instance holds the lock.
    InUse,
    /// The root resolves to a filesystem root.
    Unsafe(PathBuf),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::Io(e) => write!(f, "I/O error: {e}"),
            StorageError::NotADataRoot(p) => write!(
                f,
                "{} is not empty and was not created by meb-server; refusing to use it as the data root",
                p.display()
            ),
            StorageError::InUse => write!(f, "another meb-server instance is using this data root"),
            StorageError::Unsafe(p) => write!(f, "{} is not a safe data root", p.display()),
        }
    }
}

impl From<io::Error> for StorageError {
    fn from(e: io::Error) -> Self {
        StorageError::Io(e)
    }
}

#[derive(Debug, Clone)]
pub struct JobPaths {
    pub root: PathBuf,
    pub input_dir: PathBuf,
    pub work_dir: PathBuf,
    pub out_dir: PathBuf,
}

pub struct Storage {
    root: PathBuf,
    staging: PathBuf,
    sessions: PathBuf,
    _lock: File,
}

impl Storage {
    pub fn open(requested_root: &Path) -> Result<Storage, StorageError> {
        std::fs::create_dir_all(requested_root)?;
        let root = requested_root.canonicalize()?;
        if root.parent().is_none() {
            return Err(StorageError::Unsafe(root));
        }

        let marker = root.join(MARKER);
        if !marker.exists() {
            if std::fs::read_dir(&root)?.next().is_some() {
                return Err(StorageError::NotADataRoot(root));
            }
            std::fs::write(&marker, b"meb-server data root\n")?;
        }

        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(LOCK))?;
        lock.try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => StorageError::InUse,
            std::fs::TryLockError::Error(io) => StorageError::Io(io),
        })?;

        let staging = root.join(STAGING);
        let sessions = root.join(SESSIONS);
        for dir in [&staging, &sessions] {
            if dir.exists() {
                std::fs::remove_dir_all(dir)?;
            }
            std::fs::create_dir_all(dir)?;
        }

        Ok(Storage {
            root,
            staging,
            sessions,
            _lock: lock,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn staging_file(&self) -> PathBuf {
        self.staging
            .join(format!("{}.part", uuid::Uuid::new_v4().simple()))
    }

    pub(crate) fn session_dir(&self, owner: &Owner) -> PathBuf {
        self.sessions.join(owner.key())
    }

    pub(crate) fn file_dir(&self, owner: &Owner, id: FileId) -> PathBuf {
        self.session_dir(owner).join("files").join(id.to_string())
    }

    pub(crate) fn file_blob(&self, owner: &Owner, id: FileId) -> PathBuf {
        self.file_dir(owner, id).join("blob")
    }

    pub(crate) fn job_paths(&self, owner: &Owner, id: JobId) -> JobPaths {
        let root = self.session_dir(owner).join("jobs").join(id.to_string());
        JobPaths {
            input_dir: root.join("in"),
            work_dir: root.join("work"),
            out_dir: root.join("out"),
            root,
        }
    }

    /// Deletes a directory tree that this server created. Refuses anything
    /// that is not strictly inside `staging/` or `sessions/` (containment
    /// check on the lexical path, which only ever comes from the builders
    /// above). `remove_dir_all` does not follow symlinks out of the tree.
    pub(crate) fn remove_tree(&self, path: &Path) -> io::Result<()> {
        let inside = (path.starts_with(&self.sessions) && path != self.sessions)
            || (path.starts_with(&self.staging) && path != self.staging);
        if !inside
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "path outside data root",
            ));
        }
        match std::fs::remove_dir_all(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// True only if `path` is a regular file (not a symlink/junction/dir) whose
/// canonical location is inside `dir`.
pub fn is_regular_file_within(dir: &Path, path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_file() {
        return false;
    }
    match (dir.canonicalize(), path.canonicalize()) {
        (Ok(d), Ok(p)) => p.starts_with(&d) && p != d,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "meb_storage_test_{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn refuses_a_non_empty_directory_it_did_not_create() {
        let root = temp_root();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("someone-elses-file.txt"), b"keep me").unwrap();
        let err = Storage::open(&root).err().unwrap();
        assert!(matches!(err, StorageError::NotADataRoot(_)));
        assert!(
            root.join("someone-elses-file.txt").exists(),
            "must not purge foreign data"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_instance_cannot_open_a_locked_root() {
        let root = temp_root();
        let first = Storage::open(&root).unwrap();
        assert!(matches!(
            Storage::open(&root).err().unwrap(),
            StorageError::InUse
        ));
        drop(first);
        // Lock released with the first instance.
        let again = Storage::open(&root).unwrap();
        drop(again);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn startup_purges_leftovers_of_a_dead_instance_only_inside_its_own_dirs() {
        let root = temp_root();
        {
            let s = Storage::open(&root).unwrap();
            std::fs::create_dir_all(s.sessions.join("deadbeef/jobs/x/work")).unwrap();
            std::fs::write(s.staging.join("half.part"), b"partial").unwrap();
        }
        std::fs::write(root.join("operator-notes.txt"), b"not ours to delete").unwrap();
        let s = Storage::open(&root).unwrap();
        assert_eq!(std::fs::read_dir(&s.sessions).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(&s.staging).unwrap().count(), 0);
        assert!(root.join("operator-notes.txt").exists());
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_tree_refuses_paths_outside_its_areas() {
        let root = temp_root();
        let s = Storage::open(&root).unwrap();
        assert!(s.remove_tree(s.root()).is_err());
        assert!(s.remove_tree(&s.sessions).is_err());
        assert!(s.remove_tree(&s.sessions.join("..").join("x")).is_err());
        assert!(s.remove_tree(&std::env::temp_dir()).is_err());
        assert!(s.remove_tree(&s.sessions.join("nonexistent")).is_ok());
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
    }
}
