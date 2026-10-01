// Pure helpers for Ses Dikte (no React, no Tauri) so they are unit-testable.

import type {
  SpeechEngineStatus,
  SpeechErrorDto,
  SpeechSegment,
} from "../types/speech";
import type { CapabilityState } from "../store/useStore";

/**
 * Formats the file picker filter only. The backend (`accepted_extension` in
 * audio_prep.rs) is the authority; an extension is listed there only once a
 * real fixture of that type passes end to end.
 */
export const SPEECH_AUDIO_EXTENSIONS = ["wav", "mp3", "m4a", "aac", "flac", "ogg"] as const;

export function fileNameOf(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function hasSupportedAudioExtension(path: string): boolean {
  const name = fileNameOf(path).toLowerCase();
  const dot = name.lastIndexOf(".");
  if (dot < 0) return false;
  return (SPEECH_AUDIO_EXTENSIONS as readonly string[]).includes(name.slice(dot + 1));
}

/** mm:ss, or h:mm:ss from one hour up. */
export function formatElapsed(totalSecs: number): string {
  const s = Math.max(0, Math.floor(totalSecs));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${pad(m)}:${pad(sec)}`;
}

export function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
  return `${(mb / 1024).toFixed(1)} GB`;
}

/** "[HH:MM:SS]" - segment START, approximate (never frame-accurate). */
export function formatTimestamp(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const pad = (n: number) => String(n).padStart(2, "0");
  return `[${pad(Math.floor(s / 3600))}:${pad(Math.floor((s % 3600) / 60))}:${pad(s % 60)}]`;
}

/**
 * The text shown in the editor. With timestamps: one line per segment,
 * `[00:00:12] text`. Timestamps are only ever produced from segments the
 * engine reported - callers must not offer this when
 * `timestamps_available` is false.
 */
export function renderTranscript(segments: SpeechSegment[], withTimestamps: boolean): string {
  if (withTimestamps) {
    return segments.map((s) => `${formatTimestamp(s.start_ms)} ${s.text}`).join("\n");
  }
  return segments.map((s) => s.text).join(" ");
}

/** Backend-reported engine status -> the three user-facing labels. */
export type SpeechAvailabilityLabel = "ready" | "missing" | "unsupported";

export function availabilityOf(status: SpeechEngineStatus): SpeechAvailabilityLabel {
  if (status === "AVAILABLE") return "ready";
  if (status === "CPU_UNSUPPORTED") return "unsupported";
  return "missing";
}

/** Same mapping for the generic capability-state enum (system status modal). */
export function availabilityOfCapability(state: CapabilityState): SpeechAvailabilityLabel {
  if (state === "AVAILABLE") return "ready";
  if (state === "NOT_IMPLEMENTED") return "unsupported";
  return "missing";
}

const GENERIC_ERROR: SpeechErrorDto = {
  code: "SPEECH_TRANSCRIPTION_FAILED",
  message: "Beklenmeyen bir hata oluştu. Lütfen yeniden deneyin.",
};

/**
 * Normalizes whatever a rejected `invoke` produced into a structured
 * {code, message}. The backend rejects with a `SpeechErrorDto` whose
 * `message` is already Turkish; anything else (a bare string, an Error) is
 * NEVER shown raw - it may contain internal detail.
 */
export function toSpeechError(e: unknown): SpeechErrorDto {
  if (e && typeof e === "object") {
    const o = e as Record<string, unknown>;
    if (typeof o.code === "string" && typeof o.message === "string") {
      return { code: o.code, message: o.message };
    }
  }
  return GENERIC_ERROR;
}
