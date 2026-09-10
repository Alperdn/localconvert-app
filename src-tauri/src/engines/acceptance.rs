//! Step 4 payload/integration acceptance test - NOT part of the normal
//! test suite (`#[ignore]`), because it requires the real bundled Office
//! Engine payload to be present under `engines/office/` next to the test
//! binary. Run explicitly with:
//!
//! ```text
//! cargo test --test-threads=1 -- --ignored --nocapture office_engine_acceptance
//! ```
//!
//! This is throwaway acceptance-test infrastructure, not shipped
//! anywhere - it's the only place in this codebase allowed to shell out
//! to `cmd /c mklink` (to make the bundled engine visible to the test
//! binary's own `current_exe()`-relative resolution, mirroring how a
//! real installed app finds it next to its own executable) and to print
//! things production code never would (see `engines::office_manifest`'s
//! own no-path-in-messages contract, which this file deliberately does
//! NOT have to follow, since nothing here reaches a UI).

#![cfg(test)]

use crate::engines::engine_id::EngineId;
use crate::engines::office_manifest::{self, OfficeEngineStatus};
use crate::engines::resolver::{self, EngineTier};
use crate::security::temp::JobTempDir;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

fn repo_engines_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("engines")
}

/// Makes the real `src-tauri/engines/` tree visible at the location
/// `resolver::bundled_root()` actually looks (next to the *test binary's*
/// own executable) via a directory junction - junctions don't require
/// admin/Developer Mode on Windows, unlike symlinks.
fn link_engines_next_to_test_binary() -> PathBuf {
    let exe_dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let link = exe_dir.join("engines");
    if link.exists() {
        return link;
    }
    let target = repo_engines_dir();
    let status = std::process::Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            &link.to_string_lossy(),
            &target.to_string_lossy(),
        ])
        .status()
        .expect("failed to invoke mklink");
    assert!(status.success(), "mklink /J failed - is engines/ actually populated?");
    link
}

fn unlink(link: &Path) {
    let _ = std::process::Command::new("cmd").args(["/C", "rmdir", &link.to_string_lossy()]).status();
}

fn write_zip(path: &Path, entries: &[(&str, &[u8], bool)]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for (name, content, store_uncompressed) in entries {
        let options = if *store_uncompressed {
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
        } else {
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
        };
        zip.start_file(*name, options).unwrap();
        zip.write_all_helper(content);
    }
    zip.finish().unwrap();
}

// zip::ZipWriter's write_all comes from `std::io::Write`; name the helper
// so the call above reads clearly at the call site.
trait WriteAllHelper {
    fn write_all_helper(&mut self, buf: &[u8]);
}
impl<W: std::io::Write + std::io::Seek> WriteAllHelper for zip::ZipWriter<W> {
    fn write_all_helper(&mut self, buf: &[u8]) {
        use std::io::Write;
        self.write_all(buf).unwrap();
    }
}

/// Hand-built minimal-but-spec-valid ODP (OpenDocument Presentation),
/// one page with a text box. LibreOffice can also import plain text/CSV
/// into Writer/Calc directly (used for the ODT/ODS fixtures below), but
/// there's no equally standard plain-text->Impress import, so this one
/// is constructed directly instead of bootstrapped from a simpler format.
fn build_minimal_odp(path: &Path) {
    let content_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
 xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
 xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0"
 xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"
 office:version="1.2">
 <office:automatic-styles/>
 <office:body>
  <office:presentation>
   <draw:page draw:name="page1" draw:style-name="dp1" draw:master-page-name="Default">
    <draw:frame svg:x="2cm" svg:y="2cm" svg:width="10cm" svg:height="4cm">
     <draw:text-box><text:p>LocalConvert Step 4 acceptance test fixture.</text:p></draw:text-box>
    </draw:frame>
   </draw:page>
  </office:presentation>
 </office:body>
</office:document-content>"#;

    let styles_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
 xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
 xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"
 xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0"
 office:version="1.2">
 <office:styles><style:style style:name="dp1" style:family="drawing-page"/></office:styles>
 <office:automatic-styles>
  <style:page-layout style:name="PL1">
   <style:page-layout-properties fo:margin-top="0cm" fo:margin-bottom="0cm" fo:margin-left="0cm" fo:margin-right="0cm" fo:page-width="28cm" fo:page-height="21cm" style:print-orientation="landscape"/>
  </style:page-layout>
 </office:automatic-styles>
 <office:master-styles><style:master-page style:name="Default" style:page-layout-name="PL1"/></office:master-styles>
</office:document-styles>"#;

    let meta_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" office:version="1.2"><office:meta/></office:document-meta>"#;

    let settings_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-settings xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" office:version="1.2"><office:settings/></office:document-settings>"#;

    let manifest_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">
 <manifest:file-entry manifest:full-path="/" manifest:version="1.2" manifest:media-type="application/vnd.oasis.opendocument.presentation"/>
 <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
 <manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/>
 <manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/>
 <manifest:file-entry manifest:full-path="settings.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#;

    write_zip(
        path,
        &[
            ("mimetype", b"application/vnd.oasis.opendocument.presentation", true),
            ("META-INF/manifest.xml", manifest_xml, false),
            ("content.xml", content_xml, false),
            ("styles.xml", styles_xml, false),
            ("meta.xml", meta_xml, false),
            ("settings.xml", settings_xml, false),
        ],
    );
}

fn is_pdf(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    bytes.len() > 4 && &bytes[0..4] == b"%PDF"
}

fn soffice_process_count() -> usize {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq soffice.bin", "/FO", "CSV", "/NH"])
        .output();
    match output {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            text.lines().filter(|l| l.to_lowercase().contains("soffice")).count()
        }
        Err(_) => 0,
    }
}

