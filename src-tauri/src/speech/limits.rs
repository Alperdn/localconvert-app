//! Resource-limit policy for Ses Dikte.
//!
//! These are INITIAL DEVELOPMENT DEFAULTS (approved as such), held in one
//! struct so they are a single internal-policy edit - not scattered magic
//! numbers and not user- or frontend-configurable. They are NOT claimed to be
//! final production limits; reassess after real-hardware performance testing.
//!
//! Rationale (see also the Phase A report):
//! - 500 MB input: bounds read/decode cost; ~1 h of typical MP3/M4A is 30-120 MB
//!   and lossless FLAC/WAV of a long meeting fits comfortably.
//! - 3 h audio: at the measured speed class of the small model, longer jobs
//!   stop being practical on school-class CPUs.
//! - 2 h microphone recording: 16 kHz mono PCM16 is ~1.9 MB/min (~230 MB).
//! - 1 concurrent job: recognition is CPU-bound; a second job just halves the
//!   speed of both and doubles RAM.
//! - Disk: source + normalized WAV + working headroom (see `required_disk_bytes`).

use crate::engines::speech_error::{SpeechError, SpeechErrorCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechLimits {
    pub max_input_bytes: u64,
    pub max_audio_secs: u64,
    pub max_recording_secs: u64,
    pub max_concurrent_jobs: usize,
    /// Working headroom for transcript/JSON/temp beyond the WAV itself.
    pub workspace_bytes: u64,
}

pub const DEFAULT_LIMITS: SpeechLimits = SpeechLimits {
    max_input_bytes: 500 * 1024 * 1024,
    max_audio_secs: 3 * 3600,
    max_recording_secs: 2 * 3600,
    max_concurrent_jobs: 1,
    workspace_bytes: 256 * 1024 * 1024,
};

/// 16 kHz * 2 bytes * 1 channel.
pub const NORMALIZED_BYTES_PER_SEC: u64 = 32_000;

fn mb(b: u64) -> u64 {
    b / (1024 * 1024)
}

impl SpeechLimits {
    pub fn check_input_size(&self, bytes: u64) -> Result<(), SpeechError> {
        if bytes == 0 {
            return Err(SpeechError::with_message(SpeechErrorCode::SpeechInputUnsupported, "Ses dosyası boş."));
        }
        if bytes > self.max_input_bytes {
            return Err(SpeechError::with_message(
                SpeechErrorCode::SpeechResourceLimit,
                format!(
                    "Ses dosyası çok büyük. En fazla {} MB boyutunda dosyalar işlenebilir.",
                    mb(self.max_input_bytes)
                ),
            ));
        }
        Ok(())
    }

    pub fn check_duration(&self, secs: f64) -> Result<(), SpeechError> {
        if secs > self.max_audio_secs as f64 {
            return Err(SpeechError::with_message(
                SpeechErrorCode::SpeechResourceLimit,
                format!(
                    "Ses kaydı çok uzun. En fazla {} saat uzunluğundaki kayıtlar işlenebilir.",
                    self.max_audio_secs / 3600
                ),
            ));
        }
        Ok(())
    }

    #[allow(dead_code)] // enforced by the microphone flow (Phase D)
    pub fn check_recording_duration(&self, secs: f64) -> Result<(), SpeechError> {
        if secs > self.max_recording_secs as f64 {
            return Err(SpeechError::with_message(
                SpeechErrorCode::SpeechResourceLimit,
                format!(
                    "Mikrofon kaydı en fazla {} saat sürebilir.",
                    self.max_recording_secs / 3600
                ),
            ));
        }
        Ok(())
    }

    /// Conservative: source + the normalized WAV (probed duration, or the
    /// worst case when unknown) + workspace headroom.
    pub fn required_disk_bytes(&self, source_bytes: u64, duration_secs: Option<f64>) -> u64 {
        let secs = duration_secs
            .map(|d| d.ceil().max(0.0) as u64)
            .unwrap_or(self.max_audio_secs)
            .min(self.max_audio_secs + 1);
        source_bytes + secs * NORMALIZED_BYTES_PER_SEC + self.workspace_bytes
    }

    /// `free = None` (unknown, e.g. non-Windows) skips the check rather than
    /// blocking a job on a number we could not read.
    pub fn check_disk(&self, source_bytes: u64, duration_secs: Option<f64>, free: Option<u64>) -> Result<(), SpeechError> {
        let Some(free) = free else { return Ok(()) };
        let need = self.required_disk_bytes(source_bytes, duration_secs);
        if free < need {
            return Err(SpeechError::with_message(
                SpeechErrorCode::SpeechResourceLimit,
                format!(
                    "Diskte yeterli boş alan yok. Bu işlem için yaklaşık {} MB boş alan gerekiyor.",
                    mb(need) + 1
                ),
            ));
        }
        Ok(())
    }
}

/// Free bytes available to the current user on the volume holding `dir`.
#[cfg(windows)]
pub fn free_disk_bytes(dir: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated; the three out-pointers are valid u64s.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some(avail)
}

#[cfg(not(windows))]
pub fn free_disk_bytes(_dir: &std::path::Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    const L: SpeechLimits = DEFAULT_LIMITS;

    #[test]
    fn input_size_boundaries() {
        assert_eq!(L.check_input_size(0).unwrap_err().code, SpeechErrorCode::SpeechInputUnsupported);
        assert!(L.check_input_size(1).is_ok());
        assert!(L.check_input_size(500 * 1024 * 1024).is_ok());
        let e = L.check_input_size(500 * 1024 * 1024 + 1).unwrap_err();
        assert_eq!(e.code, SpeechErrorCode::SpeechResourceLimit);
        assert!(e.message().contains("500 MB"));
    }

    #[test]
    fn audio_duration_boundaries() {
        assert!(L.check_duration(0.0).is_ok());
        assert!(L.check_duration(3.0 * 3600.0).is_ok());
        let e = L.check_duration(3.0 * 3600.0 + 0.001).unwrap_err();
        assert_eq!(e.code, SpeechErrorCode::SpeechResourceLimit);
        assert!(e.message().contains("3 saat"));
    }

    #[test]
    fn recording_duration_boundaries() {
        assert!(L.check_recording_duration(2.0 * 3600.0).is_ok());
        assert_eq!(
            L.check_recording_duration(2.0 * 3600.0 + 1.0).unwrap_err().code,
            SpeechErrorCode::SpeechResourceLimit
        );
    }

    #[test]
    fn disk_requirement_is_source_plus_wav_plus_workspace() {
        let need = L.required_disk_bytes(100_000_000, Some(60.0));
        assert_eq!(need, 100_000_000 + 60 * 32_000 + L.workspace_bytes);
        // unknown duration -> worst case (max audio duration)
        let worst = L.required_disk_bytes(0, None);
        assert_eq!(worst, 3 * 3600 * 32_000 + L.workspace_bytes);
    }

    #[test]
    fn disk_check_boundaries_and_unknown_free_space() {
        let need = L.required_disk_bytes(1_000_000, Some(10.0));
        assert!(L.check_disk(1_000_000, Some(10.0), Some(need)).is_ok());
        let e = L.check_disk(1_000_000, Some(10.0), Some(need - 1)).unwrap_err();
        assert_eq!(e.code, SpeechErrorCode::SpeechResourceLimit);
        assert!(e.message().contains("boş alan"));
        assert!(L.check_disk(1_000_000, Some(10.0), None).is_ok());
    }

    #[test]
    fn concurrency_default_is_one_and_limits_are_the_approved_values() {
        assert_eq!(L.max_concurrent_jobs, 1);
        assert_eq!(L.max_input_bytes, 524_288_000);
        assert_eq!(L.max_audio_secs, 10_800);
        assert_eq!(L.max_recording_secs, 7_200);
    }
}
