//! End-to-end tests of the web layer: the real axum router, the real native
//! image engine, and a real data root on disk. Nothing here mocks storage
//! or the engine; the only substituted runners (`BlockingRunner`,
//! `PanickingRunner`) exist to hold a job in `running` or to make it panic,
//! so lifecycle paths that a fast in-process conversion would race past can
//! be observed deterministically.

use axum::body::Body;
use axum::http::{header, Request, Response, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use meb_server::runner::{ConversionRunner, JobControl, RunError, RunRequest};
use meb_server::{App, Config, NativeImageRunner};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt;

// ── fixtures ────────────────────────────────────────────────────────────

fn jpeg_bytes(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 7) as u8, (y * 5) as u8, 90]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .unwrap();
    out.into_inner()
}

fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 200]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

/// A PNG whose header (IHDR: 64x64) is intact but whose pixel data is cut
/// off: passes the upload probe (header only) and fails the real decode.
fn truncated_png() -> Vec<u8> {
    let full = png_bytes(64, 64);
    full[..full.len() / 2].to_vec()
}

fn animated_gif() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
        let f = |c| image::Frame::new(image::RgbaImage::from_pixel(4, 4, image::Rgba(c)));
        enc.encode_frames(vec![f([255, 0, 0, 255]), f([0, 255, 0, 255])])
            .unwrap();
    }
    out
}

// ── harness ─────────────────────────────────────────────────────────────

struct TestServer {
    app: App,
    router: Router,
    root: PathBuf,
}

impl TestServer {
    fn new() -> Self {
        Self::with(|_| {}, Arc::new(NativeImageRunner))
    }

    /// Wired exactly as the real server is (`RunnerRegistry::production`),
    /// so a kind with no engine behaves here as it does in production.
    fn production() -> Self {
        Self::built(|_| {}, None)
    }

    fn with(tune: impl FnOnce(&mut Config), runner: Arc<dyn ConversionRunner>) -> Self {
        Self::built(tune, Some(runner))
    }

    fn built(tune: impl FnOnce(&mut Config), runner: Option<Arc<dyn ConversionRunner>>) -> Self {
        let root =
            std::env::temp_dir().join(format!("meb_web_test_{}", uuid::Uuid::new_v4().simple()));
        let mut config = Config::development(root.clone());
        tune(&mut config);
        let app = match runner {
            Some(runner) => App::with_runner(config, runner).unwrap(),
            None => App::new(config).unwrap(),
        };
        let router = app.router();
        let root = app.data_root().to_path_buf();
        TestServer { app, router, root }
    }

    fn client(&self) -> Client {
        Client {
            router: self.router.clone(),
            cookie: None,
        }
    }

    /// Every file currently under the data root (excluding the marker/lock).
    fn files_on_disk(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, out);
                    } else {
                        out.push(p);
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root.join("staging"), &mut out);
        walk(&self.root.join("sessions"), &mut out);
        out
    }

    fn job_dirs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(sessions) = std::fs::read_dir(self.root.join("sessions")) {
            for s in sessions.flatten() {
                if let Ok(jobs) = std::fs::read_dir(s.path().join("jobs")) {
                    out.extend(jobs.flatten().map(|j| j.path()));
                }
            }
        }
        out
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Release the instance lock before deleting the directory.
        let root = self.root.clone();
        let router = std::mem::replace(&mut self.router, Router::new());
        drop(router);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[derive(Clone)]
struct Client {
    router: Router,
    cookie: Option<String>,
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }
}

impl Client {
    async fn send(&mut self, mut req: Request<Body>) -> Response<Body> {
        if let Some(c) = &self.cookie {
            req.headers_mut().insert(header::COOKIE, c.parse().unwrap());
        }
        let resp = self.router.clone().oneshot(req).await.unwrap();
        if let Some(set) = resp.headers().get(header::SET_COOKIE) {
            let pair = set.to_str().unwrap().split(';').next().unwrap().to_string();
            self.cookie = Some(pair);
        }
        resp
    }

