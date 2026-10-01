// Mirrors src-tauri/src/speech/* and engines/speech*.rs (serde output).
// The frontend never supplies an executable, model path or engine name -
// only a user-picked audio file path and a language code the backend
// manifest declared.

export type SpeechEngineStatus =
  | "AVAILABLE"
  | "ENGINE_MISSING"
  | "MODEL_MISSING"
  | "ENGINE_INVALID"
  | "CPU_UNSUPPORTED"
  | "RUNTIME_MISSING"
  | "AUDIO_PREP_UNAVAILABLE";

export interface SpeechEngineReport {
  status: SpeechEngineStatus;
  /** Turkish, safe to show as-is (never a path or executable name). */
  message: string;
  engine: string | null;
  model: string | null;
  languages: string[];
  supports_timestamps: boolean;
  /** Turkish heads-up for CPUs without fast instruction sets. Never a blocker. */
  performance_note?: string | null;
}

export interface SpeechSegment {
  start_ms: number;
  end_ms: number;
  text: string;
}

export interface SpeechTranscript {
  language: string;
  text: string;
  segments: SpeechSegment[];
  /** Segment-level, approximate. false => the timestamps option must be hidden. */
  timestamps_available: boolean;
}

export interface SpeechErrorDto {
  code: string;
  message: string;
}

export interface SpeechInputInfo {
  file_name: string;
  size_bytes: number;
  duration_secs: number | null;
  extension: string;
}

export interface SpeechLimits {
  max_input_mb: number;
  max_audio_hours: number;
  max_recording_hours: number;
}

export type SpeechJobState =
  | "preparing"
  | "transcribing"
  | "completed"
  | "cancelled"
  | "failed";

export interface SpeechJobUpdate {
  job_id: string;
  state: SpeechJobState;
  elapsed_secs: number;
  /** Real engine-reported progress only; null = indeterminate. */
  progress_pct: number | null;
  transcript: SpeechTranscript | null;
  error: SpeechErrorDto | null;
}

/** UI phase: the job states plus the two the backend never emits. */
export type SpeechPhase = "idle" | SpeechJobState;

export const SPEECH_UPDATE_EVENT = "speech-job-update";
