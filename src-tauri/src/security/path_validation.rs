//! Centralized path validation.
//!
//! Every path that arrives from the frontend (input files, output
//! directories, save targets) is untrusted input and must be validated
//! here before it is used by a command or handed to an external tool.
//!
//! Rules enforced:
//! - Input paths must exist and be a regular file (no directories, and
//!   on Unix, symlinks are resolved via canonicalize() so we validate
//!   the real target, not the link).
//! - Output directories must exist and be a directory (we do not
//!   silently create arbitrary directories at attacker-influenced
//!   paths; the directory always comes from an OS-native picker or a
//!   known default in this app).
//! - Neither may resolve inside the application's own install/resource
//!   directory, so a bug or malicious input can't cause the app to
//!   read or overwrite its own binaries/resources.

use std::path::{Path, PathBuf};

/// `Path::canonicalize()` on Windows returns an extended-length path with a
/// `\\?\` prefix (e.g. `\\?\C:\Users\...`). Several of the external tools
/// this app shells out to - LibreOffice in particular, per the existing
/// `converter::get_absolute_path` comment this mirrors - don't handle that
/// prefix well. Every path validated here is normalized the same way, once,
/// at the source, rather than leaving each call site downstream to
/// remember to strip it (which is what the codebase did before: only the
/// four LibreOffice-backed converters called a local `get_absolute_path`
/// helper; ffmpeg/ImageMagick/Ghostscript-backed ones did not).
fn normalize_windows_prefix(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        let s = path.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path
}

/// Validates a path that is expected to be an existing, readable file.
/// Returns the canonicalized (and Windows-prefix-normalized) path on
/// success.
pub fn validate_input_file(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("Input path is empty".to_string());
    }

    let path = Path::new(raw);
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Input file does not exist or is not accessible: {}", e))?;
    let canonical = normalize_windows_prefix(canonical);

    let metadata = std::fs::metadata(&canonical)
        .map_err(|e| format!("Failed to read input file metadata: {}", e))?;

    if !metadata.is_file() {
        return Err("Input path is not a regular file".to_string());
    }

    if is_within_app_directory(&canonical) {
        return Err("Refusing to read a file inside the application's own install directory".to_string());
    }

    Ok(canonical)
}

/// Validates a path that is expected to be an existing, writable
/// directory (the final destination for a conversion).
pub fn validate_output_dir(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("Output directory is empty".to_string());
    }

    let path = Path::new(raw);
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Output directory does not exist or is not accessible: {}", e))?;
    let canonical = normalize_windows_prefix(canonical);

    let metadata = std::fs::metadata(&canonical)
        .map_err(|e| format!("Failed to read output directory metadata: {}", e))?;

    if !metadata.is_dir() {
        return Err("Output path is not a directory".to_string());
    }

    if is_within_app_directory(&canonical) {
        return Err("Refusing to write inside the application's own install directory".to_string());
    }

    Ok(canonical)
}

/// Validates a path a user is about to read from or write to via the
/// frontend's scoped fs-plugin calls (used by the PDF editor). Unlike
/// [`validate_input_file`], the target file does not need to exist yet
/// ("Save As" picks a brand new filename) - but its parent directory
/// must exist, and the resolved path still may not land inside the
/// app's own install directory.
pub fn validate_fs_scope_target(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("Path is empty".to_string());
    }

    let path = Path::new(raw);

    // If the file already exists, canonicalize it directly (also
    // resolves symlinks so scope is granted to the real target).
    if path.exists() {
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("Failed to resolve path: {}", e))?;
        let canonical = normalize_windows_prefix(canonical);
        if is_within_app_directory(&canonical) {
            return Err("Refusing to grant access inside the application's own install directory".to_string());
        }
        return Ok(canonical);
    }

    // Otherwise this is a not-yet-created "Save As" target: its parent
    // directory must exist and be a real directory.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "Path has no parent directory".to_string())?;

    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("Target directory does not exist: {}", e))?;
    let canonical_parent = normalize_windows_prefix(canonical_parent);

    if !canonical_parent.is_dir() {
        return Err("Target's parent is not a directory".to_string());
    }

    if is_within_app_directory(&canonical_parent) {
        return Err("Refusing to grant access inside the application's own install directory".to_string());
    }

    let file_name = path
        .file_name()
        .ok_or_else(|| "Path has no file name".to_string())?;

    Ok(canonical_parent.join(file_name))
}

/// True if `candidate` resolves inside the directory that contains the
/// running executable (i.e. the app's own install/resource directory).
/// Defense in depth against a buggy or attacker-influenced path
/// pointing conversions at the app's own binaries.
fn is_within_app_directory(candidate: &Path) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Ok(exe_dir) = exe.canonicalize() else {
        return false;
    };
    let exe_dir = normalize_windows_prefix(exe_dir);
    let Some(exe_dir) = exe_dir.parent() else {
        return false;
    };
    // `candidate` is expected to already be normalized by the caller
    // (validate_input_file / validate_output_dir / validate_fs_scope_target
    // all normalize before calling this). Normalizing here too costs
    // nothing and protects against a future caller that forgets to.
    candidate.starts_with(exe_dir)
}

