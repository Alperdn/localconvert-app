import { API_BASE, apiRequest } from "./client";

export type WebJobState =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancel_requested"
  | "cancelled";

/// Mirrors crates/meb-server/src/jobs.rs `JobSnapshot`.
export interface WebJobSnapshot {
  job_id: string;
  kind: string;
  file_id: string;
  output_format: string;
  state: WebJobState;
  progress_pct: number | null;
  error: { code: string; message: string } | null;
  result: { output_name: string; size: number; content_type: string } | null;
  created_at_ms: number;
  updated_at_ms: number;
  seq: number;
}

export interface WebConvertOptions {
  quality?: number;
  width?: number;
  height?: number;
}

export function isTerminal(state: WebJobState): boolean {
  return state === "completed" || state === "failed" || state === "cancelled";
}

export function createConvertJob(
  fileId: string,
  outputFormat: string,
  options: WebConvertOptions
): Promise<WebJobSnapshot> {
  return apiRequest<WebJobSnapshot>("POST", "/jobs", {
    kind: "convert",
    file_id: fileId,
    output_format: outputFormat,
    options,
  });
}

export function getJob(jobId: string): Promise<WebJobSnapshot> {
  return apiRequest<WebJobSnapshot>("GET", `/jobs/${encodeURIComponent(jobId)}`);
}

export function cancelJob(jobId: string): Promise<WebJobSnapshot> {
  return apiRequest<WebJobSnapshot>("POST", `/jobs/${encodeURIComponent(jobId)}/cancel`);
}

export function deleteJob(jobId: string): Promise<void> {
  return apiRequest<void>("DELETE", `/jobs/${encodeURIComponent(jobId)}`);
}

/// Same-origin download URL; the server enforces ownership and serves it
/// as an attachment with a server-generated file name.
export function jobDownloadUrl(jobId: string): string {
  return `${API_BASE}/jobs/${encodeURIComponent(jobId)}/download`;
}

/**
 * Follows a job until it reaches a terminal state and resolves with that
 * final snapshot. Primary channel: Server-Sent Events, whose first event is
 * always the current snapshot (so a reconnect never misses state). If the
 * stream errors, it falls back to polling the snapshot endpoint - the
 * server's snapshot is the source of truth either way.
 */
export function watchJob(
  jobId: string,
  onSnapshot: (snapshot: WebJobSnapshot) => void,
  pollIntervalMs = 1000
): Promise<WebJobSnapshot> {
  return new Promise((resolve, reject) => {
    let lastSeq = -1;
    let settled = false;
    let source: EventSource | null = null;
    let pollTimer: ReturnType<typeof setTimeout> | null = null;

    const finish = (snapshot: WebJobSnapshot) => {
      if (settled) return;
      settled = true;
      source?.close();
      if (pollTimer) clearTimeout(pollTimer);
      resolve(snapshot);
    };

    const accept = (snapshot: WebJobSnapshot) => {
      if (settled || snapshot.seq < lastSeq) return; // never go backwards
      lastSeq = snapshot.seq;
      onSnapshot(snapshot);
      if (isTerminal(snapshot.state)) finish(snapshot);
    };

    const poll = async () => {
      if (settled) return;
      try {
        accept(await getJob(jobId));
      } catch (error) {
        settled = true;
        reject(error);
        return;
      }
      if (!settled) pollTimer = setTimeout(poll, pollIntervalMs);
    };

    if (typeof EventSource === "undefined") {
      void poll();
      return;
    }

    source = new EventSource(`${API_BASE}/jobs/${encodeURIComponent(jobId)}/events`);
    source.addEventListener("job", (event) => {
      try {
        accept(JSON.parse((event as MessageEvent<string>).data) as WebJobSnapshot);
      } catch {
        // ignore a malformed event; the snapshot endpoint is authoritative
      }
    });
    source.onerror = () => {
      // The server closes the stream after the terminal event; any other
      // error (proxy, network) switches to polling the snapshot.
      source?.close();
      source = null;
      if (!settled) void poll();
    };
  });
}