    async fn call(&mut self, req: Request<Body>) -> Reply {
        let resp = self.send(req).await;
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn get(&mut self, uri: &str) -> Reply {
        self.call(Request::get(uri).body(Body::empty()).unwrap())
            .await
    }

    async fn post_json(&mut self, uri: &str, body: Value) -> Reply {
        let req = Request::post(uri)
            .header("x-meb-request", "1")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.call(req).await
    }

    async fn delete(&mut self, uri: &str) -> Reply {
        let req = Request::delete(uri)
            .header("x-meb-request", "1")
            .body(Body::empty())
            .unwrap();
        self.call(req).await
    }

    async fn upload(&mut self, name: &str, bytes: Vec<u8>) -> Reply {
        let len = bytes.len();
        self.upload_raw(Some(name), Some(len as u64), bytes).await
    }

    async fn upload_raw(
        &mut self,
        name: Option<&str>,
        declared: Option<u64>,
        bytes: Vec<u8>,
    ) -> Reply {
        let mut req = Request::post("/api/v1/files")
            .header("x-meb-request", "1")
            // Deliberately misleading: the server must ignore this.
            .header(header::CONTENT_TYPE, "image/png");
        if let Some(name) = name {
            let encoded = percent_encode(name);
            req = req.header("x-file-name", encoded);
        }
        if let Some(len) = declared {
            req = req.header(header::CONTENT_LENGTH, len.to_string());
        }
        self.call(req.body(Body::from(bytes)).unwrap()).await
    }

    /// An upload whose body delivers `head` and then never sends the rest
    /// (and never ends): a client that stalls mid-upload. `declared` is
    /// deliberately larger than `head`, so the server keeps waiting for
    /// bytes that never arrive.
    async fn upload_stalling(&mut self, name: &str, head: Vec<u8>, declared: u64) -> Reply {
        use futures_util::StreamExt;
        let stream = futures_util::stream::once(async move {
            Ok::<_, std::io::Error>(axum::body::Bytes::from(head))
        })
        .chain(futures_util::stream::pending());
        let req = Request::post("/api/v1/files")
            .header("x-meb-request", "1")
            .header("x-file-name", percent_encode(name))
            .header(header::CONTENT_LENGTH, declared.to_string())
            .body(Body::from_stream(stream))
            .unwrap();
        self.call(req).await
    }

    async fn create_job(&mut self, file_id: &str, output_format: &str) -> Reply {
        self.post_json(
            "/api/v1/jobs",
            json!({ "kind": "convert", "file_id": file_id, "output_format": output_format }),
        )
        .await
    }

    async fn wait_terminal(&mut self, job_id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let r = self.get(&format!("/api/v1/jobs/{job_id}")).await;
            assert_eq!(
                r.status,
                StatusCode::OK,
                "{}",
                String::from_utf8_lossy(&r.body)
            );
            let snap = r.json();
            if matches!(
                snap["state"].as_str(),
                Some("completed" | "failed" | "cancelled")
            ) {
                return snap;
            }
            assert!(Instant::now() < deadline, "job did not finish: {snap}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_state(&mut self, job_id: &str, state: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let snap = self.get(&format!("/api/v1/jobs/{job_id}")).await.json();
            if snap["state"] == state {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "job never reached {state}: {snap}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// A structurally valid, minimal PDF: the header and trailer the upload
/// probe checks for. Not a renderable document - no engine opens it here.
fn pdf_bytes() -> Vec<u8> {
    b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n"
        .to_vec()
}

/// A ZIP holding the given members, all stored uncompressed.
fn zip_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        for (name, contents) in members {
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
    }
    buffer.into_inner()
}

/// Minimal OOXML files, identified by the part that defines each one.
fn docx_bytes() -> Vec<u8> {
    zip_bytes(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("word/document.xml", b"<document/>"),
    ])
}

fn xlsx_bytes() -> Vec<u8> {
    zip_bytes(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("xl/workbook.xml", b"<workbook/>"),
    ])
}

/// Uploads a JPEG and returns its file id.
async fn upload_jpeg(client: &mut Client) -> String {
    let r = client.upload("foto.jpg", jpeg_bytes(32, 24)).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    r.json()["file_id"].as_str().unwrap().to_string()
}

/// Holds a job in `running` until released; honours cancellation through
/// the real `JobControl`; on release delegates to the real engine.
struct BlockingRunner {
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl BlockingRunner {
    fn new() -> (Arc<Self>, Arc<(Mutex<bool>, Condvar)>) {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        (Arc::new(BlockingRunner { gate: gate.clone() }), gate)
    }
}

fn release(gate: &Arc<(Mutex<bool>, Condvar)>) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

/// Releases a `BlockingRunner`'s gate when the test ends, including on a
/// failed assertion: otherwise the parked engine thread keeps the runtime
/// alive and a regression would hang the suite instead of reporting.
struct GateGuard(Arc<(Mutex<bool>, Condvar)>);

impl Drop for GateGuard {
    fn drop(&mut self) {
        release(&self.0);
    }
}

impl ConversionRunner for BlockingRunner {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        let (lock, cv) = &*self.gate;
        let mut open = lock.lock().unwrap();
        while !*open {
            if control.is_cancelled() {
                return Err(RunError::Cancelled);
            }
            open = cv.wait_timeout(open, Duration::from_millis(10)).unwrap().0;
        }
        drop(open);
        NativeImageRunner.run(request, control)
    }
}

/// Stands in for the PDF engine: checks it was handed the job's inputs in
/// order, then writes a minimal valid PDF where the worker expects it.
struct MergeRunner {
    expected_inputs: usize,
}

impl ConversionRunner for MergeRunner {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        assert_eq!(
            request.inputs.len(),
            self.expected_inputs,
            "worker staged the wrong number of inputs"
        );
        for input in request.inputs {
            assert!(input.is_file(), "input was not staged: {input:?}");
        }
        control.report_progress(50);
        let mut merged = Vec::new();
        for input in request.inputs {
            merged.extend_from_slice(&std::fs::read(input).unwrap());
        }
        // The concatenation above is not a real merge; what matters here is
        // that the output is a well-formed PDF the worker will accept.
        let output = b"%PDF-1.7\n% merged\ntrailer\n%%EOF\n".to_vec();
        std::fs::write(request.output, output).map_err(|e| RunError::Failed {
            code: "INTERNAL_ERROR",
            detail: e.to_string(),
        })?;
        control.report_progress(100);
        Ok(())
    }
}

struct PanickingRunner;

impl ConversionRunner for PanickingRunner {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        // Reads the job's options through its spec, the way a real engine
        // does, so the panic is triggered by one specific request.
        let quality = request
            .spec
            .image_convert()
            .and_then(|spec| spec.options.quality);
        if quality == Some(13) {
            panic!("simulated engine bug");
        }
        NativeImageRunner.run(request, control)
    }
}

// ── 1. upload size rejection ────────────────────────────────────────────

