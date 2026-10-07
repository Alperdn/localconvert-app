import { API_BASE, CSRF_HEADER, WebApiError, errorFromResponse } from "./client";

/// Server view of an uploaded file (`POST /api/v1/files` response).
export interface WebFileView {
  file_id: string;
  display_name: string;
  size: number;
  detected_format: string;
  width: number;
  height: number;
  created_at_ms: number;
}

/// Encodes a display name for the `X-File-Name` header (header values must
/// be ASCII; Turkish names are percent-encoded UTF-8).
export function encodeFileNameHeader(name: string): string {
  return encodeURIComponent(name);
}

/**
 * Streams one file to the server as the raw request body. Uses XHR (not
 * fetch) because only XHR reports real upload progress in every browser.
 * The server decides the file type from its content; nothing here claims
 * a MIME type. `onProgress` receives 0..1 from real bytes sent.
 */
export function uploadFile(
  file: File,
  onProgress?: (fraction: number) => void,
  signal?: AbortSignal
): Promise<WebFileView> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `${API_BASE}/files`);
    xhr.withCredentials = false; // same-origin: cookies are sent anyway
    xhr.setRequestHeader(CSRF_HEADER, "1");
    xhr.setRequestHeader("X-File-Name", encodeFileNameHeader(file.name));
    // Deliberately generic: the server ignores Content-Type and sniffs.
    xhr.setRequestHeader("Content-Type", "application/octet-stream");

    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable && onProgress) onProgress(event.loaded / event.total);
    };
    xhr.onload = () => {
      if (xhr.status === 201) {
        try {
          resolve(JSON.parse(xhr.responseText) as WebFileView);
        } catch {
          reject(new WebApiError("INTERNAL_ERROR", xhr.status));
        }
      } else {
        reject(errorFromResponse(xhr.status, xhr.responseText));
      }
    };
    xhr.onerror = () => reject(new WebApiError("NETWORK_ERROR", 0));
    xhr.onabort = () => reject(new WebApiError("CANCELLED", 0));

    if (signal) {
      if (signal.aborted) {
        reject(new WebApiError("CANCELLED", 0));
        return;
      }
      signal.addEventListener("abort", () => xhr.abort(), { once: true });
    }
    xhr.send(file);
  });
}
