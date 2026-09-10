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
}

impl EngineId {
    /// All known engines - used by the capability model and tests. Never
    /// used to turn external input into an `EngineId`.
    pub const ALL: [EngineId; 4] = [
        EngineId::Office,
        EngineId::Ffmpeg,
        EngineId::Ffprobe,
        EngineId::Tesseract,
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
        }
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
        }
    }

    /// Prefix used to build error codes like `OFFICE_ENGINE_NOT_AVAILABLE`.
    pub fn code_prefix(self) -> &'static str {
        match self {
            EngineId::Office => "OFFICE",
            EngineId::Ffmpeg => "FFMPEG",
            EngineId::Ffprobe => "FFPROBE",
            EngineId::Tesseract => "TESSERACT",
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
