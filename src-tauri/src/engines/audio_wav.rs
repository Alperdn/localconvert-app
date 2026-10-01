//! Minimal, dependency-free reader for the one WAV shape the speech pipeline
//! produces/consumes (PCM s16le), plus a cheap energy pre-check.
//!
//! Used for: (1) exact post-normalization duration (the limit check that does
//! not depend on container metadata), (2) recognizing already-normalized
//! audio so FFmpeg can be skipped (microphone path), (3) "is there any
//! speech-level energy at all" so pure silence never reaches Whisper, which
//! is known to hallucinate text on silence.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const SPEECH_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub is_pcm: bool,
    pub data_offset: u64,
    pub data_len: u64,
}

impl WavInfo {
    pub fn is_speech_normalized(&self) -> bool {
        self.is_pcm && self.channels == 1 && self.sample_rate == SPEECH_SAMPLE_RATE && self.bits_per_sample == 16
    }
    pub fn duration_secs(&self) -> f64 {
        let bytes_per_sec = self.sample_rate as u64 * self.channels as u64 * (self.bits_per_sample as u64 / 8);
        if bytes_per_sec == 0 {
            0.0
        } else {
            self.data_len as f64 / bytes_per_sec as f64
        }
    }
}

/// Parses the RIFF/WAVE header. Returns `None` for anything that is not a
/// well-formed WAV (callers then fall back to FFmpeg, which is the safe path).
pub fn read_wav_info(path: &Path) -> Option<WavInfo> {
    let mut f = std::fs::File::open(path).ok()?;
    let file_len = f.metadata().ok()?.len();
    let mut hdr = [0u8; 12];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return None;
    }
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    loop {
        let mut ch = [0u8; 8];
        if f.read_exact(&mut ch).is_err() {
            return None;
        }
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        let pos = f.stream_position().ok()?;
        match &ch[0..4] {
            b"fmt " => {
                if size < 16 {
                    return None;
                }
                let mut b = [0u8; 16];
                f.read_exact(&mut b).ok()?;
                let tag = u16::from_le_bytes([b[0], b[1]]);
                let channels = u16::from_le_bytes([b[2], b[3]]);
                let rate = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                let bits = u16::from_le_bytes([b[14], b[15]]);
                fmt = Some((tag, channels, rate, bits));
            }
            b"data" => {
                let (tag, channels, rate, bits) = fmt?;
                // A streaming writer may leave 0/0xFFFFFFFF sizes; clamp to the file.
                let avail = file_len.saturating_sub(pos);
                let len = if size == 0 || size > avail { avail } else { size };
                return Some(WavInfo {
                    channels,
                    sample_rate: rate,
                    bits_per_sample: bits,
                    is_pcm: tag == 1,
                    data_offset: pos,
                    data_len: len,
                });
            }
            _ => {}
        }
        // chunks are word-aligned
        let next = pos + size + (size & 1);
        if next >= file_len {
            return None;
        }
        f.seek(SeekFrom::Start(next)).ok()?;
    }
}

/// Energy summary of a normalized (16 kHz mono s16le) WAV.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnergyReport {
    pub windows: u64,
    pub active_windows: u64,
}

impl EnergyReport {
    pub fn active_seconds(&self) -> f64 {
        self.active_windows as f64 * WINDOW_MS as f64 / 1000.0
    }
    /// True when there is effectively nothing above the speech-level floor.
    pub fn looks_silent(&self) -> bool {
        self.active_seconds() < MIN_ACTIVE_SECONDS
    }
}

const WINDOW_MS: u64 = 50;
/// RMS (of full-scale 32768) below which a 50 ms window counts as silence:
/// about -50 dBFS. Room noise on a quiet mic sits near -60..-70 dBFS; soft
/// speech is well above -45 dBFS.
const ACTIVE_RMS: f64 = 104.0;
/// Less than this much above-floor audio in the whole file = "no speech".
const MIN_ACTIVE_SECONDS: f64 = 0.3;

