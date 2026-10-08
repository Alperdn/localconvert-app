//! Fixed, backend-only identity for external engines LocalConvert can invoke.
//!
//! There is deliberately no code path that turns a frontend-supplied string
//! into an `EngineId` and then into a spawned process. Tauri commands only
//! ever pass file paths and conversion options *into* an already-chosen
//! engine; nothing in the IPC surface lets the frontend pick which engine
//! or which executable name runs.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineId {
    Office,
    Ffmpeg,
    Ffprobe,
    Tesseract,
    /// whisper.cpp speech-to-text sidecar (bundled only - never PATH).
    Speech,
    /// Minimal, audio-only, LGPL FFmpeg built for speech preprocessing
    /// (bundled only - never PATH). Deliberately a DIFFERENT identity from
    /// `Ffmpeg`, which still means "whatever full FFmpeg the user has
    /// installed" for video/audio conversion: the bundled build has no
    /// video codecs, so it must never silently back those features.
    AudioFfmpeg,
    /// ffprobe from the same minimal build (see `AudioFfmpeg`).
    AudioFfprobe,
    /// Ghostscript - every PDF page-level operation (merge, split,
    /// compress, rotate, watermark) and PDF rasterization.
    Ghostscript,
}

impl EngineId {
    /// All known engines - used by the capability model and tests. Never
    /// used to turn external input into an `EngineId`.
    pub const ALL: [EngineId; 8] = [
        EngineId::Office,
        EngineId::Ffmpeg,
        EngineId::Ffprobe,
        EngineId::Tesseract,
        EngineId::Speech,
        EngineId::AudioFfmpeg,
        EngineId::AudioFfprobe,
        EngineId::Ghostscript,
    ];

