import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, ask } from "@tauri-apps/plugin-dialog";
import toast from "react-hot-toast";
import { AlertTriangle, CheckCircle2, Clock, FileAudio, Lock, Mic, Upload, XCircle } from "lucide-react";
import { useStore } from "../../store/useStore";
import { t } from "../../locales";
import { useSpeechJob } from "../../hooks/useSpeechJob";
import type { SpeechEngineReport, SpeechInputInfo, SpeechLimits } from "../../types/speech";
import {
  SPEECH_AUDIO_EXTENSIONS,
  availabilityOf,
  formatElapsed,
  formatFileSize,
  renderTranscript,
  toSpeechError,
} from "../../utils/speech";

interface SelectedSource {
  path: string;
  info: SpeechInputInfo;
}

const LANGUAGE_OPTIONS: { code: string; labelKey: "speech.languageTurkish" | "speech.languageAuto" }[] = [
  { code: "tr", labelKey: "speech.languageTurkish" },
  { code: "auto", labelKey: "speech.languageAuto" },
];

/**
 * Ses Dikte (local speech-to-text), file mode. The microphone tab is present
 * but disabled: nothing on this page ever calls getUserMedia, so the
 * microphone can never be activated by loading or leaving the page.
 */
export function SpeechDictationPage() {
  const isDark = useStore((s) => s.settings.theme) === "dark";
  const job = useSpeechJob();

  const [report, setReport] = useState<SpeechEngineReport | null>(null);
  const [limits, setLimits] = useState<SpeechLimits | null>(null);
  const [source, setSource] = useState<SelectedSource | null>(null);
  const [inspecting, setInspecting] = useState(false);
  const [sourceError, setSourceError] = useState<{ code: string; message: string } | null>(null);
  const [language, setLanguage] = useState("tr");
  const [showTimestamps, setShowTimestamps] = useState(false);
  const [editedText, setEditedText] = useState<string | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const running = job.phase === "preparing" || job.phase === "transcribing";
  const available = report?.status === "AVAILABLE";

  useEffect(() => {
    invoke<SpeechEngineReport>("speech_get_status")
      .then(setReport)
      .catch(() => setReport(null));
    invoke<SpeechLimits>("speech_get_limits")
      .then(setLimits)
      .catch(() => setLimits(null));
  }, []);

  const languages = useMemo(() => {
    const declared = report?.languages ?? ["tr"];
    return LANGUAGE_OPTIONS.filter((o) => declared.includes(o.code));
  }, [report]);

  const pickSource = useCallback(async (path: string) => {
    setSourceError(null);
    setInspecting(true);
    try {
      const info = await invoke<SpeechInputInfo>("speech_inspect_file", { path });
      setSource({ path, info });
      job.reset();
      setEditedText(null);
    } catch (e) {
      setSource(null);
      setSourceError(toSpeechError(e));
    } finally {
      setInspecting(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // One native picker at a time. The ref is the synchronous guard (two clicks
  // in the same tick both see stale state); the state only drives the UI.
  // Both are released when the dialog promise settles, not on a timer.
  const pickerOpenRef = useRef(false);
  const [pickerOpen, setPickerOpen] = useState(false);

  const handleBrowse = useCallback(async () => {
    if (pickerOpenRef.current) return;
    pickerOpenRef.current = true;
    setPickerOpen(true);
    try {
      // The native dialog is owned by this window and inherits its foreground
      // state, so make sure the window is active first. Best effort only.
      try {
        await getCurrentWindow().setFocus();
      } catch {
        /* no Tauri window (tests) or focus refused: the picker still opens */
      }
      const selected = await open({
        multiple: false,
        directory: false,
        filters: [{ name: t("speech.sourceFile"), extensions: [...SPEECH_AUDIO_EXTENSIONS] }],
      });
      if (typeof selected === "string") await pickSource(selected);
    } catch {
      toast.error(t("speech.fileChooseFailed"));
    } finally {
      pickerOpenRef.current = false;
      setPickerOpen(false);
    }
  }, [pickSource]);

  // Native drag & drop. (App.tsx's global drop handler ignores drops while
  // this view is active, so nothing leaks into the converter.)
  useEffect(() => {
    if (running) return;
    const subs = [
      listen("tauri://drag-enter", () => setIsDragging(true)),
      listen("tauri://drag-leave", () => setIsDragging(false)),
      listen<{ paths: string[] }>("tauri://drag-drop", (event) => {
        setIsDragging(false);
        const first = event.payload?.paths?.[0];
        if (first) void pickSource(first);
      }),
    ];
    return () => {
      subs.forEach((p) => p.then((un) => un()));
    };
  }, [running, pickSource]);

  const rendered = useMemo(
    () => (job.transcript ? renderTranscript(job.transcript.segments, showTimestamps && job.transcript.timestamps_available) : ""),
    [job.transcript, showTimestamps]
  );
  const displayText = editedText ?? rendered;

  const confirmDiscardEdits = useCallback(async () => {
    if (editedText === null) return true;
    return ask(t("speech.confirmDiscardEdits"), { kind: "warning" });
  }, [editedText]);

  // Forget the selected source. Only app state is cleared: the user's original
  // file is never deleted, moved or modified (the backend only removes its own
  // temp files when a job is cancelled).
  const removeSource = useCallback(async () => {
    if (!(await confirmDiscardEdits())) return;
    job.discard();
    setSource(null);
    setSourceError(null);
    setEditedText(null);
    setShowTimestamps(false);
  }, [job, confirmDiscardEdits]);

  const startJob = useCallback(async () => {
    if (!source) return;
    if (!(await confirmDiscardEdits())) return;
    setEditedText(null);
    await job.start(source.path, language);
  }, [source, language, job, confirmDiscardEdits]);

  const toggleTimestamps = useCallback(
    async (checked: boolean) => {
      if (!(await confirmDiscardEdits())) return;
      setEditedText(null);
      setShowTimestamps(checked);
    },
    [confirmDiscardEdits]
  );

  const copyText = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(displayText);
      toast.success(t("speech.copied"));
    } catch {
      toast.error(t("speech.copyFailed"));
    }
  }, [displayText]);

  const stateLabel =
    job.phase === "idle"
      ? t("speech.stateReady")
      : job.phase === "preparing"
        ? t("speech.statePreparing")
        : job.phase === "transcribing"
          ? t("speech.stateTranscribing")
          : job.phase === "completed"
            ? t("speech.stateCompleted")
            : job.phase === "cancelled"
              ? t("speech.stateCancelled")
              : t("speech.stateFailed");

  const availabilityText = (s: SpeechEngineReport["status"]) => {
    const a = availabilityOf(s);
    return a === "ready"
      ? t("speech.availabilityReady")
      : a === "unsupported"
        ? t("speech.availabilityUnsupported")
        : t("speech.availabilityMissing");
  };

  const muted = isDark ? "text-dark-400" : "text-dark-500";
  const strong = isDark ? "text-white" : "text-dark-900";
  const card = isDark ? "bg-dark-800/40 border-dark-700/50" : "bg-white/50 border-dark-100";
  const noSpeech = job.error?.code === "SPEECH_NO_SPEECH_DETECTED";

  return (
    <div className="absolute inset-0 overflow-y-auto overflow-x-hidden custom-scrollbar p-4 xl:p-6" data-testid="speech-page">
      <div className="mx-auto w-full max-w-3xl space-y-4">
        {/* Header + privacy note */}
        <div>
          <h1 className={`text-xl font-bold ${strong}`}>{t("speech.title")}</h1>
          <p className={`mt-1 flex items-center gap-1.5 text-xs ${muted}`} data-testid="speech-privacy-note">
            <Lock className="w-3 h-3 shrink-0" />
            {t("speech.privacyNote")}
          </p>
        </div>

        {/* Engine not usable: block, explain in Turkish, no fallback of any kind. */}
        {report && !available && (
          <div
            role="alert"
            data-testid="speech-blocked"
            className={`rounded-xl border p-4 flex gap-3 ${isDark ? "border-amber-500/30 bg-amber-500/10" : "border-amber-300 bg-amber-50"}`}
          >
            <AlertTriangle className="w-5 h-5 text-amber-500 shrink-0 mt-0.5" />
            <div className="min-w-0">
              <p className={`text-sm font-semibold ${strong}`}>
                {t("speech.blockedTitle")} · {availabilityText(report.status)}
              </p>
              <p className={`text-sm mt-1 ${muted}`}>{report.message}</p>
            </div>
          </div>
        )}

        {available && report?.performance_note && (
          <p className={`flex items-start gap-1.5 text-xs ${muted}`} data-testid="speech-performance-note">
            <Clock className="w-3 h-3 shrink-0 mt-0.5" />
            {report.performance_note}
          </p>
        )}

        {/* Source tabs */}
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-2" role="tablist">
          <button
            role="tab"
            aria-selected={false}
            aria-disabled="true"
            disabled
            data-testid="tab-mic"
            className={`flex items-center justify-center gap-2 px-4 py-3 rounded-xl border text-sm font-medium opacity-60 cursor-not-allowed ${card} ${muted}`}
          >
            <Mic className="w-4 h-4" />
            {t("speech.sourceMic")}
            <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-dark-500/20">{t("speech.comingSoon")}</span>
          </button>
          {/* Same handler as the dropzone: one picker implementation, one guard. */}
          <button
            type="button"
            role="tab"
            aria-selected={true}
            aria-busy={pickerOpen}
            data-testid="tab-file"
            onClick={handleBrowse}
            disabled={pickerOpen || running || inspecting}
            className="flex items-center justify-center gap-2 px-4 py-3 rounded-xl text-sm font-medium text-white bg-accent-gradient disabled:opacity-70 disabled:cursor-wait"
          >
            <Upload className="w-4 h-4" />
            {t("speech.sourceFile")}
          </button>
        </div>

        {/* Language */}
        <div>
          <label htmlFor="speech-language" className={`block text-[11px] font-bold uppercase tracking-widest mb-1.5 ${muted}`}>
            {t("speech.language")}
          </label>
          <select
            id="speech-language"
            data-testid="speech-language"
            value={language}
            disabled={running}
            onChange={(e) => setLanguage(e.target.value)}
            className={`w-full sm:w-64 rounded-lg border px-3 py-2 text-sm ${
              isDark ? "bg-dark-800 border-dark-700 text-white" : "bg-white border-dark-200 text-dark-900"
            }`}
          >
            {languages.map((o) => (
              <option key={o.code} value={o.code}>
                {t(o.labelKey)}
              </option>
            ))}
          </select>
        </div>

        {/* Source area */}
        {!source ? (
          <button
            type="button"
            data-testid="speech-dropzone"
            onClick={handleBrowse}
            disabled={inspecting || running || pickerOpen}
            aria-busy={pickerOpen}
            className={`w-full rounded-2xl border-2 border-dashed p-8 text-center transition-colors disabled:cursor-wait ${
              isDragging ? "border-brand bg-brand/10" : isDark ? "border-dark-700 hover:border-dark-500" : "border-dark-200 hover:border-brand"
            }`}
          >
            <FileAudio className={`w-8 h-8 mx-auto mb-2 ${muted}`} />
            <p className={`text-sm font-semibold ${strong}`}>
              {pickerOpen ? t("speech.pickerOpen") : inspecting ? t("speech.inspecting") : t("speech.dropHere")}
            </p>
            <p className={`text-xs mt-1 ${muted}`}>{t("speech.orBrowse")}</p>
            <p className={`text-xs mt-3 ${muted}`}>{t("speech.supportedFormats")}</p>
            {limits && (
              <p className={`text-xs mt-1 ${muted}`}>
                {t("speech.limitsNote")
                  .replace("{size}", String(limits.max_input_mb))
                  .replace("{hours}", String(limits.max_audio_hours))}
              </p>
            )}
          </button>
        ) : (
          <div className={`rounded-xl border p-4 ${card}`} data-testid="speech-file-card">
            <div className="flex items-start gap-3">
              <FileAudio className="w-5 h-5 shrink-0 text-brand mt-0.5" />
              <dl className="min-w-0 flex-1 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
                <dt className={muted}>{t("speech.fileName")}</dt>
                <dd className={`truncate font-medium ${strong}`} title={source.info.file_name}>{source.info.file_name}</dd>
                <dt className={muted}>{t("speech.fileSize")}</dt>
                <dd className={strong}>{formatFileSize(source.info.size_bytes)}</dd>
                <dt className={muted}>{t("speech.fileDuration")}</dt>
                <dd className={strong}>
                  {source.info.duration_secs != null ? formatElapsed(source.info.duration_secs) : t("speech.durationUnknown")}
                </dd>
                <dt className={muted}>{t("speech.selectedLanguage")}</dt>
                <dd className={strong}>{languages.find((l) => l.code === language) ? t(languages.find((l) => l.code === language)!.labelKey) : language}</dd>
                <dt className={muted}>{t("speech.status")}</dt>
                <dd className={strong} data-testid="speech-state">{stateLabel}</dd>
              </dl>
              <div className="flex flex-col items-end gap-1.5 shrink-0">
                {!running && (
                  <button
                    type="button"
                    data-testid="speech-change-file"
                    onClick={handleBrowse}
                    disabled={pickerOpen}
                    aria-busy={pickerOpen}
                    className="text-xs underline text-brand disabled:opacity-50 disabled:cursor-wait"
                  >
                    {t("speech.changeFile")}
                  </button>
                )}
                <button
                  type="button"
                  data-testid="speech-remove-file"
                  onClick={() => void removeSource()}
                  disabled={pickerOpen || inspecting}
                  className="text-xs underline text-error-500 disabled:opacity-50"
                >
                  {t("speech.removeFile")}
                </button>
              </div>
            </div>
          </div>
        )}

        {sourceError && (
          <p role="alert" data-testid="speech-source-error" className="flex items-start gap-2 text-sm text-error-500">
            <XCircle className="w-4 h-4 shrink-0 mt-0.5" />
            {sourceError.message}
          </p>
        )}

        {/* Actions */}
        <div className="flex flex-wrap items-center gap-2">
          {!running && job.phase !== "failed" && job.phase !== "cancelled" && (
            <button
              data-testid="speech-start"
              onClick={startJob}
              disabled={!source || !available || inspecting}
              className="px-5 py-2.5 rounded-xl text-sm font-semibold text-white bg-accent-gradient disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {job.transcript ? t("speech.redictate") : t("speech.start")}
            </button>
          )}
          {running && (
            <button
              data-testid="speech-cancel"
              onClick={job.cancel}
              className={`px-5 py-2.5 rounded-xl text-sm font-semibold border ${isDark ? "border-dark-600 text-white" : "border-dark-300 text-dark-800"}`}
            >
              {t("speech.cancel")}
            </button>
          )}
          {(job.phase === "failed" || job.phase === "cancelled") && source && (
            <button
              data-testid="speech-retry"
              onClick={startJob}
              disabled={!available}
              className="px-5 py-2.5 rounded-xl text-sm font-semibold text-white bg-accent-gradient disabled:opacity-50"
            >
              {t("speech.retry")}
            </button>
          )}
        </div>

        {/* Progress: indeterminate unless the engine reports real progress. */}
        {running && (
          <div data-testid="speech-progress" className={`rounded-xl border p-4 ${card}`}>
            <p className={`text-sm font-medium ${strong}`}>
              {job.phase === "preparing" ? t("speech.statePreparing") : t("speech.working")}
            </p>
            <div className={`mt-2 h-1.5 rounded-full overflow-hidden ${isDark ? "bg-dark-700" : "bg-dark-100"}`}>
              {job.progressPct != null ? (
                <div className="h-full bg-brand transition-all" style={{ width: `${job.progressPct}%` }} data-testid="speech-progress-determinate" />
              ) : (
                <div className="h-full w-1/3 bg-brand animate-pulse" data-testid="speech-progress-indeterminate" />
              )}
            </div>
            <p className={`mt-2 text-xs flex items-center gap-1.5 ${muted}`} data-testid="speech-elapsed">
              <Clock className="w-3 h-3" />
              {t("speech.elapsed")}: {formatElapsed(job.elapsedSecs)}
              {job.progressPct != null && <span> · %{job.progressPct}</span>}
            </p>
          </div>
        )}

        {/* Result states */}
        {job.phase === "cancelled" && (
          <p role="status" className={`text-sm ${muted}`} data-testid="speech-cancelled">
            {job.error?.message ?? t("speech.stateCancelled")}
          </p>
        )}
        {job.phase === "failed" && job.error && (
          <p
            role="alert"
            data-testid="speech-error"
            className={`flex items-start gap-2 text-sm ${noSpeech ? "text-amber-500" : "text-error-500"}`}
          >
            <XCircle className="w-4 h-4 shrink-0 mt-0.5" />
            {job.error.message}
          </p>
        )}
        {job.phase === "completed" && (
          <p role="status" className="flex items-center gap-2 text-sm text-success-500" data-testid="speech-completed">
            <CheckCircle2 className="w-4 h-4" />
            {t("speech.stateCompleted")} · {t("speech.elapsed")}: {formatElapsed(job.elapsedSecs)}
          </p>
        )}

        {/* Transcript */}
        <div>
          <h2 className={`text-[11px] font-bold uppercase tracking-widest mb-1.5 ${muted}`}>{t("speech.transcriptTitle")}</h2>
          <textarea
            ref={textareaRef}
            data-testid="speech-transcript"
            value={displayText}
            onChange={(e) => setEditedText(e.target.value)}
            placeholder={t("speech.transcriptPlaceholder")}
            rows={10}
            className={`w-full rounded-xl border p-3 text-sm leading-relaxed resize-y min-h-[160px] ${
              isDark ? "bg-dark-800 border-dark-700 text-white placeholder:text-dark-500" : "bg-white border-dark-200 text-dark-900"
            }`}
          />
          <div className="mt-2 flex flex-wrap items-center gap-2">
            {job.transcript?.timestamps_available && (
              <label className={`flex items-center gap-1.5 text-xs ${muted}`} title={t("speech.timestampsNote")}>
                <input
                  type="checkbox"
                  data-testid="speech-timestamps"
                  checked={showTimestamps}
                  onChange={(e) => void toggleTimestamps(e.target.checked)}
                />
                {t("speech.showTimestamps")}
              </label>
            )}
            <span className="flex-1" />
            <button
              data-testid="speech-select-all"
              onClick={() => textareaRef.current?.select()}
              disabled={!displayText}
              className={`px-3 py-1.5 rounded-lg text-xs font-medium border disabled:opacity-40 ${isDark ? "border-dark-600 text-white" : "border-dark-300 text-dark-800"}`}
            >
              {t("speech.selectAll")}
            </button>
            <button
              data-testid="speech-copy"
              onClick={copyText}
              disabled={!displayText}
              className={`px-3 py-1.5 rounded-lg text-xs font-medium border disabled:opacity-40 ${isDark ? "border-dark-600 text-white" : "border-dark-300 text-dark-800"}`}
            >
              {t("speech.copy")}
            </button>
            <button
              data-testid="speech-clear"
              onClick={() => setEditedText("")}
              disabled={!displayText}
              className={`px-3 py-1.5 rounded-lg text-xs font-medium border disabled:opacity-40 ${isDark ? "border-dark-600 text-white" : "border-dark-300 text-dark-800"}`}
            >
              {t("speech.clear")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