#[tokio::test]
async fn upload_over_the_size_cap_is_rejected_before_and_during_streaming() {
    let server = TestServer::with(|c| c.max_upload_bytes = 1024, Arc::new(NativeImageRunner));
    let mut c = server.client();

    // Declared too large: rejected without reading the body.
    let r = c.upload("big.jpg", jpeg_bytes(256, 256)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(r.code(), "UPLOAD_TOO_LARGE");

    // Lies about its size (declares less than it sends): caught by the
    // streaming byte counter, partial file removed.
    let body = jpeg_bytes(256, 256);
    let r = c.upload_raw(Some("liar.jpg"), Some(512), body).await;
    assert!(r.status.is_client_error(), "{}", r.status);
    assert!(
        server.files_on_disk().is_empty(),
        "partial upload left behind: {:?}",
        server.files_on_disk()
    );
}

// ── 2. malformed uploads ────────────────────────────────────────────────

/// A non-image kind travels the whole generalized pipeline: several inputs
/// staged into one workspace, an engine that is not the image pipeline, an
/// output validated as a PDF (not as an image), and a download named and
/// typed by the job's spec rather than by anything the client sent.
#[tokio::test]
async fn a_multi_input_pdf_job_runs_through_the_generalized_pipeline() {
    let server = TestServer::with(|_| {}, Arc::new(MergeRunner { expected_inputs: 3 }));
    let mut c = server.client();

    let mut ids = Vec::new();
    for name in ["bir.pdf", "iki.pdf", "uc.pdf"] {
        let r = c.upload(name, pdf_bytes()).await;
        assert_eq!(
            r.status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        ids.push(r.json()["file_id"].as_str().unwrap().to_string());
    }

    let created = c
        .post_json(
            "/api/v1/jobs",
            json!({ "kind": "pdf_merge", "file_ids": ids }),
        )
        .await;
    assert_eq!(
        created.status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&created.body)
    );
    let snapshot = created.json();
    assert_eq!(snapshot["kind"], "pdf_merge");
    assert_eq!(snapshot["output_format"], "pdf");
    // Every input is reported, and `file_id` stays the first for clients
    // that predate multi-input jobs.
    assert_eq!(snapshot["file_ids"].as_array().unwrap().len(), 3);
    assert_eq!(snapshot["file_id"], ids[0].clone());

    let job_id = snapshot["job_id"].as_str().unwrap().to_string();
    let terminal = c.wait_terminal(&job_id).await;
    assert_eq!(terminal["state"], "completed", "{terminal}");
    assert_eq!(terminal["result"]["content_type"], "application/pdf");
    assert_eq!(
        terminal["result"]["output_name"], "bir_birlestirildi.pdf",
        "{terminal}"
    );

    let download = c.get(&format!("/api/v1/jobs/{job_id}/download")).await;
    assert_eq!(download.status, StatusCode::OK);
    assert_eq!(download.headers[header::CONTENT_TYPE], "application/pdf");
    let disposition = download.headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap();
    assert!(
        disposition.contains("bir_birlestirildi.pdf"),
        "{disposition}"
    );
    assert!(download.body.starts_with(b"%PDF-"));
}

/// A kind the API publishes but has no engine for is refused before any
/// work, workspace or job record exists - it never becomes a failed job.
#[tokio::test]
async fn kinds_without_an_engine_are_refused_before_any_work() {
    let server = TestServer::production();
    let mut c = server.client();
    let pdf_id = c.upload("rapor.pdf", pdf_bytes()).await.json()["file_id"]
        .as_str()
        .unwrap()
        .to_string();

    for body in [
        json!({ "kind": "pdf_merge", "file_ids": [pdf_id.clone(), pdf_id.clone()] }),
        json!({ "kind": "pdf_compress", "file_id": pdf_id.clone() }),
        json!({ "kind": "pdf_rotate", "file_id": pdf_id.clone(), "options": { "rotation": "90" } }),
        json!({ "kind": "pdf_ocr", "file_id": pdf_id.clone() }),
    ] {
        let r = c.post_json("/api/v1/jobs", body.clone()).await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "UNSUPPORTED_CONVERSION"
            ),
            "{body}"
        );
    }

    // An unknown kind is refused the same way, and neither case leaves a
    // job behind.
    let r = c
        .post_json(
            "/api/v1/jobs",
            json!({ "kind": "pdf_encrypt", "file_id": pdf_id }),
        )
        .await;
    assert_eq!(r.code(), "UNSUPPORTED_CONVERSION");
    assert!(
        server.job_dirs().is_empty(),
        "a refused kind left a workspace: {:?}",
        server.job_dirs()
    );

    // The image pipeline is in-process, so it is available in production.
    let jpeg_id = upload_jpeg(&mut c).await;
    let created = c.create_job(&jpeg_id, "png").await;
    assert_eq!(created.status, StatusCode::ACCEPTED);
    assert_eq!(
        c.wait_terminal(created.json()["job_id"].as_str().unwrap())
            .await["state"],
        "completed"
    );
}

/// PDFs and Office documents are admitted, and stored as the format their
/// CONTENT proves them to be. A short prefix cannot tell DOCX from XLSX -
/// both are ZIPs - so the probe is what decides, and a name that disagrees
/// with the parts inside is rejected.
#[tokio::test]
async fn documents_are_admitted_only_when_their_content_proves_their_type() {
    let server = TestServer::new();
    let mut c = server.client();

    let r = c.upload("rapor.pdf", pdf_bytes()).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let view = r.json();
    assert_eq!(view["detected_format"], "pdf");
    // A document has no single pixel size: the field is present and null
    // rather than a made-up zero.
    assert!(view["width"].is_null(), "{view}");
    assert!(view["height"].is_null(), "{view}");

    let r = c.upload("mektup.docx", docx_bytes()).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.json()["detected_format"], "docx");

    // The same ZIP container, but the parts inside say spreadsheet.
    let r = c.upload("mektup.docx", xlsx_bytes()).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "FILE_TYPE_MISMATCH")
    );

    // An OpenDocument file is identified by its `mimetype` member.
    let odt = zip_bytes(&[
        ("mimetype", b"application/vnd.oasis.opendocument.text"),
        ("content.xml", b"<document/>"),
    ]);
    assert_eq!(
        c.upload("belge.odt", odt).await.json()["detected_format"],
        "odt"
    );

    // A ZIP that is not an Office document at all.
    let r = c
        .upload("sahte.docx", zip_bytes(&[("notes.txt", b"just a zip")]))
        .await;
    assert_eq!(r.code(), "FILE_TYPE_MISMATCH");

    // A PDF truncated before its trailer. The streaming byte counter cannot
    // catch a client that declared exactly what it sent; the probe does.
    let mut truncated = pdf_bytes();
    truncated.truncate(20);
    let r = c.upload("yarim.pdf", truncated).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "FILE_CORRUPT")
    );

    // An image conversion cannot be asked of a document.
    let pdf_id = c.upload("ikinci.pdf", pdf_bytes()).await.json()["file_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = c.create_job(&pdf_id, "png").await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "UNSUPPORTED_CONVERSION")
    );
}

#[tokio::test]
async fn malformed_uploads_are_rejected_safely_and_leave_nothing_on_disk() {
    let server = TestServer::new();
    let mut c = server.client();

    let r = c.upload_raw(None, Some(4), b"abcd".to_vec()).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "MISSING_FILE_NAME")
    );

    let r = c.upload_raw(Some("a.jpg"), None, jpeg_bytes(8, 8)).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::LENGTH_REQUIRED, "LENGTH_REQUIRED")
    );

    // A real PDF under a .png name. The server admits PDFs, so this is a
    // recognized container contradicting its name - not an unknown type.
    let r = c
        .upload(
            "rapor.png",
            b"%PDF-1.7\n this is a pdf, not a png at all\n%%EOF".to_vec(),
        )
        .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "FILE_TYPE_MISMATCH")
    );

    // Content that is no container this server opens at all.
    let r = c
        .upload("notlar.png", b"just some plain text, no magic".to_vec())
        .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNSUPPORTED_MEDIA_TYPE, "UNSUPPORTED_FILE_TYPE")
    );

    // A legacy binary .doc (OLE container) renamed to .docx: not admitted,
    // and recognized as unsupported rather than mistaken for OOXML.
    let mut ole = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1".to_vec();
    ole.extend_from_slice(&[0u8; 64]);
    let r = c.upload("eski.docx", ole).await;
    assert_eq!(r.code(), "UNSUPPORTED_FILE_TYPE");

    // Real JPEG content under a .png name.
    let r = c.upload("disguised.png", jpeg_bytes(8, 8)).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "FILE_TYPE_MISMATCH")
    );

    // Unsupported extension.
    let r = c.upload("notes.exe", jpeg_bytes(8, 8)).await;
    assert_eq!(r.code(), "UNSUPPORTED_FILE_TYPE");

    // PNG magic followed by garbage: header unreadable.
    let mut bogus = png_bytes(4, 4)[..8].to_vec();
    bogus.extend_from_slice(&[0u8; 40]);
    let r = c.upload("broken.png", bogus).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "FILE_CORRUPT")
    );

    // Engine limit preserved at upload: animated GIF rejected.
    let r = c.upload("anim.gif", animated_gif()).await;
    assert_eq!(r.code(), "IMAGE_MULTI_FRAME_UNSUPPORTED");

    // Error bodies never leak server paths.
    let root = server.root.to_string_lossy().to_string();
    assert!(!String::from_utf8_lossy(&r.body).contains(&root));

    assert!(
        server.files_on_disk().is_empty(),
        "rejected uploads left files: {:?}",
        server.files_on_disk()
    );
}