pub fn analyze_energy(path: &Path, info: &WavInfo) -> std::io::Result<EnergyReport> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(info.data_offset))?;
    let mut remaining = info.data_len;
    let win_samples = (SPEECH_SAMPLE_RATE as u64 * WINDOW_MS / 1000) as usize;
    let mut buf = vec![0u8; win_samples * 2];
    let mut windows = 0u64;
    let mut active = 0u64;
    while remaining >= buf.len() as u64 {
        f.read_exact(&mut buf)?;
        remaining -= buf.len() as u64;
        let mut acc = 0f64;
        for c in buf.chunks_exact(2) {
            let s = i16::from_le_bytes([c[0], c[1]]) as f64;
            acc += s * s;
        }
        let rms = (acc / win_samples as f64).sqrt();
        windows += 1;
        if rms >= ACTIVE_RMS {
            active += 1;
        }
    }
    Ok(EnergyReport { windows, active_windows: active })
}

/// Writes a canonical 44-byte-header WAV (used by tests and, in Phase D, to
/// wrap streamed microphone PCM).
#[cfg_attr(not(test), allow(dead_code))] // used by the microphone flow (Phase D)
pub fn write_wav_header(out: &mut impl std::io::Write, pcm_len: u32) -> std::io::Result<()> {
    let rate = SPEECH_SAMPLE_RATE;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + pcm_len).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&(rate * 2).to_le_bytes())?;
    out.write_all(&2u16.to_le_bytes())?;
    out.write_all(&16u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&pcm_len.to_le_bytes())
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;
    use std::path::PathBuf;

    /// Writes a 16 kHz mono s16 WAV of `secs` seconds; `amp` = sine amplitude (0 = digital silence).
    pub fn write_tone(dir: &Path, name: &str, secs: f64, amp: f64) -> PathBuf {
        let n = (secs * SPEECH_SAMPLE_RATE as f64) as usize;
        let mut pcm = Vec::with_capacity(n * 2);
        for i in 0..n {
            let v = (amp * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / SPEECH_SAMPLE_RATE as f64).sin()) as i16;
            pcm.extend_from_slice(&v.to_le_bytes());
        }
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        write_wav_header(&mut f, pcm.len() as u32).unwrap();
        std::io::Write::write_all(&mut f, &pcm).unwrap();
        p
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::write_tone;
    use super::*;

    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("meb_wav_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn parses_normalized_wav_and_reports_exact_duration() {
        let d = tmp();
        let p = write_tone(&d, "a.wav", 2.5, 8000.0);
        let info = read_wav_info(&p).unwrap();
        assert!(info.is_speech_normalized());
        assert!((info.duration_secs() - 2.5).abs() < 0.001);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn rejects_non_wav_and_truncated_headers() {
        let d = tmp();
        std::fs::write(d.join("x.wav"), b"not a wav at all, definitely").unwrap();
        assert!(read_wav_info(&d.join("x.wav")).is_none());
        std::fs::write(d.join("y.wav"), b"RIFF").unwrap();
        assert!(read_wav_info(&d.join("y.wav")).is_none());
        std::fs::write(d.join("z.wav"), b"").unwrap();
        assert!(read_wav_info(&d.join("z.wav")).is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn digital_silence_and_room_noise_look_silent_but_a_tone_does_not() {
        let d = tmp();
        let silent = write_tone(&d, "s.wav", 3.0, 0.0);
        let noise = write_tone(&d, "n.wav", 3.0, 20.0); // ~ -64 dBFS
        let loud = write_tone(&d, "l.wav", 3.0, 8000.0);
        for (p, expect_silent) in [(&silent, true), (&noise, true), (&loud, false)] {
            let info = read_wav_info(p).unwrap();
            assert_eq!(analyze_energy(p, &info).unwrap().looks_silent(), expect_silent, "{:?}", p);
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn non_normalized_wav_is_not_reported_as_normalized() {
        let d = tmp();
        let p = write_tone(&d, "a.wav", 1.0, 1000.0);
        let mut b = std::fs::read(&p).unwrap();
        b[24..28].copy_from_slice(&44_100u32.to_le_bytes()); // sample rate field
        std::fs::write(&p, b).unwrap();
        assert!(!read_wav_info(&p).unwrap().is_speech_normalized());
        let _ = std::fs::remove_dir_all(d);
    }
}
