//! Structured API errors.
//!
//! Every error the browser sees is `{ "error": { "code", "message" } }` with
//! a stable SCREAMING_SNAKE code (translated by the frontend's existing
//! `errorCodes` locale map) and a Turkish message. Filesystem paths, OS
//! error text and panic payloads never reach a response: internal failures
//! are logged with an incident id and only that id is returned.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: &'static str,
    pub incident: Option<String>,
}

impl ApiError {
    pub const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        ApiError {
            status,
            code,
            message,
            incident: None,
        }
    }

    /// The single response for unknown, malformed, expired AND foreign ids.
    /// Identical bytes in every case, so a response never reveals whether
    /// some other session's resource exists.
    pub const fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "İstenen kayıt bulunamadı.",
        )
    }

    /// Logs `detail` server-side with a fresh incident id; the client only
    /// gets the id and a generic message.
    pub fn internal(context: &str, detail: impl std::fmt::Display) -> Self {
        let incident = uuid::Uuid::new_v4().simple().to_string();
        tracing::error!(incident = %incident, context, detail = %detail, "internal error");
        ApiError {
            incident: Some(incident),
            ..Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Beklenmeyen bir sunucu hatası oluştu.",
            )
        }
    }

    pub const fn csrf_rejected() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "CSRF_REJECTED",
            "İstek doğrulanamadı. Sayfayı yenileyip yeniden deneyin.",
        )
    }
    pub const fn invalid_request() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "İstek geçersiz.",
        )
    }
    pub const fn missing_file_name() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "MISSING_FILE_NAME",
            "Dosya adı eksik veya geçersiz.",
        )
    }
    pub const fn length_required() -> Self {
        Self::new(
            StatusCode::LENGTH_REQUIRED,
            "LENGTH_REQUIRED",
            "Dosya boyutu bildirilmedi.",
        )
    }
    pub const fn upload_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "UPLOAD_TOO_LARGE",
            "Dosya izin verilen en büyük boyutu aşıyor.",
        )
    }
    pub const fn upload_incomplete() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "UPLOAD_INCOMPLETE",
            "Dosya yüklemesi tamamlanamadı.",
        )
    }
    /// The upload body stalled for longer than `upload_idle_timeout`. The
    /// partial file has already been deleted; the client may retry.
    pub const fn upload_timeout() -> Self {
        Self::new(
            StatusCode::REQUEST_TIMEOUT,
            "UPLOAD_TIMEOUT",
            "Dosya yüklemesi zaman aşımına uğradı. Lütfen yeniden deneyin.",
        )
    }
    pub const fn empty_file() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "EMPTY_FILE", "Dosya boş.")
    }
    pub const fn unsupported_file_type() -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "UNSUPPORTED_FILE_TYPE",
            "Bu dosya türü web sürümünde henüz desteklenmiyor.",
        )
    }
    pub const fn file_type_mismatch() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "FILE_TYPE_MISMATCH",
            "Dosyanın içeriği uzantısıyla uyuşmuyor.",
        )
    }
    pub const fn file_corrupt() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "FILE_CORRUPT",
            "Dosya okunamadı veya bozuk.",
        )
    }
    pub const fn session_quota_exceeded() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "SESSION_QUOTA_EXCEEDED",
            "Oturum depolama sınırına ulaşıldı. Bazı dosyaları silip yeniden deneyin.",
        )
    }
    pub const fn unsupported_conversion() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "UNSUPPORTED_CONVERSION",
            "Bu dönüştürme web sürümünde desteklenmiyor.",
        )
    }
    pub const fn invalid_dimensions() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_DIMENSIONS",
            "İstenen boyutlar geçersiz.",
        )
    }
    pub const fn too_many_jobs() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "TOO_MANY_JOBS",
            "Aynı anda çok fazla işiniz var. Bazılarının bitmesini bekleyin.",
        )
    }
    pub const fn server_busy() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVER_BUSY",
            "Sunucu şu anda yoğun. Lütfen biraz sonra yeniden deneyin.",
        )
    }
    pub const fn not_ready() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "NOT_READY",
            "Dönüştürme henüz tamamlanmadı.",
        )
    }

    /// Maps a native-image engine error (by its stable code) onto an upload
    /// validation error. Unknown codes collapse to FILE_CORRUPT.
    pub fn from_image_probe(code: &str) -> Self {
        match code {
            "IMAGE_TOO_LARGE" => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "IMAGE_TOO_LARGE",
                "Görsel güvenli işlem sınırlarını aşıyor.",
            ),
            "IMAGE_MULTI_FRAME_UNSUPPORTED" => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "IMAGE_MULTI_FRAME_UNSUPPORTED",
                "Hareketli GIF ve çok sayfalı TIFF henüz desteklenmiyor.",
            ),
            "UNSUPPORTED_IMAGE_FORMAT" => Self::file_type_mismatch(),
            _ => Self::file_corrupt(),
        }
    }

    /// Maps a document probe error (`meb_core::document`) onto an upload
    /// validation error. Its codes are already this vocabulary; an unknown
    /// one collapses to FILE_CORRUPT rather than being trusted.
    pub fn from_document_probe(code: &str) -> Self {
        match code {
            "UNSUPPORTED_FILE_TYPE" => Self::unsupported_file_type(),
            "FILE_TYPE_MISMATCH" => Self::file_type_mismatch(),
            _ => Self::file_corrupt(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": { "code": self.code, "message": self.message } });
        if let Some(incident) = &self.incident {
            body["error"]["incident_id"] = json!(incident);
        }
        (self.status, Json(body)).into_response()
    }
}

/// Turkish text for a job's terminal error code (shown in job snapshots).
/// Codes come from the engine (`meb_core::image` error kinds) or the worker.
pub fn job_error_message(code: &str) -> &'static str {
    match code {
        "UNSUPPORTED_IMAGE_FORMAT" => "Bu görsel biçimi henüz desteklenmiyor.",
        "IMAGE_TOO_LARGE" => "Görsel güvenli işlem sınırlarını aşıyor.",
        "IMAGE_DECODE_FAILED" => "Görsel dosyası okunamadı.",
        "IMAGE_ENCODE_FAILED" => "Dönüştürülen görsel yazılamadı.",
        "INVALID_DIMENSIONS" => "İstenen boyutlar geçersiz.",
        "IMAGE_MULTI_FRAME_UNSUPPORTED" => {
            "Hareketli GIF ve çok sayfalı TIFF henüz desteklenmiyor."
        }
        "PDF_ENGINE_UNAVAILABLE" => "PDF işlem motoru bu sunucuda kullanılamıyor.",
        "PDF_OPERATION_FAILED" => "PDF dosyası işlenemedi.",
        "PDF_ENGINE_TIMEOUT" => "PDF işlemi zaman sınırını aştı ve durduruldu.",
        "PDF_PAGE_UNAVAILABLE" => "İstenen sayfalardan biri bu PDF dosyasında bulunamadı.",
        "OFFICE_ENGINE_UNAVAILABLE" => "Ofis dönüştürme motoru bu sunucuda kullanılamıyor.",
        "OFFICE_CONVERSION_FAILED" => "Belge dönüştürülemedi.",
        "OFFICE_ENGINE_TIMEOUT" => "Belge dönüştürme zaman sınırını aştı ve durduruldu.",
        "OFFICE_NO_OUTPUT" => "Dönüştürme motoru bu belgeden çıktı üretemedi.",
        "OUTPUT_VALIDATION_FAILED" => "Dönüştürme çıktısı doğrulanamadı.",
        "WORKSPACE_ERROR" => "Çalışma alanı hazırlanamadı.",
        _ => "Beklenmeyen bir sunucu hatası oluştu.",
    }
}
