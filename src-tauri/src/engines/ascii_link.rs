//! ASCII-only names for child processes.
//!
//! Root cause this exists for (measured): `whisper-cli.exe` receives an ANSI-narrowed
//! argv from the CRT and opens its model with a UTF-8-only loader, so **any** non-ASCII
//! character in a path it is handed - including Turkish `ç ş ğ ı ö ü İ` on a Turkish
//! Windows - makes it fail (Cyrillic/Arabic/CJK are additionally mangled into `?`).
//! The launcher is not at fault (`CreateProcessW`; `ffmpeg`/`ffprobe` open the very same
//! paths fine) and neither are the file APIs (relative names in a non-ASCII cwd work).
//!
//! So children are never given a non-ASCII path. They run with `cwd = <UUID job dir>` and
//! receive only short generated relative names:
//!   * the user's source file  -> `input.<ext>` (hard link, else a cancel-aware copy)
//!   * the model file          -> `m/<file>` through a directory junction to the engine's
//!     `models/` directory (no admin needed, no 190-570 MB copy), else a hard link, else a copy.
//! The user's original path/filename is display metadata only.

use super::speech_error::{SpeechError, SpeechErrorCode};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Name of the model junction inside a job directory.
pub const MODEL_LINK_DIR: &str = "m";
const COPY_CHUNK: usize = 4 * 1024 * 1024;

fn io_err(code: SpeechErrorCode, e: impl ToString) -> SpeechError {
    SpeechError::new(code).with_detail(e.to_string())
}

/// Copies `src` to `dst` in chunks, checking `cancel` between chunks and removing the
/// partial file if cancelled or failed.
pub fn copy_cancellable(src: &Path, dst: &Path, cancel: &AtomicBool) -> Result<(), SpeechError> {
    let r = (|| -> Result<(), SpeechError> {
        let mut input = std::fs::File::open(src)
            .map_err(|e| io_err(SpeechErrorCode::SpeechInputNotFound, e))?;
        let mut output = std::fs::File::create(dst).map_err(|e| io_err(SpeechErrorCode::SpeechPreprocessFailed, e))?;
        let mut buf = vec![0u8; COPY_CHUNK];
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(SpeechError::new(SpeechErrorCode::SpeechCancelled));
            }
            let n = input.read(&mut buf).map_err(|e| io_err(SpeechErrorCode::SpeechPreprocessFailed, e))?;
            if n == 0 {
                return Ok(());
            }
            output.write_all(&buf[..n]).map_err(|e| {
                // typically "disk full"
                SpeechError::with_message(
                    SpeechErrorCode::SpeechResourceLimit,
                    "Diskte yeterli boş alan yok. Ses dosyası geçici klasöre kopyalanamadı.",
                )
                .with_detail(e.to_string())
            })?;
        }
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    r
}

/// Makes `src` available inside `job_dir` under the generated name `dst_name`:
/// hard link when possible (instant, no extra disk), otherwise a cancel-aware copy.
pub fn stage_file(src: &Path, job_dir: &Path, dst_name: &str, cancel: &AtomicBool) -> Result<(), SpeechError> {
    debug_assert!(dst_name.is_ascii() && !dst_name.contains(['/', '\\']));
    let dst = job_dir.join(dst_name);
    if std::fs::hard_link(src, &dst).is_ok() {
        return Ok(());
    }
    copy_cancellable(src, &dst, cancel)
}

/// Returns the argument to hand a child so it can open `model`, relative to `job_dir`
/// (its cwd) unless `model` is already pure ASCII.
pub fn expose_model(model: &Path, job_dir: &Path, cancel: &AtomicBool) -> Result<String, SpeechError> {
    let full = model.to_string_lossy();
    if full.is_ascii() {
        return Ok(full.to_string());
    }
    let file = model.file_name().and_then(|n| n.to_str()).filter(|n| n.is_ascii()).ok_or_else(|| {
        SpeechError::new(SpeechErrorCode::SpeechEngineInvalid).with_detail("model file name is not ASCII")
    })?;
    if let Some(parent) = model.parent() {
        let link = job_dir.join(MODEL_LINK_DIR);
        if create_dir_junction(&link, parent).is_ok() && link.join(file).is_file() {
            return Ok(format!("{MODEL_LINK_DIR}/{file}"));
        }
        let _ = std::fs::remove_dir(&link); // failed/partial link: remove just the link
    }
    stage_file(model, job_dir, file, cancel)?;
    Ok(file.to_string())
}

