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
    } else {
        cleaned
    };

    cleaned.to_string()
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
}