// ── 3. server-generated ids ─────────────────────────────────────────────

#[tokio::test]
async fn ids_are_server_generated_and_client_ids_are_refused() {
    let server = TestServer::new();
    let mut c = server.client();

    let r1 = c.upload("a.jpg", jpeg_bytes(8, 8)).await;
    let r2 = c.upload("a.jpg", jpeg_bytes(8, 8)).await;
    let (id1, id2) = (
        r1.json()["file_id"].as_str().unwrap().to_string(),
        r2.json()["file_id"].as_str().unwrap().to_string(),
    );
    assert_ne!(id1, id2);
    for id in [&id1, &id2] {
        let parsed = uuid::Uuid::parse_str(id).unwrap();
        assert_eq!(parsed.get_version_num(), 4);
    }

    // A client trying to choose the job id is refused (unknown field).
    let r = c
        .post_json(
            "/api/v1/jobs",
            json!({ "kind": "convert", "file_id": id1, "output_format": "png", "job_id": "11111111-1111-4111-8111-111111111111" }),
        )
        .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "INVALID_REQUEST")
    );

    // Same for smuggled engine parameters.
    let r = c
        .post_json(
            "/api/v1/jobs",
            json!({ "kind": "convert", "file_id": id1, "output_format": "png", "options": { "output_path": "C:\\x.png" } }),
        )
        .await;
    assert_eq!(r.code(), "INVALID_REQUEST");

    let r = c.create_job(&id1, "png").await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let job_id = r.json()["job_id"].as_str().unwrap().to_string();
    assert_eq!(uuid::Uuid::parse_str(&job_id).unwrap().get_version_num(), 4);
    assert_ne!(job_id, id1);
}

// ── 4 + 5. not-found uniformity and ownership isolation ─────────────────

#[tokio::test]
async fn foreign_unknown_and_malformed_ids_are_indistinguishable() {
    let server = TestServer::new();
    let mut alice = server.client();
    let mut bob = server.client();

    let file_id = upload_jpeg(&mut alice).await;
    let job_id = alice.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.wait_terminal(&job_id).await;
    // Bob needs his own session first.
    bob.get("/api/v1/capabilities").await;
    assert_ne!(alice.cookie, bob.cookie);

    let unknown = uuid::Uuid::new_v4().to_string();
    let reference = bob.get(&format!("/api/v1/jobs/{unknown}")).await;
    assert_eq!(reference.status, StatusCode::NOT_FOUND);

    for id in [
        job_id.as_str(),
        unknown.as_str(),
        "not-a-uuid",
        "..%2F..%2Fsessions",
        &job_id.to_uppercase(),
    ] {
        for path in [
            format!("/api/v1/jobs/{id}"),
            format!("/api/v1/jobs/{id}/download"),
            format!("/api/v1/jobs/{id}/events"),
        ] {
            let r = bob.get(&path).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(
                r.body, reference.body,
                "{path} response differs from unknown-id response"
            );
        }
        let r = bob
            .post_json(&format!("/api/v1/jobs/{id}/cancel"), json!({}))
            .await;
        assert_eq!(
            (r.status, r.body.clone()),
            (StatusCode::NOT_FOUND, reference.body.clone())
        );
        let r = bob.delete(&format!("/api/v1/jobs/{id}")).await;
        assert_eq!(
            (r.status, r.body.clone()),
            (StatusCode::NOT_FOUND, reference.body.clone())
        );
    }

    // Bob cannot read Alice's file or build a job on it.
    let r = bob.get(&format!("/api/v1/files/{file_id}")).await;
    assert_eq!(
        (r.status, r.body.clone()),
        (StatusCode::NOT_FOUND, reference.body.clone())
    );
    let r = bob.create_job(&file_id, "webp").await;
    assert_eq!(
        (r.status, r.body.clone()),
        (StatusCode::NOT_FOUND, reference.body.clone())
    );

    // ...and none of that disturbed Alice's job.
    let r = alice.get(&format!("/api/v1/jobs/{job_id}")).await;
    assert_eq!(r.json()["state"], "completed");
    let r = alice.get(&format!("/api/v1/jobs/{job_id}/download")).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn a_forged_or_missing_session_cookie_gets_a_fresh_empty_session() {
    let server = TestServer::new();
    let mut alice = server.client();
    let file_id = upload_jpeg(&mut alice).await;

    let mut mallory = server.client();
    mallory.cookie = Some(format!("meb_sid={}", "a".repeat(64)));
    let r = mallory.get(&format!("/api/v1/files/{file_id}")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_ne!(
        mallory.cookie.as_deref(),
        Some(&*format!("meb_sid={}", "a".repeat(64))),
        "forged token must be replaced"
    );
}

#[tokio::test]
async fn state_changing_requests_without_the_csrf_header_are_refused() {
    let server = TestServer::new();
    let mut c = server.client();
    let req = Request::post("/api/v1/files")
        .header("x-file-name", "a.jpg")
        .header(header::CONTENT_LENGTH, "4")
        .body(Body::from("abcd"))
        .unwrap();
    let r = c.call(req).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "CSRF_REJECTED")
    );
    assert!(server.files_on_disk().is_empty());
}

// ── 6. path traversal ───────────────────────────────────────────────────

