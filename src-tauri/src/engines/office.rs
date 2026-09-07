//! Office document conversion, backed today by a system-installed
//! LibreOffice (`soffice --headless --convert-to`).
//!
//! This is the ONLY place LibreOffice is invoked from. Every Office<->PDF
//! and Office<->Office conversion in `converter.rs` calls into this
//! module instead of building its own `soffice` command line - see
//! `convert()` below for the shared implementation, and
//! `reconstruction.rs` for why the PDF-input direction is kept as an
//! architecturally distinct caller even though it currently reuses this
//! same LibreOffice call.

use super::engine_id::EngineId;
use super::error::EngineError;
use super::process;
use super::resolver;
use std::path::Path;

/// Converts a filesystem path to the `file://` URI form LibreOffice's
/// `-env:UserInstallation=` flag requires - forward slashes and a `file://`
/// scheme, even on Windows.
fn to_file_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{}", s)
    } else {
        format!("file:///{}", s)
    }
}

/// One LibreOffice `soffice --headless --convert-to` invocation.
///
/// `filter` is the LibreOffice export filter name (`"pdf"`, `"docx"`,
/// `"odt"`, ...). `use_pdf_import_filter` requests
/// `--infilter=writer_pdf_import`, required when the *input* is a PDF
/// being reconstructed into an editable format.
///
/// `job_dir` is the caller's isolated per-job temp directory (see
/// `security::temp::JobTempDir`). This function gives the LibreOffice
/// process its own `-env:UserInstallation=` profile *inside* that
/// directory rather than letting it use the shared default profile.
/// Without this, two conversions running at the same time - or one
/// running while the user's own LibreOffice happens to be open - race on
/// the same profile lock file, and one of them fails with an opaque
/// "another instance is running" error. This is a real, documented
/// LibreOffice headless-mode pitfall, not a hypothetical one, and it's
/// exactly the kind of concurrency bug that's invisible until two
/// conversions genuinely overlap.
pub fn convert(
    input: &str,
    output: &str,
    filter: &str,
    use_pdf_import_filter: bool,
    job_dir: &Path,
) -> Result<String, EngineError> {
    let input_path = Path::new(input);
    let output_path = Path::new(output);
    let output_dir = output_path.parent().unwrap_or(Path::new("."));

    std::fs::create_dir_all(output_dir)
        .map_err(|e| EngineError::output_invalid("Could not prepare the output folder.").with_detail(e.to_string()))?;

    let resolved = resolver::resolve(EngineId::Office)?;

    let profile_dir = job_dir.join("loffice_profile");
    let user_installation_arg = format!("-env:UserInstallation={}", to_file_uri(&profile_dir));

    let output_dir_abs = crate::converter::get_absolute_path(output_dir);
    let input_abs = crate::converter::get_absolute_path(input_path);

    let mut args = vec!["--headless".to_string()];
    if use_pdf_import_filter {
        args.push("--infilter=writer_pdf_import".to_string());
    }
    args.push(user_installation_arg);
    args.push("--convert-to".to_string());
    args.push(filter.to_string());
    args.push("--outdir".to_string());
    args.push(output_dir_abs.to_string_lossy().to_string());
    args.push(input_abs.to_string_lossy().to_string());

    process::run(&resolved, &args, job_dir)?;

    let input_stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let produced = output_dir_abs.join(format!("{}.{}", input_stem, filter));

    if !produced.exists() {
        return Err(EngineError::output_invalid(
            "Office conversion finished but did not produce the expected output file.",
        ));
    }

    if produced != output_path {
        std::fs::rename(&produced, output_path).map_err(|e| {
            EngineError::output_invalid("Could not finalize the converted file.").with_detail(e.to_string())
        })?;
    }

    Ok(output.to_string())
}

/// DOCX/XLSX/PPTX/ODT/ODS/ODP -> PDF - the six V1-core Office->PDF pairs,
/// as a dedicated named entry point. `converter.rs`'s category converters
/// currently call `convert()` directly with a dynamically-computed
/// filter (which is sometimes, but not always, "pdf"); this wrapper is
/// the intended call site once a dedicated "Export to PDF" action exists
/// in the UI, so it's kept even though nothing calls it yet.
#[allow(dead_code)]
pub fn convert_to_pdf(input: &str, output: &str, job_dir: &Path) -> Result<String, EngineError> {
    convert(input, output, "pdf", false, job_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_uses_forward_slashes_and_file_scheme() {
        let uri = to_file_uri(Path::new(r"C:\temp\job\loffice_profile"));
        assert!(uri.starts_with("file://"));
        assert!(!uri.contains('\\'));
    }

    #[test]
    fn missing_office_engine_surfaces_structured_error_not_raw_os_error() {
        // Phase 1 policy: this dev/CI machine never has LibreOffice
        // installed, so `convert` must fail with the structured
        // OFFICE_ENGINE_NOT_AVAILABLE error rather than an OS-level
        // "program not found" string leaking through.
        if resolver::is_available(EngineId::Office) {
            return;
        }
        let job_dir = std::env::temp_dir().join(format!(
            "localconvert_office_engine_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&job_dir).unwrap();

        let result = convert("input.docx", "output.pdf", "pdf", false, &job_dir);

        let _ = std::fs::remove_dir_all(&job_dir);

        let err = result.unwrap_err();
        assert_eq!(err.code(), "OFFICE_ENGINE_NOT_AVAILABLE");
        assert!(!err.to_string().contains("program not found"));
    }
}
