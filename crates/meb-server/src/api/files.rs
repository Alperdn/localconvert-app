//! `POST /api/v1/files` (streamed upload) and `GET /api/v1/files/:id`.
//!
//! Upload protocol: the request body is the raw file bytes; the original
//! name travels percent-encoded in `X-File-Name` (display only);
//! `Content-Length` is required. The client's `Content-Type` is ignored.
//!
//! Order of checks: declared size vs cap -> an in-flight upload slot
//! (`max_concurrent_uploads`) -> the session's quota and file count,
//! RESERVED atomically for this upload so concurrent uploads cannot
//! overshoot the limits together -> stream to `staging/<uuid>.part` with a
//! byte counter (abort over cap) and an idle timeout (abort a stalled
//! body) -> magic-byte sniff of the first bytes (abort early on
//! non-images) -> extension must agree with the sniffed format ->
//! structural probe by the engine (dimension/bomb limits, multi-frame) ->
//! move into the session's file directory under a fixed name. Any failure
//! deletes the partial file and releases the reservation and the slot.

use crate::error::ApiError;
use crate::ids::FileId;
use crate::names::{extension_of, sanitize_display_name};
use crate::registry::{FileRecord, FileView};
use crate::session::Owner;
use crate::AppState;
use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::header::CONTENT_LENGTH;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use futures_util::StreamExt;
use meb_core::image::{self, NativeImageFormat};
use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

const FILE_NAME_HEADER: &str = "x-file-name";
/// Bytes needed before the content sniff runs (WebP needs 12).
const SNIFF_BYTES: usize = 16;

/// Deletes the staged part file unless disarmed.
struct PartGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for PartGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn display_name_from(headers: &HeaderMap) -> Result<String, ApiError> {
    let raw = headers
        .get(FILE_NAME_HEADER)
        .ok_or_else(ApiError::missing_file_name)?;
    let raw = raw.to_str().map_err(|_| ApiError::missing_file_name())?;
    let decoded = percent_decode_str(raw)
        .decode_utf8()
        .map_err(|_| ApiError::missing_file_name())?;
    sanitize_display_name(&decoded).ok_or_else(ApiError::missing_file_name)
}

fn declared_length(headers: &HeaderMap) -> Result<u64, ApiError> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(ApiError::length_required)
}

pub async fn upload(
    State(state): State<AppState>,
    owner: Owner,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<FileView>), ApiError> {
    let display_name = display_name_from(&headers)?;
    let declared = declared_length(&headers)?;
    let cfg = &state.config;
    if declared == 0 {
        return Err(ApiError::empty_file());
    }
    if declared > cfg.max_upload_bytes {
        return Err(ApiError::upload_too_large());
    }

    // One of a bounded number of upload slots. Held for this request only,
    // so a stalled body occupies a slot no longer than its idle timeout.
    let _slot = state
        .uploads
        .try_acquire()
        .map_err(|_| ApiError::server_busy())?;

    // The claimed type comes from the (sanitized) name; it is only accepted
    // if the content later sniffs as the same format.
    let claimed = extension_of(&display_name)
        .and_then(|ext| NativeImageFormat::from_extension(&ext))
        .ok_or_else(ApiError::unsupported_file_type)?;

    // Admission: the session's share of quota and file count is taken here,
    // under the registry lock, and held until this upload is stored or
    // fails. Two uploads that only fit one between them cannot both pass.
    let reservation = state.registry.reserve_upload(
        &owner,
        declared,
        cfg.max_files_per_session,
        cfg.session_quota_bytes,
    )?;

    let part_path = state.storage.staging_file();
    let mut guard = PartGuard {
        path: part_path.clone(),
        armed: true,
    };
    let sniffed = stream_to_part(
        &part_path,
        body,
        declared,
        cfg.max_upload_bytes,
        cfg.upload_idle_timeout,
    )
    .await?;
    if sniffed != claimed {
        return Err(ApiError::file_type_mismatch());
    }

    let probe_path = part_path.clone();
    let probe = tokio::task::spawn_blocking(move || image::probe_file(&probe_path, sniffed))
        .await
        .map_err(|e| ApiError::internal("upload probe", e))?
        .map_err(|e| {
            tracing::info!(code = e.code(), "upload rejected by probe");
            ApiError::from_image_probe(e.code())
        })?;

    let file_id = FileId::generate();
    let dir = state.storage.file_dir(&owner, file_id);
    let blob = state.storage.file_blob(&owner, file_id);
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal("upload: create file dir", e))?;
    if let Err(e) = std::fs::rename(&part_path, &blob) {
        state.remove_tree_or_defer(&dir);
        return Err(ApiError::internal("upload: publish blob", e));
    }
    guard.armed = false;

    let record = FileRecord::new(
        file_id,
        owner,
        display_name,
        sniffed,
        declared,
        probe.width,
        probe.height,
    );
    let record = state.registry.insert_file(record, reservation)?;
    tracing::info!(file_id = %file_id, format = sniffed.canonical_extension(), size = declared, "upload accepted");
    Ok((StatusCode::CREATED, Json(record.view())))
}

/// Streams the body into `path` (created fresh, never overwriting), aborting
/// as soon as the byte count exceeds the declared length or the cap, the
/// first bytes are not an image this server handles, or the client stops
/// sending for longer than `idle_timeout`. Returns the sniffed format.
async fn stream_to_part(
    path: &Path,
    body: Body,
    declared: u64,
    cap: u64,
    idle_timeout: std::time::Duration,
) -> Result<NativeImageFormat, ApiError> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|e| ApiError::internal("upload: create part", e))?;

    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;
    let mut head: Vec<u8> = Vec::with_capacity(SNIFF_BYTES);
    let mut sniffed: Option<NativeImageFormat> = None;

    // The timeout is per chunk, so a slow-but-progressing upload is never
    // cut off, while one that stops sending is abandoned (and its partial
    // file deleted by `PartGuard`) instead of holding its slot forever.
    while let Some(chunk) = tokio::time::timeout(idle_timeout, stream.next())
        .await
        .map_err(|_| {
            tracing::info!("upload abandoned: idle timeout");
            ApiError::upload_timeout()
        })?
    {
        let chunk = chunk.map_err(|_| ApiError::upload_incomplete())?;
        written += chunk.len() as u64;
        if written > cap {
            return Err(ApiError::upload_too_large());
        }
        if written > declared {
            return Err(ApiError::upload_incomplete());
        }
        if sniffed.is_none() && head.len() < SNIFF_BYTES {
            let take = (SNIFF_BYTES - head.len()).min(chunk.len());
            head.extend_from_slice(&chunk[..take]);
            if head.len() >= SNIFF_BYTES {
                sniffed =
                    Some(image::sniff_format(&head).ok_or_else(ApiError::unsupported_file_type)?);
            }
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| ApiError::internal("upload: write part", e))?;
    }
    file.flush()
        .await
        .map_err(|e| ApiError::internal("upload: flush part", e))?;
    drop(file);

    if written != declared {
        return Err(ApiError::upload_incomplete());
    }
    match sniffed {
        Some(f) => Ok(f),
        None => image::sniff_format(&head).ok_or_else(ApiError::unsupported_file_type),
    }
}

pub async fn get_file(
    State(state): State<AppState>,
    owner: Owner,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<FileView>, ApiError> {
    let record = state.registry.find_file(&owner, &id)?;
    Ok(Json(record.view()))
}
