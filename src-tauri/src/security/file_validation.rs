//! Small, focused helpers for validating/sanitizing individual file
//! name components (as opposed to full paths - see `path_validation`).

/// Strips anything from `name` that could act as a path separator or
/// directory-traversal sequence, so a value derived from user-supplied
/// text can be safely used as a single path *component* (e.g. a
/// filename built inside a job's own temp directory).
///
/// This is defense in depth: today every call site already builds
/// filenames from `Path::file_stem()` of an already-validated path
/// (which cannot itself contain a separator), never from raw
/// user-typed text. This function protects future call sites from
/// accidentally regressing that invariant.
pub fn sanitize_filename_component(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' => '_',
            c => c,
        })
        .collect();

    let cleaned = cleaned.trim();

    let cleaned = if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "output"
    } else if is_windows_reserved_name(cleaned) {
        // On Windows, CON/PRN/AUX/NUL/COM1-9/LPT1-9 (with or without an
        // extension - "con.txt" is just as reserved as "con") name a
        // device, not a regular file. Creating/writing one either fails
        // outright or is silently redirected to the device depending on
        // the API used, on every OS this app ships for - not just when
        // actually running on Windows - so a job's temp directory can
        // never be poisoned by one regardless of which platform produced
        // the archive/document the name came from.
        "output"
    } else {
        cleaned
    };

    cleaned.to_string()
}

/// True if `name`'s stem (the part before the first `.`) is one of
/// Windows' reserved device names, case-insensitively.
fn is_windows_reserved_name(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = name.split('.').next().unwrap_or(name);
    RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_separators() {
        assert_eq!(sanitize_filename_component("a/b\\c"), "a_b_c");
    }

    #[test]
    fn rejects_traversal() {
        assert_eq!(sanitize_filename_component(".."), "output");
        assert_eq!(sanitize_filename_component("."), "output");
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(sanitize_filename_component(""), "output");
        assert_eq!(sanitize_filename_component("   "), "output");
    }

    #[test]
    fn keeps_normal_names() {
        assert_eq!(sanitize_filename_component("my-report_v2"), "my-report_v2");
    }

    #[test]
    fn rejects_windows_reserved_device_names() {
        assert_eq!(sanitize_filename_component("CON"), "output");
        assert_eq!(sanitize_filename_component("con"), "output");
        assert_eq!(sanitize_filename_component("con.txt"), "output");
        assert_eq!(sanitize_filename_component("LPT1"), "output");
        assert_eq!(sanitize_filename_component("COM9.tar.gz"), "output");
        // Not reserved - must not be false-positived on.
        assert_eq!(sanitize_filename_component("console.txt"), "console.txt");
        assert_eq!(sanitize_filename_component("COM10"), "COM10");
    }
}