#[test]
#[ignore = "requires the real bundled engines/office payload - run explicitly"]
fn office_engine_acceptance() {
    let link = link_engines_next_to_test_binary();

    // --- 1. System-independence precondition ---------------------------
    assert!(
        std::env::var("LOCALCONVERT_ALLOW_SYSTEM_OFFICE_FALLBACK").is_err(),
        "acceptance run must NOT have system Office fallback enabled"
    );

    // --- 2. Self-check must report AVAILABLE against the real payload --
    let report = office_manifest::self_check();
    println!("[acceptance] self_check() = {:?} / {:?}", report.status, report.message);
    assert_eq!(report.status, OfficeEngineStatus::Available);

    // --- 3. Resolver must pick the Bundled tier, never System ----------
    let resolved = resolver::resolve(EngineId::Office).expect("bundled engine must resolve");
    println!("[acceptance] resolver tier = {:?}", resolved.tier);
    assert_eq!(resolved.tier, EngineTier::Bundled);

    let system_office_on_path = crate::tools::check_tool_installed("soffice").installed;
    println!("[acceptance] system soffice on PATH/common install locations: {}", system_office_on_path);
    // Whether or not a system copy exists is irrelevant - the assertion
    // above (tier == Bundled) is what actually proves independence.

    let procs_before = soffice_process_count();

    // --- 4. Build fixtures ----------------------------------------------
    let fixtures_dir = std::env::temp_dir().join(format!("loc_fixtures_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&fixtures_dir).unwrap();

    let txt_path = fixtures_dir.join("fixture.txt");
    std::fs::write(&txt_path, "LocalConvert Step 4 acceptance test fixture.\r\n").unwrap();

    let csv_path = fixtures_dir.join("fixture.csv");
    std::fs::write(&csv_path, "Name,Value\r\nLocalConvert,25.8.7\r\n").unwrap();

    let odp_path = fixtures_dir.join("fixture.odp");
    build_minimal_odp(&odp_path);

    let bootstrap = |input: &Path, output: &Path, filter: &str| {
        let job = JobTempDir::new().unwrap();
        let job_dir_path = job.path().to_path_buf();
        let result = crate::engines::office::convert(
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            filter,
            false,
            &job_dir_path,
        );
        result.unwrap_or_else(|e| panic!("bootstrap conversion {:?} -> {} failed: {:?}", input, filter, e));
        drop(job); // JobTempDir's Drop impl does the cleanup - must run before checking.
        assert!(!job_dir_path.exists(), "job dir must be cleaned up after bootstrap conversion");
    };

    let odt_path = fixtures_dir.join("fixture.odt");
    bootstrap(&txt_path, &odt_path, "odt");

    let ods_path = fixtures_dir.join("fixture.ods");
    bootstrap(&csv_path, &ods_path, "ods");

    let docx_path = fixtures_dir.join("fixture.docx");
    bootstrap(&odt_path, &docx_path, "docx");

    let xlsx_path = fixtures_dir.join("fixture.xlsx");
    bootstrap(&ods_path, &xlsx_path, "xlsx");

    let pptx_path = fixtures_dir.join("fixture.pptx");
    bootstrap(&odp_path, &pptx_path, "pptx");

    println!("[acceptance] fixtures prepared: {:?}", fixtures_dir);

    // --- 5. THE acceptance conversions: each fixture -> PDF -------------
    let cases: &[(&str, &Path)] = &[
        ("DOCX", &docx_path),
        ("XLSX", &xlsx_path),
        ("PPTX", &pptx_path),
        ("ODT", &odt_path),
        ("ODS", &ods_path),
        ("ODP", &odp_path),
    ];

    for (label, input) in cases {
        let job = JobTempDir::new().unwrap();
        let job_dir_path = job.path().to_path_buf();
        let out_path = fixtures_dir.join(format!("result_{}.pdf", label.to_lowercase()));

        let resolved = resolver::resolve(EngineId::Office).unwrap();
        assert_eq!(resolved.tier, EngineTier::Bundled, "{} conversion must use the bundled tier", label);

        let result = crate::engines::office::convert(
            input.to_str().unwrap(),
            out_path.to_str().unwrap(),
            "pdf",
            false,
            &job_dir_path,
        );
        let output_path = result.unwrap_or_else(|e| panic!("{} -> PDF failed: {}", label, e));
        drop(job); // JobTempDir's Drop impl does the cleanup - must run before checking.

        let produced = Path::new(&output_path);
        assert!(produced.exists(), "{} -> PDF: output does not exist", label);
        let size = std::fs::metadata(produced).unwrap().len();
        assert!(size > 0, "{} -> PDF: output is zero bytes", label);
        assert!(is_pdf(produced), "{} -> PDF: output does not start with %PDF signature", label);
        assert!(!job_dir_path.exists(), "{} -> PDF: job temp dir (incl. LibreOffice profile) was not cleaned up", label);

        println!("[acceptance] {} -> PDF OK ({} bytes) at {:?}", label, size, produced);
    }

    // --- 6. Orphan-process check -----------------------------------------
    // Give any just-exited soffice.bin a moment to fully tear down before
    // sampling, so a normal, slightly-delayed process exit isn't
    // misreported as an orphan.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let procs_after = soffice_process_count();
    println!("[acceptance] soffice.bin processes before={}, after={}", procs_before, procs_after);
    assert_eq!(
        procs_after, procs_before,
        "orphan soffice.bin process(es) left running after conversions completed - BLOCKER"
    );

    let _ = std::fs::remove_dir_all(&fixtures_dir);
    unlink(&link);
}
