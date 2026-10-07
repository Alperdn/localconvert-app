// Minimal HTTP client for meb-server (/api/v1). Same-origin only: in
// development Vite proxies /api to the server (vite.config.ts), so the
// session cookie and every request stay on the page's own origin.

export const API_BASE = "/api/v1";

/// Required by the server on every state-changing request (CSRF guard).
export const CSRF_HEADER = "X-MEB-Request";

/// A structured server error. `code` is a stable SCREAMING_SNAKE id that
/// the UI translates via `translateErrorCode` (locales `errorCodes.*`).
export class WebApiError extends Error {
  readonly code: string;
  readonly status: number;

  constructor(code: string, status: number, message?: string) {
    super(message ?? code);
    this.name = "WebApiError";
    this.code = code;
    this.status = status;
  }
}

/// Extracts `{ error: { code } }` from a server response body, falling back
/// to a generic code - never surfaces raw response text.
export function errorFromResponse(status: number, bodyText: string): WebApiError {
  try {
    const parsed = JSON.parse(bodyText) as { error?: { code?: unknown; message?: unknown } };
    const code = parsed?.error?.code;
    const message = parsed?.error?.message;
    if (typeof code === "string" && /^[A-Z0-9_]+$/.test(code)) {
      return new WebApiError(code, status, typeof message === "string" ? message : undefined);
    }
  } catch {
    // not JSON
  }
  return new WebApiError(status >= 500 ? "INTERNAL_ERROR" : "PROCESS_FAILED", status);
}

export async function apiRequest<T>(
  method: "GET" | "POST" | "DELETE",
  path: string,
  body?: unknown
): Promise<T> {
  const headers: Record<string, string> = {};
  if (method !== "GET") headers[CSRF_HEADER] = "1";
  if (body !== undefined) headers["Content-Type"] = "application/json";

  let response: Response;
  try {
    response = await fetch(`${API_BASE}${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      credentials: "same-origin",
      cache: "no-store",
    });
  } catch {
    throw new WebApiError("NETWORK_ERROR", 0);
  }

  if (!response.ok) {
    throw errorFromResponse(response.status, await response.text());
  }
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}