    /// The key this engine is registered under in `tools::TOOLS` / the
    /// PATH + common-install-path scanner in `tools.rs`. `Ffprobe` has no
    /// entry of its own there - it doesn't ship as a standalone install,
    /// it's resolved relative to wherever `Ffmpeg` was found instead (see
    /// `resolver::system_path`).
    pub fn system_tool_key(self) -> Option<&'static str> {
        match self {
            EngineId::Office => Some("soffice"),
            EngineId::Ffmpeg => Some("ffmpeg"),
            EngineId::Ffprobe => None,
            EngineId::Tesseract => Some("tesseract"),
            EngineId::Ghostscript => Some("gs"),
            // Bundled-only: no PATH / system tier exists for these.
            EngineId::Speech | EngineId::AudioFfmpeg | EngineId::AudioFfprobe => None,
        }
    }

    /// Subdirectory name this engine would live under inside a future
    /// bundled `engines/` resource folder (see `resolver::bundled_path`).
    pub fn bundle_dir_name(self) -> &'static str {
        match self {
            EngineId::Office => "office",
            EngineId::Ffmpeg => "ffmpeg",
            EngineId::Ffprobe => "ffmpeg", // ships alongside ffmpeg
            EngineId::Tesseract => "tesseract",
            EngineId::Speech => "speech",
            EngineId::AudioFfmpeg | EngineId::AudioFfprobe => "ffmpeg",
            EngineId::Ghostscript => "ghostscript",
        }
    }

    /// The executable's name as a system install puts it on `PATH`. This is
    /// the name the default system tier (`resolver::path_lookup`) searches
    /// for, and it is NOT always `bundled_exe_name` - Ghostscript's console
    /// build is `gswin64c.exe` on Windows but plain `gs` elsewhere, and a
    /// system LibreOffice is a bare `soffice`, not the nested
    /// `LibreOffice/program/soffice.exe` of the bundled payload.
    ///
    /// `None` means the engine has no system identity at all: either it is
    /// bundled-only (`Speech`, `AudioFfmpeg`, `AudioFfprobe`) or it is
    /// resolved relative to another engine rather than searched for on its
    /// own (`Ffprobe` - see the desktop resolver's `system_path`).
    pub fn system_exe_name(self) -> Option<&'static str> {
        let windows = cfg!(windows);
        Some(match self {
            EngineId::Office => {
                if windows {
                    "soffice.exe"
                } else {
                    "soffice"
                }
            }
            EngineId::Ffmpeg => {
                if windows {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                }
            }
            EngineId::Tesseract => {
                if windows {
                    "tesseract.exe"
                } else {
                    "tesseract"
                }
            }
            EngineId::Ghostscript => {
                if windows {
                    "gswin64c.exe"
                } else {
                    "gs"
                }
            }
            EngineId::Ffprobe
            | EngineId::Speech
            | EngineId::AudioFfmpeg
            | EngineId::AudioFfprobe => return None,
        })
    }

    /// The executable's path *relative to* `engines/<bundle_dir_name()>/`
    /// (not necessarily a bare filename - Office's real bundled payload
    /// nests `soffice.exe` under `LibreOffice/program/`, matching the
    /// layout LibreOffice's own installer produces and that
    /// `office_manifest::OfficeManifest::executable_relative_path` (Step
    /// 4's manifest) also names; keeping both in agreement is exactly
    /// what `office_manifest::self_check`'s final cross-check against
    /// `resolver::is_available` exists to catch).
    pub fn bundled_exe_name(self) -> &'static str {
        match self {
            EngineId::Office => {
                if cfg!(windows) {
                    "LibreOffice/program/soffice.exe"
                } else {
                    "LibreOffice/program/soffice"
                }
            }
            EngineId::Ffmpeg => {
                if cfg!(windows) {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                }
            }
            EngineId::Ffprobe => {
                if cfg!(windows) {
                    "ffprobe.exe"
                } else {
                    "ffprobe"
                }
            }
            EngineId::Tesseract => {
                if cfg!(windows) {
                    "tesseract.exe"
                } else {
                    "tesseract"
                }
            }
            EngineId::Speech => {
                if cfg!(windows) {
                    "bin/whisper-cli.exe"
                } else {
                    "bin/whisper-cli"
                }
            }
            EngineId::AudioFfmpeg => {
                if cfg!(windows) {
                    "bin/ffmpeg.exe"
                } else {
                    "bin/ffmpeg"
                }
            }
            EngineId::AudioFfprobe => {
                if cfg!(windows) {
                    "bin/ffprobe.exe"
                } else {
                    "bin/ffprobe"
                }
            }
            EngineId::Ghostscript => {
                if cfg!(windows) {
                    "bin/gswin64c.exe"
                } else {
                    "bin/gs"
                }
            }
        }
    }

    /// User-safe display name - fine to put directly in error messages
    /// shown to the user. Never an executable name or filesystem path.
    pub fn display_name(self) -> &'static str {
        match self {
            EngineId::Office => "Office conversion",
            EngineId::Ffmpeg => "Video/audio conversion",
            EngineId::Ffprobe => "Media inspection",
            EngineId::Tesseract => "OCR",
            EngineId::Speech => "Speech recognition",
            EngineId::AudioFfmpeg => "Audio preprocessing",
            EngineId::AudioFfprobe => "Audio inspection",
            EngineId::Ghostscript => "PDF processing",
        }
    }

    /// Prefix used to build error codes like `OFFICE_ENGINE_NOT_AVAILABLE`.
    pub fn code_prefix(self) -> &'static str {
        match self {
            EngineId::Office => "OFFICE",
            EngineId::Ffmpeg => "FFMPEG",
            EngineId::Ffprobe => "FFPROBE",
            EngineId::Tesseract => "TESSERACT",
            EngineId::Speech => "SPEECH",
            EngineId::AudioFfmpeg => "AUDIO_FFMPEG",
            EngineId::AudioFfprobe => "AUDIO_FFPROBE",
            EngineId::Ghostscript => "GHOSTSCRIPT",
        }
    }

    /// Strictly internal/test use: maps a name back to a variant. This is
    /// NOT exposed via any Tauri command - no command accepts a tool name
    /// as input in the first place. It exists only so a test can assert
    /// that arbitrary/attacker-supplied strings never resolve to anything,
    /// i.e. that there is no hidden name-to-engine path anywhere.
    #[cfg(test)]
    fn from_str_for_tests(name: &str) -> Option<EngineId> {
        match name {
            "office" | "soffice" => Some(EngineId::Office),
            "ffmpeg" => Some(EngineId::Ffmpeg),
            "ffprobe" => Some(EngineId::Ffprobe),
            "tesseract" => Some(EngineId::Tesseract),
            "gs" => Some(EngineId::Ghostscript),
            _ => None,
        }
    }
}

impl fmt::Display for EngineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_frontend_supplied_names_never_resolve_to_an_engine() {
        let attacker_supplied = [
            "cmd",
            "cmd.exe",
            "powershell",
            "powershell.exe",
            "sh",
            "../../evil.exe",
            "C:\\Windows\\System32\\cmd.exe",
            "rm -rf /",
            "",
            "OFFICE", // case must matter - no case-insensitive backdoor
            "ffmpeg; calc.exe",
            "gswin64c.exe",
            "GS",
        ];
        for name in attacker_supplied {
            assert_eq!(
                EngineId::from_str_for_tests(name),
                None,
                "unexpected engine resolved for {:?}",
                name
            );
        }
    }

    #[test]
    fn known_names_resolve_to_the_expected_engine() {
        assert_eq!(EngineId::from_str_for_tests("ffmpeg"), Some(EngineId::Ffmpeg));
        assert_eq!(EngineId::from_str_for_tests("soffice"), Some(EngineId::Office));
    }
}