/// Confirms `candidate` really is located inside `root` once both are
/// canonicalized. Used as a defense-in-depth check after a conversion
/// writes into a job's temp directory, before that file is moved
/// anywhere - guards against a future bug in filename construction
/// accidentally escaping the temp directory.
pub fn confirm_within(root: &Path, candidate: &Path) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Failed to resolve temp directory: {}", e))?;
    let canonical_candidate = candidate
        .canonicalize()
        .map_err(|e| format!("Failed to resolve output file: {}", e))?;

    if !canonical_candidate.starts_with(&canonical_root) {
        return Err("Internal error: produced file escaped its job directory".to_string());
    }

    Ok(canonical_candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc_pv_test_{}_{}", name, uuid_like()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Tiny dependency-free unique suffix so this test file doesn't need the
    // `uuid` crate as a dev-dependency just for itself.
    fn uuid_like() -> u128 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn validate_input_file_accepts_real_file() {
        let dir = temp_sandbox("in_ok");
        let file = dir.join("a.txt");
        fs::write(&file, b"hi").unwrap();

        let result = validate_input_file(file.to_str().unwrap()).unwrap();
        assert!(result.is_file());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_input_file_rejects_missing_path() {
        let err = validate_input_file("/definitely/not/a/real/path/xyz.txt");
        assert!(err.is_err());
    }

    #[test]
    fn validate_input_file_rejects_directory() {
        let dir = temp_sandbox("in_dir");
        let err = validate_input_file(dir.to_str().unwrap());
        assert!(err.is_err(), "a directory must not validate as an input file");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_input_file_rejects_empty_string() {
        assert!(validate_input_file("").is_err());
        assert!(validate_input_file("   ").is_err());
    }

    #[test]
    fn validate_output_dir_accepts_real_directory() {
        let dir = temp_sandbox("out_ok");
        let result = validate_output_dir(dir.to_str().unwrap()).unwrap();
        assert!(result.is_dir());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_output_dir_rejects_a_file() {
        let dir = temp_sandbox("out_file");
        let file = dir.join("not_a_dir.txt");
        fs::write(&file, b"x").unwrap();
        assert!(validate_output_dir(file.to_str().unwrap()).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_output_dir_rejects_missing_path() {
        assert!(validate_output_dir("/definitely/not/a/real/dir/xyz").is_err());
    }

    #[test]
    fn fs_scope_target_accepts_existing_file() {
        let dir = temp_sandbox("scope_existing");
        let file = dir.join("edit_me.pdf");
        fs::write(&file, b"pdf-ish").unwrap();

        let result = validate_fs_scope_target(file.to_str().unwrap()).unwrap();
        // Compare against the same `\\?\`-normalized form the function
        // itself returns - on Windows, `Path::canonicalize()` alone
        // produces a `\\?\`-prefixed extended-length path, which is a
        // different (if equivalent) string from what
        // `validate_fs_scope_target` returns after normalization.
        assert_eq!(result, normalize_windows_prefix(file.canonicalize().unwrap()));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fs_scope_target_accepts_not_yet_created_save_as_target() {
        let dir = temp_sandbox("scope_new");
        let not_yet_created = dir.join("edited_copy.pdf");
        assert!(!not_yet_created.exists());

        let result = validate_fs_scope_target(not_yet_created.to_str().unwrap()).unwrap();
        assert_eq!(result.file_name().unwrap(), "edited_copy.pdf");
        assert_eq!(result.parent().unwrap(), normalize_windows_prefix(dir.canonicalize().unwrap()));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fs_scope_target_rejects_parent_that_does_not_exist() {
        let bogus = "/definitely/not/a/real/dir/xyz/file.pdf";
        assert!(validate_fs_scope_target(bogus).is_err());
    }

    #[test]
    fn fs_scope_target_rejects_empty() {
        assert!(validate_fs_scope_target("").is_err());
    }

    #[test]
    fn confirm_within_accepts_nested_path() {
        let root = temp_sandbox("confirm_root");
        let nested = root.join("sub").join("file.bin");
        fs::create_dir_all(nested.parent().unwrap()).unwrap();
        fs::write(&nested, b"x").unwrap();

        let result = confirm_within(&root, &nested).unwrap();
        assert!(result.starts_with(root.canonicalize().unwrap()));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn confirm_within_rejects_path_outside_root() {
        let root = temp_sandbox("confirm_root_a");
        let outside_dir = temp_sandbox("confirm_root_b");
        let outside_file = outside_dir.join("escaped.bin");
        fs::write(&outside_file, b"x").unwrap();

        let result = confirm_within(&root, &outside_file);
        assert!(result.is_err(), "a path outside root must not be confirmed as contained");

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&outside_dir).ok();
    }
}
