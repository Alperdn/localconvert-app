import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  SPEECH_UPDATE_EVENT,
  type SpeechErrorDto,
  type SpeechJobUpdate,
  type SpeechPhase,
  type SpeechTranscript,
} from "../types/speech";
import { toSpeechError } from "../utils/speech";

export interface SpeechJobView {
  phase: SpeechPhase;
  elapsedSecs: number;
  /** Real engine progress only. null = indeterminate (never faked). */
  progressPct: number | null;
  transcript: SpeechTranscript | null;
  error: SpeechErrorDto | null;
}

const IDLE: SpeechJobView = {
  phase: "idle",
  elapsedSecs: 0,
  progressPct: null,
  transcript: null,
  error: null,
};

const RUNNING: SpeechPhase[] = ["preparing", "transcribing"];

/**
 * Drives one Ses Dikte job. All state comes from backend `speech-job-update`
 * events; the hook itself invents nothing (no fake percentage, no fake state).
 *
 * A failed/cancelled job never poisons the next one: each `start` gets a fresh
 * job id from the backend and resets the view.
 *
 * Leaving the page (unmount) cancels a running job: its result would have
 * nowhere to go, and cancelling stops the CPU work and deletes its temp files.
 */
export function useSpeechJob() {
  const [view, setView] = useState<SpeechJobView>(IDLE);
  const jobIdRef = useRef<string | null>(null);
  const startingRef = useRef(false);
  const cancelRequestedRef = useRef(false);
  // Set by `discard()` while a job is still being started: the job is cancelled
  // the moment the backend reveals its id, and is never shown.
  const discardedRef = useRef(false);
  const phaseRef = useRef<SpeechPhase>("idle");

  const apply = useCallback((next: SpeechJobView) => {
    phaseRef.current = next.phase;
    setView(next);
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    listen<SpeechJobUpdate>(SPEECH_UPDATE_EVENT, (event) => {
      const u = event.payload;
      // The first events can arrive before `invoke` has returned the job id.
      if (jobIdRef.current === null && startingRef.current) {
        jobIdRef.current = u.job_id;
      }
      if (u.job_id !== jobIdRef.current) return;

      const terminal = u.state === "completed" || u.state === "cancelled" || u.state === "failed";
      apply({
        phase: u.state,
        elapsedSecs: u.elapsed_secs,
        progressPct: u.progress_pct,
        transcript: u.transcript,
        error: u.error,
      });
      if (terminal) {
        jobIdRef.current = null;
        startingRef.current = false;
      }
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });

    return () => {
      disposed = true;
      unlisten?.();
      if (jobIdRef.current && RUNNING.includes(phaseRef.current)) {
        void invoke("speech_cancel_job", { jobId: jobIdRef.current }).catch(() => {});
      }
    };
  }, [apply]);

  const start = useCallback(
    async (path: string, language: string) => {
      jobIdRef.current = null;
      cancelRequestedRef.current = false;
      discardedRef.current = false;
      startingRef.current = true;
      apply({ ...IDLE, phase: "preparing" });
      try {
        const id = await invoke<string>("speech_start_file_job", { path, language });
        if (discardedRef.current) {
          // The source was removed while this job was starting: stop it, show nothing.
          void invoke("speech_cancel_job", { jobId: id }).catch(() => {});
          return;
        }
        // A fast job may already have finished (and cleared the ref) by now.
        if (!RUNNING.includes(phaseRef.current)) return;
        if (jobIdRef.current === null) jobIdRef.current = id;
        if (cancelRequestedRef.current) {
          void invoke("speech_cancel_job", { jobId: id }).catch(() => {});
        }
      } catch (e) {
        if (discardedRef.current) return; // removed meanwhile: stay empty
        apply({ ...IDLE, phase: "failed", error: toSpeechError(e) });
      } finally {
        startingRef.current = false;
      }
    },
    [apply]
  );

  const cancel = useCallback(() => {
    if (jobIdRef.current) {
      void invoke("speech_cancel_job", { jobId: jobIdRef.current }).catch(() => {});
    } else if (startingRef.current) {
      cancelRequestedRef.current = true;
    }
  }, []);

  const reset = useCallback(() => {
    jobIdRef.current = null;
    apply(IDLE);
  }, [apply]);

  /**
   * Cancel whatever is running (or starting), then return to idle. Forgetting
   * the job id first means late events from the cancelled job are ignored, so
   * nothing stale can reappear. Backend cancel removes the job's temp files;
   * the user's source file is never touched.
   */
  const discard = useCallback(() => {
    if (jobIdRef.current) {
      void invoke("speech_cancel_job", { jobId: jobIdRef.current }).catch(() => {});
    } else if (startingRef.current) {
      discardedRef.current = true;
    }
    jobIdRef.current = null;
    startingRef.current = false;
    apply(IDLE);
  }, [apply]);

  return { ...view, start, cancel, reset, discard };
}