#[tokio::test]
async fn path_traversal_attempts_never_reach_the_filesystem() {
    let server = TestServer::new();
    let mut c = server.client();
    let outside = server.root.parent().unwrap().join("evil.jpg");
    let _ = std::fs::remove_file(&outside);

    for name in [
        "../../evil.jpg",
        "..\\..\\evil.jpg",
        "/etc/evil.jpg",
        "C:\\Windows\\evil.jpg",
        "sub/../../evil.jpg",
    ] {
        let r = c.upload(name, jpeg_bytes(8, 8)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{name}");
        assert_eq!(r.json()["display_name"], "evil.jpg", "{name}");
    }
    assert!(!outside.exists(), "a file escaped the data root");
    // Every stored file is a fixed-name blob under the data root.
    for f in server.files_on_disk() {
        assert!(f.starts_with(&server.root));
        assert_eq!(f.file_name().unwrap(), "blob");
    }

    // Traversal in the URL id is just "not found".
    for uri in [
        "/api/v1/jobs/..%2F..%2F..%2Fstaging",
        "/api/v1/files/..%5C..%5Cblob",
        "/api/v1/jobs/%2e%2e",
    ] {
        assert_eq!(c.get(uri).await.status, StatusCode::NOT_FOUND, "{uri}");
    }

    // The download name is header-safe and contains no path separators.
    let file_id = c
        .upload("../../a\"b\r\nc.jpg", jpeg_bytes(8, 8))
        .await
        .json()["file_id"]
        .as_str()
        .unwrap()
        .to_string();
    let job_id = c.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.wait_terminal(&job_id).await;
    let r = c.get(&format!("/api/v1/jobs/{job_id}/download")).await;
    let cd = r.headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .to_string();
    assert!(cd.starts_with("attachment; filename=\"a_bc.png\""), "{cd}");
    assert!(!cd.contains('/') && !cd.contains('\\'), "{cd}");
}

// ── 7. successful conversion ────────────────────────────────────────────

#[tokio::test]
async fn jpeg_to_png_conversion_end_to_end() {
    let server = TestServer::new();
    let mut c = server.client();

    // Capabilities are derived from the engines actually wired in, so this
    // is asked of a production-wired server: the test harness gives every
    // kind the same injected runner, which would make them all look
    // available.
    {
        let production = TestServer::production();
        let mut pc = production.client();
        let caps = pc.get("/api/v1/capabilities").await.json();
        let state_of = |id: &str| {
            caps["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["id"] == id)
                .unwrap_or_else(|| panic!("{id} is not reported"))["state"]
                .clone()
        };
        assert_eq!(state_of("image_conversion"), "AVAILABLE");
        assert_eq!(state_of("image_resize"), "AVAILABLE");

        // The in-process image pipeline needs no external tool, so it is
        // the one kind that is always executable.
        let kinds: Vec<&str> = caps["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"convert"));

        // Every other capability depends on an external engine being
        // installed on THIS machine, so what is asserted is the derivation
        // itself: a capability is AVAILABLE exactly when every job kind it
        // needs is posted as executable. (Hardcoding "not implemented"
        // here would instead assert a fact about the dev machine.)
        let published: &[(&str, &[&str])] = &[
            ("office_to_pdf", &["office_convert"]),
            ("pdf_to_office", &["pdf_to_office"]),
            ("ocr", &["pdf_ocr"]),
            (
                "pdf_structural_ops",
                &[
                    "pdf_merge",
                    "pdf_split",
                    "pdf_compress",
                    "pdf_rotate",
                    "pdf_watermark",
                ],
            ),
        ];
        for (id, needed) in published {
            let wired = needed.iter().all(|k| kinds.contains(k));
            let expected = if wired { "AVAILABLE" } else { "NOT_IMPLEMENTED" };
            assert_eq!(state_of(id), expected, "{id} (kinds: {kinds:?})");
        }
        // Watermark and OCR have no engine at all yet, so nothing can wire
        // them and `pdf_structural_ops` cannot be available either.
        assert!(!kinds.contains(&"pdf_watermark"));
        assert!(!kinds.contains(&"pdf_ocr"));
        assert_eq!(state_of("ocr"), "NOT_IMPLEMENTED");
        assert_eq!(state_of("pdf_structural_ops"), "NOT_IMPLEMENTED");

        // Advertised formats follow the same rule.
        let office_outputs = caps["formats"]["office"]["outputs"].as_array().unwrap();
        assert_eq!(
            !office_outputs.is_empty(),
            kinds.contains(&"office_convert"),
            "office outputs disagree with the wired kinds"
        );
        assert_eq!(
            !caps["formats"]["office"]["conversions"]
                .as_object()
                .unwrap()
                .is_empty(),
            kinds.contains(&"office_convert")
        );

        // An experimental operation is only published once it is wired, and
        // reconstruction is the only one.
        let experimental: Vec<&str> = caps["experimental_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        assert!(experimental.iter().all(|k| kinds.contains(k)));
        assert_eq!(
            experimental.contains(&"pdf_to_office"),
            kinds.contains(&"pdf_to_office")
        );
        assert!(!experimental.contains(&"convert"));
    }

    let up = c.upload("Öğrenci Fotoğrafı.jpg", jpeg_bytes(40, 30)).await;
    assert_eq!(up.status, StatusCode::CREATED);
    let view = up.json();
    assert_eq!(view["detected_format"], "jpg");
    assert_eq!(
        (view["width"].as_u64(), view["height"].as_u64()),
        (Some(40), Some(30))
    );

    let created = c.create_job(view["file_id"].as_str().unwrap(), "png").await;
    assert_eq!(created.status, StatusCode::ACCEPTED);
    let job_id = created.json()["job_id"].as_str().unwrap().to_string();

    let done = c.wait_terminal(&job_id).await;
    assert_eq!(done["state"], "completed", "{done}");
    assert_eq!(done["progress_pct"], 100);
    assert_eq!(done["result"]["output_name"], "Öğrenci Fotoğrafı.png");
    assert_eq!(done["result"]["content_type"], "image/png");

    let r = c.get(&format!("/api/v1/jobs/{job_id}/download")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(r.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(r.headers[header::CACHE_CONTROL], "no-store");
    assert!(r.headers[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .contains("sandbox"));
    assert!(r.headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .starts_with("attachment;"));
    let png =
        image::load_from_memory_with_format(&r.body, image::ImageFormat::Png).expect("valid PNG");
    assert_eq!((png.width(), png.height()), (40, 30));
    assert_eq!(done["result"]["size"].as_u64(), Some(r.body.len() as u64));

    // Only the published output remains in the job workspace.
    let dirs = server.job_dirs();
    assert_eq!(dirs.len(), 1);
    assert!(!dirs[0].join("in").exists() && !dirs[0].join("work").exists());
    assert!(dirs[0].join("out").join("result.png").is_file());
}

#[tokio::test]
async fn resize_option_is_applied_and_invalid_options_are_refused() {
    let server = TestServer::new();
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;

    let r = c
        .post_json("/api/v1/jobs", json!({ "kind": "convert", "file_id": file_id, "output_format": "webp", "options": { "width": 16, "height": 12 } }))
        .await;
    let job_id = r.json()["job_id"].as_str().unwrap().to_string();
    assert_eq!(c.wait_terminal(&job_id).await["state"], "completed");
    let out = c.get(&format!("/api/v1/jobs/{job_id}/download")).await.body;
    let img = image::load_from_memory(&out).unwrap();
    assert_eq!((img.width(), img.height()), (16, 12));

    for options in [
        json!({ "width": 16 }),
        json!({ "width": 20001, "height": 1 }),
        json!({ "width": 9000, "height": 9000 }),
    ] {
        let r = c.post_json("/api/v1/jobs", json!({ "kind": "convert", "file_id": file_id, "output_format": "png", "options": options })).await;
        assert_eq!(r.code(), "INVALID_DIMENSIONS", "{options}");
    }
    let r = c.create_job(&file_id, "docx").await;
    assert_eq!(r.code(), "UNSUPPORTED_CONVERSION");
}

// ── 8 + 9. failed job and its cleanup ───────────────────────────────────

#[tokio::test]
async fn a_corrupt_image_fails_its_job_and_leaves_no_workspace() {
    let server = TestServer::new();
    let mut c = server.client();

    let up = c.upload("yarim.png", truncated_png()).await;
    assert_eq!(
        up.status,
        StatusCode::CREATED,
        "header-valid file passes the upload probe"
    );
    let job_id = c
        .create_job(up.json()["file_id"].as_str().unwrap(), "jpg")
        .await
        .json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let done = c.wait_terminal(&job_id).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert_eq!(done["error"]["code"], "IMAGE_DECODE_FAILED");
    assert_eq!(done["error"]["message"], "Görsel dosyası okunamadı.");
    assert!(done["result"].is_null());

    assert!(
        server.job_dirs().is_empty(),
        "failed job workspace not cleaned: {:?}",
        server.job_dirs()
    );
    let r = c.get(&format!("/api/v1/jobs/{job_id}/download")).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "NOT_READY")
    );
}

#[tokio::test]
async fn an_engine_panic_fails_only_that_job_and_the_server_keeps_working() {
    let server = TestServer::with(|_| {}, Arc::new(PanickingRunner));
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;

    let r = c
        .post_json("/api/v1/jobs", json!({ "kind": "convert", "file_id": file_id, "output_format": "png", "options": { "quality": 13 } }))
        .await;
    let bad = r.json()["job_id"].as_str().unwrap().to_string();
    let done = c.wait_terminal(&bad).await;
    assert_eq!(
        (done["state"].as_str(), done["error"]["code"].as_str()),
        (Some("failed"), Some("INTERNAL_ERROR"))
    );
    assert!(
        !done.to_string().contains("simulated engine bug"),
        "panic payload leaked"
    );
    assert!(server.job_dirs().is_empty());

    let good = c.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(c.wait_terminal(&good).await["state"], "completed");
}

// ── cancellation and deletion ───────────────────────────────────────────

#[tokio::test]
async fn cancelling_a_running_job_stops_it_and_removes_its_workspace() {
    let (runner, _gate) = BlockingRunner::new();
    let server = TestServer::with(|_| {}, runner);
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;
    let job_id = c.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.wait_state(&job_id, "running").await;
    assert_eq!(server.job_dirs().len(), 1);

    let r = c
        .post_json(&format!("/api/v1/jobs/{job_id}/cancel"), json!({}))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert!(matches!(
        r.json()["state"].as_str(),
        Some("cancel_requested" | "cancelled")
    ));

    let done = c.wait_terminal(&job_id).await;
    assert_eq!(done["state"], "cancelled");
    assert!(
        server.job_dirs().is_empty(),
        "cancelled job workspace not cleaned"
    );

    // Cancel is idempotent on a terminal job.
    let r = c
        .post_json(&format!("/api/v1/jobs/{job_id}/cancel"), json!({}))
        .await;
    assert_eq!(
        (r.status, r.json()["state"].as_str()),
        (StatusCode::OK, Some("cancelled"))
    );
}

#[tokio::test]
async fn a_queued_job_can_be_cancelled_before_it_starts() {
    let (runner, gate) = BlockingRunner::new();
    let server = TestServer::with(
        |c| {
            c.worker_permits = 1;
            c.max_running_per_session = 1;
        },
        runner,
    );
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;
    let first = c.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.wait_state(&first, "running").await;
    let second = c.create_job(&file_id, "webp").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        c.get(&format!("/api/v1/jobs/{second}")).await.json()["state"],
        "queued"
    );

    let r = c
        .post_json(&format!("/api/v1/jobs/{second}/cancel"), json!({}))
        .await;
    assert_eq!(r.json()["state"], "cancelled");

    release(&gate);
    assert_eq!(c.wait_terminal(&first).await["state"], "completed");
    assert_eq!(c.wait_terminal(&second).await["state"], "cancelled");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        server.job_dirs().len(),
        1,
        "only the completed job keeps a workspace"
    );
}

#[tokio::test]
async fn deleting_a_job_forgets_it_and_removes_its_files() {
    let server = TestServer::new();
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;
    let job_id = c.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.wait_terminal(&job_id).await;
    assert_eq!(server.job_dirs().len(), 1);

    let r = c.delete(&format!("/api/v1/jobs/{job_id}")).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(server.job_dirs().is_empty());
    assert_eq!(
        c.get(&format!("/api/v1/jobs/{job_id}")).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.get(&format!("/api/v1/jobs/{job_id}/download"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn deleting_a_running_job_cleans_up_when_the_engine_returns() {
    let (runner, gate) = BlockingRunner::new();
    let server = TestServer::with(|_| {}, runner);
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;
    let job_id = c.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.wait_state(&job_id, "running").await;

    assert_eq!(
        c.delete(&format!("/api/v1/jobs/{job_id}")).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        c.get(&format!("/api/v1/jobs/{job_id}")).await.status,
        StatusCode::NOT_FOUND
    );
    release(&gate);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !server.job_dirs().is_empty() {
        assert!(
            Instant::now() < deadline,
            "deleted job's workspace never removed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── 10. download authorization ──────────────────────────────────────────

#[tokio::test]
async fn download_requires_ownership_and_completion() {
    let (runner, gate) = BlockingRunner::new();
    let server = TestServer::with(|_| {}, runner);
    let mut alice = server.client();
    let mut bob = server.client();
    let file_id = upload_jpeg(&mut alice).await;
    let job_id = alice.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.wait_state(&job_id, "running").await;

    let r = alice.get(&format!("/api/v1/jobs/{job_id}/download")).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "NOT_READY")
    );

    release(&gate);
    alice.wait_terminal(&job_id).await;
    assert_eq!(
        alice
            .get(&format!("/api/v1/jobs/{job_id}/download"))
            .await
            .status,
        StatusCode::OK
    );
    let r = bob.get(&format!("/api/v1/jobs/{job_id}/download")).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "NOT_FOUND")
    );
}

// ── 11. SSE / current state ─────────────────────────────────────────────

async fn next_sse_event(body: &mut Body) -> Option<String> {
    let mut buf = String::new();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), body.frame())
            .await
            .expect("SSE stalled")?;
        if let Ok(data) = frame.unwrap().into_data() {
            buf.push_str(&String::from_utf8_lossy(&data));
            if buf.contains("\n\n") && buf.contains("data:") {
                return Some(buf);
            }
        }
    }
}

fn sse_json(event: &str) -> Value {
    let data = event
        .lines()
        .find_map(|l| l.strip_prefix("data:"))
        .unwrap()
        .trim();
    serde_json::from_str(data).unwrap()
}

#[tokio::test]
async fn events_start_with_the_current_snapshot_and_end_after_the_terminal_one() {
    let (runner, gate) = BlockingRunner::new();
    let server = TestServer::with(|_| {}, runner);
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;
    let job_id = c.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.wait_state(&job_id, "running").await;

    // A client connecting mid-job immediately gets the current state.
    let resp = c
        .send(
            Request::get(format!("/api/v1/jobs/{job_id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let mut body = resp.into_body();
    let first = next_sse_event(&mut body).await.unwrap();
    assert!(first.contains("event: job"), "{first}");
    assert_eq!(sse_json(&first)["state"], "running");

    release(&gate);
    let mut last = sse_json(&first);
    while let Some(ev) = next_sse_event(&mut body).await {
        let snap = sse_json(&ev);
        assert!(
            snap["seq"].as_u64() > last["seq"].as_u64(),
            "snapshots out of order"
        );
        last = snap;
        if last["state"] == "completed" {
            break;
        }
    }
    assert_eq!(last["state"], "completed");
    // Stream closes after the terminal snapshot.
    let tail = tokio::time::timeout(Duration::from_secs(5), body.frame())
        .await
        .expect("stream did not end");
    assert!(tail.is_none());

    // Reconnecting after completion: one terminal snapshot, then the end.
    let resp = c
        .send(
            Request::get(format!("/api/v1/jobs/{job_id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert_eq!(text.matches("event: job").count(), 1, "{text}");
    assert_eq!(sse_json(&text)["state"], "completed");
}

// ── limits and expiry ───────────────────────────────────────────────────

#[tokio::test]
async fn per_session_active_job_limit_is_enforced() {
    let (runner, gate) = BlockingRunner::new();
    let server = TestServer::with(|c| c.max_active_jobs_per_session = 2, runner);
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;
    assert_eq!(
        c.create_job(&file_id, "png").await.status,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        c.create_job(&file_id, "png").await.status,
        StatusCode::ACCEPTED
    );
    let r = c.create_job(&file_id, "png").await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "TOO_MANY_JOBS")
    );
    // Another session is unaffected by this session's limit.
    let mut other = server.client();
    let other_file = upload_jpeg(&mut other).await;
    assert_eq!(
        other.create_job(&other_file, "png").await.status,
        StatusCode::ACCEPTED
    );
    release(&gate);
}

#[tokio::test]
async fn session_quota_is_enforced() {
    let server = TestServer::with(
        |c| c.session_quota_bytes = 2_000,
        Arc::new(NativeImageRunner),
    );
    let mut c = server.client();
    let small = jpeg_bytes(8, 8);
    let mut accepted = 0;
    loop {
        let r = c.upload("a.jpg", small.clone()).await;
        if r.status != StatusCode::CREATED {
            assert_eq!(r.code(), "SESSION_QUOTA_EXCEEDED");
            break;
        }
        accepted += 1;
        assert!(accepted < 100);
    }
    assert!(accepted >= 1);
}

// ── concurrent upload admission ─────────────────────────────────────────

/// Establishes the session cookie without spending any quota, so the
/// clones below all upload as ONE browser.
async fn start_session(client: &mut Client) {
    assert_eq!(
        client.get("/api/v1/capabilities").await.status,
        StatusCode::OK
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_uploads_cannot_exceed_the_session_byte_quota() {
    let payload = jpeg_bytes(64, 64);
    let one = payload.len() as u64;
    // Room for exactly three of these uploads, and a bit of slack that is
    // nowhere near enough for a fourth.
    let quota = one * 3 + one / 2;
    let server = TestServer::with(
        |c| c.session_quota_bytes = quota,
        Arc::new(NativeImageRunner),
    );
    let mut c = server.client();
    start_session(&mut c).await;

    let mut handles = Vec::new();
    for _ in 0..8 {
        let mut client = c.clone();
        let bytes = payload.clone();
        handles.push(tokio::spawn(async move {
            client.upload("foto.jpg", bytes).await
        }));
    }

    let mut created = 0u64;
    for h in handles {
        let r = h.await.unwrap();
        if r.status == StatusCode::CREATED {
            created += 1;
        } else {
            assert_eq!(
                (r.status, r.code().as_str()),
                (StatusCode::TOO_MANY_REQUESTS, "SESSION_QUOTA_EXCEEDED")
            );
        }
    }
    assert_eq!(created, 3, "quota admission is not atomic");
    assert!(created * one <= quota, "stored bytes exceeded the quota");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_uploads_cannot_exceed_the_session_file_count() {
    let server = TestServer::with(|c| c.max_files_per_session = 2, Arc::new(NativeImageRunner));
    let mut c = server.client();
    start_session(&mut c).await;

    let mut handles = Vec::new();
    for _ in 0..8 {
        let mut client = c.clone();
        let bytes = jpeg_bytes(16, 16);
        handles.push(tokio::spawn(async move {
            client.upload("foto.jpg", bytes).await.status
        }));
    }
    let mut created = 0;
    for h in handles {
        if h.await.unwrap() == StatusCode::CREATED {
            created += 1;
        }
    }
    assert_eq!(created, 2, "file-count admission is not atomic");
}

// ── stalled uploads: idle timeout and in-flight slots ───────────────────

#[tokio::test]
async fn a_stalled_upload_times_out_and_leaves_no_partial_file() {
    let server = TestServer::with(
        |c| c.upload_idle_timeout = Duration::from_millis(150),
        Arc::new(NativeImageRunner),
    );
    let mut c = server.client();

    let head = jpeg_bytes(32, 24);
    let declared = head.len() as u64 + 4096;
    let started = Instant::now();
    let r = c.upload_stalling("yavas.jpg", head, declared).await;

    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::REQUEST_TIMEOUT, "UPLOAD_TIMEOUT")
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the stalled upload was not bounded by the idle timeout"
    );
    assert!(
        server.files_on_disk().is_empty(),
        "timed-out upload left a partial file: {:?}",
        server.files_on_disk()
    );
    // The error says nothing about paths, OS errors or internals.
    let body = String::from_utf8_lossy(&r.body);
    assert!(
        !body.contains("staging") && !body.contains(".part"),
        "{body}"
    );

    // Its quota reservation was released: a normal upload still works.
    assert_eq!(
        c.upload("foto.jpg", jpeg_bytes(16, 16)).await.status,
        StatusCode::CREATED
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_uploads_are_capped_and_the_slot_is_released() {
    let server = TestServer::with(
        |c| {
            c.max_concurrent_uploads = 1;
            c.upload_idle_timeout = Duration::from_secs(5);
        },
        Arc::new(NativeImageRunner),
    );
    let mut c = server.client();
    start_session(&mut c).await;

    let mut staller = c.clone();
    let head = jpeg_bytes(32, 24);
    let declared = head.len() as u64 + 4096;
    let stalled =
        tokio::spawn(async move { staller.upload_stalling("asili.jpg", head, declared).await });

    // Wait until the stalled upload actually holds the only slot, so the
    // probe below measures the limit and not a lost race for it.
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.app.state.uploads.available_permits() > 0 {
        assert!(
            Instant::now() < deadline,
            "the stalled upload never took its slot"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // With the only slot held, a second upload is refused outright instead
    // of queueing behind the stalled one.
    let r = c.upload("foto.jpg", jpeg_bytes(16, 16)).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVER_BUSY")
    );

    // Once the stalled upload times out, its slot comes back.
    assert_eq!(stalled.await.unwrap().status, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(
        c.upload("foto.jpg", jpeg_bytes(16, 16)).await.status,
        StatusCode::CREATED
    );
}

// ── queued cancellation and deleted-job accounting ──────────────────────

#[tokio::test]
async fn cancelling_a_queued_job_cleans_it_up_without_waiting_for_a_worker() {
    let (runner, gate) = BlockingRunner::new();
    let server = TestServer::with(
        |c| {
            c.worker_permits = 1;
            c.max_running_per_session = 1;
        },
        runner,
    );
    let _gate_guard = GateGuard(gate.clone());
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;

    let running = c.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.wait_state(&running, "running").await;
    let queued = c.create_job(&file_id, "webp").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        c.get(&format!("/api/v1/jobs/{queued}")).await.json()["state"],
        "queued"
    );
    assert_eq!(server.job_dirs().len(), 2);

    assert_eq!(
        c.post_json(&format!("/api/v1/jobs/{queued}/cancel"), json!({}))
            .await
            .json()["state"],
        "cancelled"
    );

    // The one worker is still busy with `running`, so this only passes if
    // the cancelled job stopped waiting for a permit it no longer needs.
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.job_dirs().len() > 1 {
        assert!(
            Instant::now() < deadline,
            "cancelled queued job waited for a worker permit before cleanup"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        c.get(&format!("/api/v1/jobs/{running}")).await.json()["state"],
        "running",
        "cleanup must happen while the other job is still blocked"
    );

    release(&gate);
    assert_eq!(c.wait_terminal(&running).await["state"], "completed");
}

#[tokio::test]
async fn a_deleted_running_job_keeps_counting_until_its_worker_exits() {
    let (runner, gate) = BlockingRunner::new();
    let server = TestServer::with(|c| c.max_active_jobs_per_session = 1, runner);
    let _gate_guard = GateGuard(gate.clone());
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;

    let job_id = c.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.wait_state(&job_id, "running").await;

    assert_eq!(
        c.delete(&format!("/api/v1/jobs/{job_id}")).await.status,
        StatusCode::NO_CONTENT
    );
    // Gone for its owner...
    assert_eq!(
        c.get(&format!("/api/v1/jobs/{job_id}")).await.status,
        StatusCode::NOT_FOUND
    );
    // ...but still executing, so it still occupies the session's slot:
    // deleting must not buy more concurrent work than the limit allows.
    let r = c.create_job(&file_id, "png").await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "TOO_MANY_JOBS"),
        "a deleted job stopped counting while its worker was still running"
    );

    release(&gate);

    // Once the worker really exits, the slot is free again.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if c.create_job(&file_id, "png").await.status == StatusCode::ACCEPTED {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the deleted job's slot was never released"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn janitor_expires_files_and_finished_jobs_from_disk_and_registry() {
    let server = TestServer::with(
        |c| {
            c.upload_ttl = Duration::ZERO;
            c.job_ttl = Duration::ZERO;
        },
        Arc::new(NativeImageRunner),
    );
    let mut c = server.client();
    let file_id = upload_jpeg(&mut c).await;
    let job_id = c.create_job(&file_id, "png").await.json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.wait_terminal(&job_id).await;
    assert!(!server.files_on_disk().is_empty());

    server.app.sweep();

    assert!(
        server.files_on_disk().is_empty(),
        "expired data still on disk: {:?}",
        server.files_on_disk()
    );
    assert_eq!(
        c.get(&format!("/api/v1/jobs/{job_id}")).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.get(&format!("/api/v1/files/{file_id}")).await.status,
        StatusCode::NOT_FOUND
    );
}