/// Detaches any model junction from `job_dir` WITHOUT touching its target. Must run before
/// a job directory is removed: only the reparse point itself may ever be deleted.
pub fn detach_links(job_dir: &Path) {
    let link = job_dir.join(MODEL_LINK_DIR);
    if std::fs::symlink_metadata(&link).is_ok() {
        // `remove_dir` on a junction removes the junction only; on a real (non-empty)
        // directory it fails harmlessly - and we never fall back to remove_dir_all here.
        let _ = std::fs::remove_dir(&link);
    }
}

#[cfg(windows)]
pub fn create_dir_junction(link: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    extern "system" {
        fn DeviceIoControl(
            h: *mut core::ffi::c_void,
            code: u32,
            inbuf: *const core::ffi::c_void,
            insz: u32,
            outbuf: *mut core::ffi::c_void,
            outsz: u32,
            returned: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
    }
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let target = std::fs::canonicalize(target)?;
    let t = target.to_string_lossy().to_string();
    // canonicalize gives `\\?\C:\dir`; a mount point wants the NT form `\??\C:\dir`.
    let nt = if let Some(rest) = t.strip_prefix(r"\\?\") {
        if rest.starts_with("UNC\\") {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "network targets are not linked"));
        }
        format!(r"\??\{rest}")
    } else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "unexpected target form"));
    };
    let sub: Vec<u16> = std::ffi::OsStr::new(&nt).encode_wide().collect();
    let sub_bytes = (sub.len() * 2) as u16;

    std::fs::create_dir(link)?;
    let result = (|| {
        let dir = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags_compat(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(link)?;
        // REPARSE_DATA_BUFFER (mount point): 8-byte header + 8 bytes of offsets/lengths + PathBuffer
        let path_len = sub_bytes as usize + 2 /*NUL*/ + 2 /*empty print name NUL*/;
        let data_len = 8 + path_len;
        let mut buf: Vec<u8> = Vec::with_capacity(8 + data_len);
        buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buf.extend_from_slice(&(data_len as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&0u16.to_le_bytes()); // substitute name offset
        buf.extend_from_slice(&sub_bytes.to_le_bytes()); // substitute name length
        buf.extend_from_slice(&(sub_bytes + 2).to_le_bytes()); // print name offset
        buf.extend_from_slice(&0u16.to_le_bytes()); // print name length
        for u in &sub {
            buf.extend_from_slice(&u.to_le_bytes());
        }
        buf.extend_from_slice(&[0, 0, 0, 0]);
        let mut returned = 0u32;
        // SAFETY: `dir` is a valid open handle for the lifetime of the call; `buf` is a fully
        // initialised REPARSE_DATA_BUFFER of the stated length.
        let ok = unsafe {
            DeviceIoControl(
                dir.as_raw_handle() as *mut _,
                FSCTL_SET_REPARSE_POINT,
                buf.as_ptr() as *const _,
                buf.len() as u32,
                core::ptr::null_mut(),
                0,
                &mut returned,
                core::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir(link);
    }
    result
}

#[cfg(windows)]
trait CustomFlags {
    fn custom_flags_compat(&mut self, flags: u32) -> &mut Self;
}
#[cfg(windows)]
impl CustomFlags for std::fs::OpenOptions {
    fn custom_flags_compat(&mut self, flags: u32) -> &mut Self {
        use std::os::windows::fs::OpenOptionsExt;
        self.custom_flags(flags)
    }
}

#[cfg(not(windows))]
pub fn create_dir_junction(_link: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "junctions are Windows-only"))
}

#[allow(dead_code)]
pub fn is_ascii_path(p: &Path) -> bool {
    p.to_string_lossy().is_ascii()
}

#[allow(dead_code)]
pub fn unicode_test_dir_names() -> Vec<&'static str> {
    vec![
        "Çalışmalar_Öğretmen_ğışİ", // Turkish
        "Müller_Ærø_ñ",             // Western European
        "Жук_Иван",                 // Cyrillic
        "مجلد_عربي",                // Arabic
        "日本語_中文",              // CJK
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("meb_ascii_link_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    const NO_CANCEL: AtomicBool = AtomicBool::new(false);

    #[test]
    fn ascii_model_paths_are_passed_through_unchanged() {
        let d = tmp();
        let m = d.join("ggml-x.bin");
        std::fs::write(&m, b"m").unwrap();
        let arg = expose_model(&m, &d, &NO_CANCEL).unwrap();
        assert_eq!(arg, m.to_string_lossy());
        let _ = std::fs::remove_dir_all(d);
    }

    #[cfg(windows)]
    #[test]
    fn non_ascii_model_dirs_of_every_script_get_an_ascii_relative_argument() {
        for name in unicode_test_dir_names() {
            let d = tmp();
            let engine_models = d.join(name).join("models");
            std::fs::create_dir_all(&engine_models).unwrap();
            let model = engine_models.join("ggml-x.bin");
            std::fs::write(&model, b"model-bytes").unwrap();
            let job = d.join("job");
            std::fs::create_dir_all(&job).unwrap();

            let arg = expose_model(&model, &job, &NO_CANCEL).unwrap();
            assert!(arg.is_ascii(), "{name}: argument must be ASCII, got {arg}");
            assert!(!Path::new(&arg).is_absolute(), "{name}: must be relative to the job dir");
            assert_eq!(std::fs::read(job.join(&arg)).unwrap(), b"model-bytes", "{name}");
            assert!(job.join(MODEL_LINK_DIR).exists(), "{name}: the junction path should have been used");

            // Cleanup must remove ONLY the link and never touch the engine's files.
            detach_links(&job);
            assert!(!job.join(MODEL_LINK_DIR).exists());
            assert_eq!(std::fs::read(&model).unwrap(), b"model-bytes", "{name}: model destroyed by cleanup!");
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[cfg(windows)]
    #[test]
    fn removing_a_job_dir_that_still_holds_the_junction_never_deletes_the_target() {
        let d = tmp();
        let models = d.join("Öğretmen_models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("ggml-x.bin"), b"precious").unwrap();
        let job = d.join("job");
        std::fs::create_dir_all(&job).unwrap();
        expose_model(&models.join("ggml-x.bin"), &job, &NO_CANCEL).unwrap();
        // the crash-recovery / stale-sweep path: plain remove_dir_all on the job dir
        std::fs::remove_dir_all(&job).unwrap();
        assert_eq!(std::fs::read(models.join("ggml-x.bin")).unwrap(), b"precious");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn stage_file_hardlinks_or_copies_and_the_original_is_untouched() {
        let d = tmp();
        let src = d.join("ders kaydı_ığş.mp3");
        std::fs::write(&src, b"audio").unwrap();
        let job = d.join("job");
        std::fs::create_dir_all(&job).unwrap();
        stage_file(&src, &job, "input.mp3", &NO_CANCEL).unwrap();
        assert_eq!(std::fs::read(job.join("input.mp3")).unwrap(), b"audio");
        std::fs::remove_dir_all(&job).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), b"audio", "removing the job must not affect the user's file");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_cancelled_copy_stops_and_leaves_no_partial_file() {
        let d = tmp();
        let src = d.join("big.bin");
        std::fs::write(&src, vec![1u8; 9 * 1024 * 1024]).unwrap();
        let dst = d.join("out.bin");
        let cancelled = AtomicBool::new(true);
        let e = copy_cancellable(&src, &dst, &cancelled).unwrap_err();
        assert_eq!(e.code, SpeechErrorCode::SpeechCancelled);
        assert!(!dst.exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn copy_fallback_produces_an_identical_file() {
        let d = tmp();
        let src = d.join("a.bin");
        let data: Vec<u8> = (0..10_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let dst = d.join("b.bin");
        copy_cancellable(&src, &dst, &NO_CANCEL).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), data);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn model_falls_back_to_link_or_copy_when_a_junction_cannot_be_created() {
        let d = tmp();
        let models = d.join("Öğretmen").join("models");
        std::fs::create_dir_all(&models).unwrap();
        let model = models.join("ggml-x.bin");
        std::fs::write(&model, b"model-bytes").unwrap();
        let job = d.join("job");
        std::fs::create_dir_all(&job).unwrap();
        // a FILE squatting on the junction name makes create_dir fail -> must fall back
        std::fs::write(job.join(MODEL_LINK_DIR), b"in the way").unwrap();
        let arg = expose_model(&model, &job, &NO_CANCEL).unwrap();
        assert_eq!(arg, "ggml-x.bin");
        assert_eq!(std::fs::read(job.join(&arg)).unwrap(), b"model-bytes");
        let _ = std::fs::remove_dir_all(d);
    }
}
