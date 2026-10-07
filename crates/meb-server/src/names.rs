//! Display-name handling. A client-supplied file name is DISPLAY METADATA
//! ONLY: it is sanitized, kept in memory, used for the extension check and
//! the download name - and never used to build a filesystem path.

use axum::http::HeaderValue;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

const MAX_NAME_CHARS: usize = 200;

/// Reduces a client-supplied name to a safe display name: last path
/// component only, no control characters, no characters that are invalid
/// in Windows file names, no leading dots, bounded length. `None` if
/// nothing usable remains.
pub fn sanitize_display_name(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_NAME_CHARS).collect())
}

/// Lowercased extension after the last dot, if there is a non-empty stem.
pub fn extension_of(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext.to_lowercase())
}

pub fn stem_of(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

/// ASCII-only fallback for `filename=`: Turkish letters transliterated,
/// everything else outside a conservative set replaced.
fn ascii_fallback(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let mapped = match c {
            'ç' => 'c',
            'Ç' => 'C',
            'ğ' => 'g',
            'Ğ' => 'G',
            'ı' => 'i',
            'İ' => 'I',
            'ö' => 'o',
            'Ö' => 'O',
            'ş' => 's',
            'Ş' => 'S',
            'ü' => 'u',
            'Ü' => 'U',
            c if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') => c,
            _ => '_',
        };
        out.push(mapped);
    }
    out
}

const RFC5987: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'.')
    .remove(b'-')
    .remove(b'_')
    .remove(b'~');

/// `attachment; filename="<ascii>"; filename*=UTF-8''<percent-encoded>`.
/// The display name was sanitized at upload, and both forms here are
/// re-encoded, so CR/LF/quotes can never break out of the header.
pub fn content_disposition(download_name: &str) -> HeaderValue {
    let ascii = ascii_fallback(download_name);
    let ascii = if ascii.trim_matches(|c| c == '_' || c == '.').is_empty() {
        "donusturulen".to_string()
    } else {
        ascii
    };
    let encoded = utf8_percent_encode(download_name, RFC5987);
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"donusturulen\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_and_control_characters_are_stripped() {
        assert_eq!(
            sanitize_display_name("../../evil.png").as_deref(),
            Some("evil.png")
        );
        assert_eq!(
            sanitize_display_name(r"C:\Windows\system32\x.jpg").as_deref(),
            Some("x.jpg")
        );
        assert_eq!(
            sanitize_display_name("a\r\nb.png").as_deref(),
            Some("ab.png")
        );
        assert_eq!(
            sanitize_display_name("...hidden.png").as_deref(),
            Some("hidden.png")
        );
        assert_eq!(
            sanitize_display_name("a\"b<c>.png").as_deref(),
            Some("a_b_c_.png")
        );
        assert_eq!(sanitize_display_name("../"), None);
        assert_eq!(sanitize_display_name("   "), None);
    }

    #[test]
    fn turkish_names_survive_and_get_an_ascii_fallback() {
        let name = sanitize_display_name("Öğrenci Şöleni ı.png").unwrap();
        assert_eq!(name, "Öğrenci Şöleni ı.png");
        let header = content_disposition("Öğrenci Şöleni ı.jpg");
        let header = header.to_str().unwrap();
        assert!(
            header.starts_with("attachment; filename=\"Ogrenci Soleni i.jpg\""),
            "{header}"
        );
        assert!(
            header.contains("filename*=UTF-8''%C3%96%C4%9Frenci"),
            "{header}"
        );
    }

    #[test]
    fn extension_and_stem() {
        assert_eq!(extension_of("photo.JPG").as_deref(), Some("jpg"));
        assert_eq!(extension_of("archive.tar.gz").as_deref(), Some("gz"));
        assert_eq!(extension_of("noext"), None);
        assert_eq!(extension_of(".png"), None);
        assert_eq!(stem_of("photo.final.jpg"), "photo.final");
    }
}
