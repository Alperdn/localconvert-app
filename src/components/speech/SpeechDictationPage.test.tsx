// @vitest-environment jsdom
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react";
import type { SpeechEngineReport, SpeechJobUpdate } from "../../types/speech";

// ---- controllable Tauri mocks (override src/test/setup.ts's defaults) -------
const h = vi.hoisted(() => {
  type Handler = (e: { payload: unknown }) => void;
  const handlers = new Map<string, Set<Handler>>();
  return {
    handlers,
    invokeMock: vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(() => Promise.resolve(undefined)),
    emit(event: string, payload: unknown) {
      handlers.get(event)?.forEach((fn) => fn({ payload }));
    },
    listenerCount(event: string) {
      return handlers.get(event)?.size ?? 0;
    },
    openMock: vi.fn(),
    askMock: vi.fn(),
    setFocusMock: vi.fn<() => Promise<void>>(() => Promise.resolve()),
    callOrder: [] as string[],
  };
});
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ setFocus: () => h.setFocusMock() }),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => h.invokeMock(cmd, args) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, fn: (e: { payload: unknown }) => void) => {
    if (!h.handlers.has(event)) h.handlers.set(event, new Set());
    h.handlers.get(event)!.add(fn);
    return Promise.resolve(() => h.handlers.get(event)?.delete(fn));
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...a: unknown[]) => h.openMock(...a),
  ask: (...a: unknown[]) => h.askMock(...a),
}));
vi.mock("react-hot-toast", () => ({ default: { success: vi.fn(), error: vi.fn() } }));

import { SpeechDictationPage } from "./SpeechDictationPage";
import { Sidebar } from "../Sidebar";
import { useStore } from "../../store/useStore";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const READY: SpeechEngineReport = {
  status: "AVAILABLE",
  message: "Konuşma tanıma hazır.",
  engine: "whisper.cpp v1.9.2",
  model: "ggml-small-q5_1",
  languages: ["tr", "auto"],
  supports_timestamps: true,
};

const INFO = { file_name: "toplanti.mp3", size_bytes: 3_145_728, duration_secs: 75, extension: "mp3" };

function configure(report: SpeechEngineReport = READY, extra: Record<string, (args: unknown) => unknown> = {}) {
  h.invokeMock.mockImplementation((cmd: string, args: unknown) => {
    if (extra[cmd]) return Promise.resolve().then(() => extra[cmd](args));
    switch (cmd) {
      case "speech_get_status":
        return Promise.resolve(report);
      case "speech_get_limits":
        return Promise.resolve({ max_input_mb: 500, max_audio_hours: 3, max_recording_hours: 2 });
      case "speech_inspect_file":
        return Promise.resolve(INFO);
      case "speech_start_file_job":
        return Promise.resolve("job-1");
      case "speech_cancel_job":
        return Promise.resolve(true);
      default:
        return Promise.resolve(undefined);
    }
  });
}

let container: HTMLDivElement;
let root: Root;

async function mount(ui: React.ReactElement) {
  await act(async () => {
    root.render(ui);
  });
  await flush();
}
async function flush() {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}
const q = (id: string) => container.querySelector(`[data-testid="${id}"]`) as HTMLElement | null;
async function click(el: HTMLElement | null) {
  expect(el).toBeTruthy();
  await act(async () => {
    el!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
}
function update(u: Partial<SpeechJobUpdate>) {
  return act(async () => {
    h.emit("speech-job-update", {
      job_id: "job-1",
      state: "preparing",
      elapsed_secs: 0,
      progress_pct: null,
      transcript: null,
      error: null,
      ...u,
    });
  });
}

const TRANSCRIPT = {
  language: "tr",
  text: "Merhaba. Nasılsınız?",
  timestamps_available: true,
  segments: [
    { start_ms: 12_000, end_ms: 20_000, text: "Merhaba." },
    { start_ms: 24_000, end_ms: 30_000, text: "Nasılsınız?" },
  ],
};

async function selectFileAndStart() {
  h.openMock.mockResolvedValue("C:\\Users\\a\\toplanti.mp3");
  await click(q("speech-dropzone"));
  await click(q("speech-start"));
}

beforeEach(() => {
  h.handlers.clear();
  h.invokeMock.mockReset();
  h.openMock.mockReset();
  h.askMock.mockReset();
  h.setFocusMock.mockReset();
  h.setFocusMock.mockImplementation(() => Promise.resolve());
  h.callOrder.length = 0;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("navigation", () => {
  it("the sidebar has a distinct Ses Dikte entry that switches the view", async () => {
    useStore.setState({ activeView: "convert" });
    configure();
    await mount(<Sidebar />);
    const btn = q("nav-dictation");
    expect(btn).toBeTruthy();
    expect(btn!.textContent).toContain("Ses Dikte");
    // and it's not the audio-conversion category button
    expect(container.textContent).toContain("Ses"); // the conversion category still exists
    await click(btn);
    expect(useStore.getState().activeView).toBe("dictation");
  });

  it("choosing a conversion category leaves the dictation view", () => {
    useStore.setState({ activeView: "dictation" });
    useStore.getState().setActiveCategory("audio");
    expect(useStore.getState().activeView).toBe("convert");
  });
});

describe("privacy + microphone", () => {
  it("shows the exact privacy sentence, without exaggerated claims", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    expect(q("speech-privacy-note")!.textContent).toBe("Ses dosyanız ve dikte işlemi bu cihazda işlenir.");
    expect(container.textContent).not.toMatch(/100\s*%|tamamen güvenli|%100/i);
  });

  it("never activates the microphone on load, and never registers a mic API call", async () => {
    const getUserMedia = vi.fn();
    Object.defineProperty(navigator, "mediaDevices", { value: { getUserMedia }, configurable: true });
    configure();
    await mount(<SpeechDictationPage />);
    await act(async () => root.unmount());
    root = createRoot(container);
    expect(getUserMedia).not.toHaveBeenCalled();
  });

  it("the microphone tab is present but disabled in this phase", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    const mic = q("tab-mic") as HTMLButtonElement;
    expect(mic.disabled).toBe(true);
    expect(mic.textContent).toContain("Mikrofonla Kaydet");
    expect(q("tab-file")!.textContent).toContain("Ses Dosyası Yükle");
  });
});

describe("capability gating (backend is the source of truth)", () => {
  it("ready: start is enabled once a file is chosen", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    expect(q("speech-blocked")).toBeNull();
    h.openMock.mockResolvedValue("C:\\a\\toplanti.mp3");
    await click(q("speech-dropzone"));
    expect(q("speech-file-card")!.textContent).toContain("toplanti.mp3");
    expect(q("speech-file-card")!.textContent).toContain("3.0 MB");
    expect(q("speech-file-card")!.textContent).toContain("01:15");
    expect((q("speech-start") as HTMLButtonElement).disabled).toBe(false);
  });

  it("engine missing: blocks the action, shows 'Bileşen eksik' + the backend's Turkish text, no fallback", async () => {
    configure({
      status: "ENGINE_MISSING",
      message: "Konuşma tanıma bileşeni bu kurulumda bulunamadı. Lütfen uygulamayı yeniden yükleyin.",
      engine: null,
      model: null,
      languages: [],
      supports_timestamps: false,
    });
    await mount(<SpeechDictationPage />);
    const blocked = q("speech-blocked")!;
    expect(blocked.textContent).toContain("Bileşen eksik");
    expect(blocked.textContent).toContain("Konuşma tanıma bileşeni bu kurulumda bulunamadı");
    h.openMock.mockResolvedValue("C:\\a\\x.mp3");
    await click(q("speech-dropzone"));
    expect((q("speech-start") as HTMLButtonElement).disabled).toBe(true);
    expect(container.textContent).not.toMatch(/çevrimiçi|online|bulut|cloud/i);
  });

  it("model missing / CPU unsupported map to the right labels", async () => {
    configure({ ...READY, status: "MODEL_MISSING", message: "Model bulunamadı." });
    await mount(<SpeechDictationPage />);
    expect(q("speech-blocked")!.textContent).toContain("Bileşen eksik");
    await act(async () => root.unmount());
    root = createRoot(container);
    configure({ ...READY, status: "CPU_UNSUPPORTED", message: "İşlemci desteklenmiyor." });
    await mount(<SpeechDictationPage />);
    expect(q("speech-blocked")!.textContent).toContain("Henüz desteklenmiyor");
  });

  it("language options are Türkçe + Otomatik algıla by default, and only what the manifest declares", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    const sel = q("speech-language") as HTMLSelectElement;
    expect(sel.value).toBe("tr");
    expect([...sel.options].map((o) => o.textContent)).toEqual(["Türkçe", "Otomatik algıla"]);
    await act(async () => root.unmount());
    root = createRoot(container);
    configure({ ...READY, languages: ["tr"] });
    await mount(<SpeechDictationPage />);
    expect([...(q("speech-language") as HTMLSelectElement).options].map((o) => o.textContent)).toEqual(["Türkçe"]);
  });
});

describe("native picker lifecycle (no stacked dialogs)", () => {
  /** A picker that stays open until the test settles it, like the real native dialog. */
  function pendingPicker() {
    let settle!: (v: string | null) => void;
    let fail!: (e: unknown) => void;
    h.openMock.mockImplementation(
      () =>
        new Promise<string | null>((res, rej) => {
          settle = res;
          fail = rej;
        })
    );
    return { settle: (v: string | null) => act(async () => settle(v)), fail: (e: unknown) => act(async () => fail(e)) };
  }
  const burst = (el: HTMLElement | null, n: number) =>
    act(async () => {
      for (let i = 0; i < n; i++) el!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

  it("repeated clicks while the picker is open start exactly one native dialog and show a busy state", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    const picker = pendingPicker();
    await burst(q("speech-dropzone"), 3); // same tick: state alone would not catch this
    await flush();
    expect(h.openMock).toHaveBeenCalledTimes(1);
    const dz = q("speech-dropzone") as HTMLButtonElement;
    expect(dz.disabled).toBe(true);
    expect(dz.getAttribute("aria-busy")).toBe("true");
    expect(dz.textContent).toContain("Dosya seçici açık");

    await picker.settle(null); // user cancels the dialog
    await flush();
    const after = q("speech-dropzone") as HTMLButtonElement;
    expect(after.disabled).toBe(false);
    expect(after.getAttribute("aria-busy")).toBe("false");
    expect(after.textContent).toContain("Ses dosyasını buraya sürükleyin");
    expect(q("speech-file-card")).toBeNull();

    // released for real: a later click opens a new dialog
    pendingPicker();
    await click(q("speech-dropzone"));
    expect(h.openMock).toHaveBeenCalledTimes(2);
  });

  it("a picker that throws releases the guard and reports the Turkish error", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    const picker = pendingPicker();
    await click(q("speech-dropzone"));
    expect((q("speech-dropzone") as HTMLButtonElement).disabled).toBe(true);
    await picker.fail(new Error("boom"));
    await flush();
    expect((q("speech-dropzone") as HTMLButtonElement).disabled).toBe(false);
    const toast = (await import("react-hot-toast")).default;
    expect(toast.error).toHaveBeenCalledWith("Dosya seçilemedi");
  });

  it("choosing a file through the picker closes the busy state and shows the file", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    const picker = pendingPicker();
    await click(q("speech-dropzone"));
    await picker.settle("C:\\a\\toplanti.mp3");
    await flush();
    expect(q("speech-file-card")!.textContent).toContain("toplanti.mp3");
    expect(h.invokeMock).toHaveBeenCalledWith("speech_inspect_file", { path: "C:\\a\\toplanti.mp3" });
  });

  it("'Başka Dosya Seç' is guarded the same way", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    h.openMock.mockResolvedValueOnce("C:\\a\\toplanti.mp3");
    await click(q("speech-dropzone"));
    expect(h.openMock).toHaveBeenCalledTimes(1);
    const picker = pendingPicker();
    await burst(q("speech-change-file"), 3);
    await flush();
    expect(h.openMock).toHaveBeenCalledTimes(2);
    expect((q("speech-change-file") as HTMLButtonElement).disabled).toBe(true);
    await picker.settle(null);
    await flush();
    expect((q("speech-change-file") as HTMLButtonElement).disabled).toBe(false);
    expect(q("speech-file-card")!.textContent).toContain("toplanti.mp3"); // cancelling keeps the current file
  });

  it("asks its own window for focus before opening the dialog, and still opens if focus is refused", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    h.setFocusMock.mockImplementation(() => {
      h.callOrder.push("setFocus");
      return Promise.resolve();
    });
    h.openMock.mockImplementation(() => {
      h.callOrder.push("open");
      return Promise.resolve(null);
    });
    await click(q("speech-dropzone"));
    expect(h.callOrder).toEqual(["setFocus", "open"]);

    h.setFocusMock.mockImplementation(() => Promise.reject(new Error("refused")));
    await click(q("speech-dropzone"));
    expect(h.openMock).toHaveBeenCalledTimes(2);
  });
});

describe("'Ses Dosyası Yükle' button shares the dropzone's picker", () => {
  const burst = (els: (HTMLElement | null)[], n: number) =>
    act(async () => {
      for (let i = 0; i < n; i++) els.forEach((el) => el!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    });

  it("empty → click 'Ses Dosyası Yükle' → the picker opens (this button used to have no handler)", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    h.openMock.mockResolvedValue(null);
    await click(q("tab-file"));
    expect(h.openMock).toHaveBeenCalledTimes(1);
    const opts = h.openMock.mock.calls[0][0] as { multiple: boolean; directory: boolean; filters: { extensions: string[] }[] };
    expect(opts.multiple).toBe(false);
    expect(opts.directory).toBe(false);
    expect(opts.filters[0].extensions).toEqual(["wav", "mp3", "m4a", "aac", "flac", "ogg"]);
  });

  it("empty → click the central dropzone → the picker opens, with the same options", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    h.openMock.mockResolvedValue(null);
    await click(q("tab-file"));
    await click(q("speech-dropzone"));
    expect(h.openMock).toHaveBeenCalledTimes(2);
    expect(h.openMock.mock.calls[1][0]).toEqual(h.openMock.mock.calls[0][0]);
  });

  it("selecting through the button shows the file", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    h.openMock.mockResolvedValue("C:\\a\\toplanti.mp3");
    await click(q("tab-file"));
    expect(q("speech-file-card")!.textContent).toContain("toplanti.mp3");
    expect(q("speech-dropzone")).toBeNull();
  });

  it("button and dropzone share one guard: mixed rapid clicks open a single dialog, both show busy", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    let settle!: (v: string | null) => void;
    h.openMock.mockImplementation(() => new Promise<string | null>((res) => (settle = res)));
    await burst([q("tab-file"), q("speech-dropzone")], 3);
    await flush();
    expect(h.openMock).toHaveBeenCalledTimes(1);
    expect((q("tab-file") as HTMLButtonElement).disabled).toBe(true);
    expect((q("speech-dropzone") as HTMLButtonElement).disabled).toBe(true);
    expect(q("tab-file")!.getAttribute("aria-busy")).toBe("true");
    await act(async () => settle(null)); // cancel
    await flush();
    expect((q("tab-file") as HTMLButtonElement).disabled).toBe(false);
    expect((q("speech-dropzone") as HTMLButtonElement).disabled).toBe(false);
    h.openMock.mockResolvedValue(null);
    await click(q("tab-file"));
    expect(h.openMock).toHaveBeenCalledTimes(2);
  });

  it("the tab cannot start a picker while a job runs", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "transcribing" });
    expect((q("tab-file") as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("remove file ('Dosyayı kaldır')", () => {
  async function selectFile(path = "C:\\a\\toplanti.mp3") {
    h.openMock.mockResolvedValue(path);
    await click(q("speech-dropzone"));
  }
  function expectEmpty() {
    expect(q("speech-file-card")).toBeNull();
    expect(q("speech-remove-file")).toBeNull();
    expect(q("speech-dropzone")).toBeTruthy();
    expect(q("speech-state")).toBeNull();
    expect(q("speech-progress")).toBeNull();
    expect(q("speech-error")).toBeNull();
    expect(q("speech-cancelled")).toBeNull();
    expect(q("speech-completed")).toBeNull();
    expect((q("speech-transcript") as HTMLTextAreaElement).value).toBe("");
    expect((q("speech-start") as HTMLButtonElement).disabled).toBe(true);
  }

  it("the remove button is only shown while a file is loaded, with the Turkish label", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    expect(q("speech-remove-file")).toBeNull();
    await selectFile();
    expect(q("speech-remove-file")!.textContent).toBe("Dosyayı kaldır");
  });

  it("empty → file selected → ready → transcribing → completed", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    expect(q("speech-dropzone")).toBeTruthy();
    await selectFile();
    expect(q("speech-state")!.textContent).toBe("Hazır");
    await click(q("speech-start"));
    await update({ state: "transcribing", elapsed_secs: 2 });
    expect(q("speech-state")!.textContent).toBe("Dikte ediliyor");
    await update({ state: "completed", elapsed_secs: 5, progress_pct: 100, transcript: TRANSCRIPT });
    expect(q("speech-state")!.textContent).toBe("Tamamlandı");
  });

  it("file selected → remove → empty, and another file can be chosen immediately", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFile();
    await click(q("speech-remove-file"));
    expectEmpty();
    expect((q("speech-dropzone") as HTMLButtonElement).disabled).toBe(false);
    await selectFile("C:\\a\\ikinci.mp3");
    expect(h.openMock).toHaveBeenCalledTimes(2);
    expect(q("speech-file-card")).toBeTruthy();
  });

  it("file selected → replace with another file → the new file is shown", async () => {
    configure(READY, {
      speech_inspect_file: (a) => {
        const p = (a as { path: string }).path;
        return { ...INFO, file_name: p.split("\\").pop() };
      },
    });
    await mount(<SpeechDictationPage />);
    await selectFile("C:\\a\\birinci.mp3");
    expect(q("speech-file-card")!.textContent).toContain("birinci.mp3");
    h.openMock.mockResolvedValue("C:\\a\\ikinci.wav");
    await click(q("speech-change-file"));
    expect(q("speech-file-card")!.textContent).toContain("ikinci.wav");
    expect(q("speech-file-card")!.textContent).not.toContain("birinci.mp3");
  });

  it("transcribing → remove → the job is cancelled → empty, and late events from it are ignored", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "transcribing", elapsed_secs: 4, progress_pct: 30 });
    expect(q("speech-progress")).toBeTruthy();
    await click(q("speech-remove-file"));
    expect(h.invokeMock).toHaveBeenCalledWith("speech_cancel_job", { jobId: "job-1" });
    expectEmpty();
    // the backend keeps talking for a moment: none of it may resurface
    await update({ state: "transcribing", elapsed_secs: 5, progress_pct: 40 });
    await update({ state: "completed", transcript: TRANSCRIPT });
    await update({ state: "cancelled", error: { code: "SPEECH_CANCELLED", message: "Dikte işlemi iptal edildi." } });
    expectEmpty();
    // a fresh file + job still works afterwards
    await selectFile("C:\\a\\yeni.mp3");
    expect(q("speech-state")!.textContent).toBe("Hazır");
  });

  async function startWithPendingHandshake() {
    let release!: (id: string) => void;
    let fail!: (e: unknown) => void;
    configure(READY, {
      speech_start_file_job: () =>
        new Promise<string>((res, rej) => {
          release = res;
          fail = rej;
        }) as unknown as string,
    });
    await mount(<SpeechDictationPage />);
    h.openMock.mockResolvedValue("C:\\a\\toplanti.mp3");
    await click(q("speech-dropzone"));
    await click(q("speech-start")); // start_file_job has not returned its id yet
    return { release: (id: string) => act(async () => release(id)), fail: (e: unknown) => act(async () => fail(e)) };
  }
  const cancelCalls = () => h.invokeMock.mock.calls.filter((c) => c[0] === "speech_cancel_job");

  it("removed while starting, before any event: cancelled once the id is known, never shown", async () => {
    const hs = await startWithPendingHandshake();
    await click(q("speech-remove-file")); // no job id exists yet
    expectEmpty();
    expect(cancelCalls()).toHaveLength(0); // nothing to cancel *yet*
    await hs.release("job-1");
    await flush();
    expect(cancelCalls().map((c) => c[1])).toEqual([{ jobId: "job-1" }]);
    await update({ state: "transcribing" });
    await update({ state: "completed", transcript: TRANSCRIPT });
    expectEmpty();
  });

  it("removed while starting, after an early event: cancelled immediately, never shown", async () => {
    const hs = await startWithPendingHandshake();
    await update({ state: "preparing" }); // event arrives before the invoke returns
    await click(q("speech-remove-file"));
    expectEmpty();
    expect(cancelCalls().map((c) => c[1])).toEqual([{ jobId: "job-1" }]);
    await hs.release("job-1");
    await flush();
    await update({ state: "transcribing" });
    expectEmpty();
  });

  it("removed while starting, and the start then fails: stays empty (no error painted onto the cleared page)", async () => {
    const hs = await startWithPendingHandshake();
    await click(q("speech-remove-file"));
    await hs.fail({ code: "SPEECH_TRANSCRIPTION_FAILED", message: "Dikte işlemi tamamlanamadı." });
    await flush();
    expectEmpty();
  });

  it("completed → remove → empty (transcript cleared)", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "completed", elapsed_secs: 5, transcript: TRANSCRIPT });
    expect((q("speech-transcript") as HTMLTextAreaElement).value).toContain("Merhaba");
    h.invokeMock.mockClear();
    await click(q("speech-remove-file"));
    expectEmpty();
    // nothing running, so nothing to cancel
    expect(h.invokeMock.mock.calls.some((c) => c[0] === "speech_cancel_job")).toBe(false);
  });

  it("error → remove → empty (error text cleared)", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({
      state: "failed",
      error: { code: "SPEECH_TRANSCRIPTION_FAILED", message: "Dikte işlemi tamamlanamadı. Lütfen yeniden deneyin." },
    });
    expect(q("speech-error")).toBeTruthy();
    await click(q("speech-remove-file"));
    expectEmpty();
  });

  it("cancelled → remove → empty", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "cancelled", error: { code: "SPEECH_CANCELLED", message: "Dikte işlemi iptal edildi." } });
    expect(q("speech-cancelled")).toBeTruthy();
    await click(q("speech-remove-file"));
    expectEmpty();
  });

  it("removing never asks the backend or the OS to delete the user's file", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "transcribing" });
    h.invokeMock.mockClear();
    await click(q("speech-remove-file"));
    const cmds = h.invokeMock.mock.calls.map((c) => c[0]);
    expect(cmds).toEqual(["speech_cancel_job"]);
    expect(cmds.join(" ")).not.toMatch(/delete|remove|unlink|trash|fs/i);
  });

  it("edited transcript: removal asks first, and declining keeps everything", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "completed", transcript: TRANSCRIPT });
    await act(async () => {
      const ta = q("speech-transcript") as HTMLTextAreaElement;
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(ta, "Benim düzeltmem");
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    });
    h.askMock.mockResolvedValue(false);
    await click(q("speech-remove-file"));
    expect(h.askMock).toHaveBeenCalledTimes(1);
    expect(q("speech-file-card")).toBeTruthy();
    expect((q("speech-transcript") as HTMLTextAreaElement).value).toBe("Benim düzeltmem");
    h.askMock.mockResolvedValue(true);
    await click(q("speech-remove-file"));
    expectEmpty();
  });

  it("cannot be removed while the picker is open", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFile();
    h.openMock.mockImplementation(() => new Promise(() => {}));
    await click(q("speech-change-file"));
    expect((q("speech-remove-file") as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("file selection", () => {
  it("unsupported / invalid audio: shows the backend's Turkish error and no file card", async () => {
    configure(READY, {
      speech_inspect_file: () => {
        throw { code: "SPEECH_INPUT_UNSUPPORTED", message: "Bu ses dosyası desteklenmiyor. Desteklenen biçimler: WAV, MP3, M4A, AAC, FLAC, OGG." };
      },
    });
    await mount(<SpeechDictationPage />);
    h.openMock.mockResolvedValue("C:\\a\\notes.txt");
    await click(q("speech-dropzone"));
    expect(q("speech-source-error")!.textContent).toContain("Desteklenen biçimler");
    expect(q("speech-file-card")).toBeNull();
  });

  it("a native drop selects the file (and only the first path)", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await act(async () => h.emit("tauri://drag-drop", { paths: ["C:\\a\\ilk.mp3", "C:\\a\\ikinci.mp3"] }));
    await flush();
    expect(h.invokeMock).toHaveBeenCalledWith("speech_inspect_file", { path: "C:\\a\\ilk.mp3" });
    expect(q("speech-file-card")).toBeTruthy();
  });

  it("the frontend never sends an executable, model path or engine name", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    const startCall = h.invokeMock.mock.calls.find((c) => c[0] === "speech_start_file_job")!;
    expect(Object.keys(startCall[1] as object).sort()).toEqual(["language", "path"]);
  });
});

describe("job lifecycle", () => {
  it("long-running: indeterminate progress + elapsed time, no fake percentage", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "transcribing", elapsed_secs: 138, progress_pct: null });
    expect(q("speech-progress-indeterminate")).toBeTruthy();
    expect(q("speech-progress-determinate")).toBeNull();
    expect(q("speech-progress")!.textContent).toContain("Dikte ediliyor");
    expect(q("speech-elapsed")!.textContent).toContain("Geçen süre: 02:18");
    expect(q("speech-elapsed")!.textContent).not.toContain("%");
    expect(q("speech-state")!.textContent).toBe("Dikte ediliyor");
  });

  it("real engine progress (when reported) is shown as-is", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "transcribing", elapsed_secs: 10, progress_pct: 42 });
    expect(q("speech-progress-determinate")).toBeTruthy();
    expect(q("speech-elapsed")!.textContent).toContain("%42");
  });

  it("preparing state label", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "preparing" });
    expect(q("speech-state")!.textContent).toBe("Ses hazırlanıyor");
  });

  it("success: transcript appears in an editable text area", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "completed", elapsed_secs: 5, progress_pct: 100, transcript: TRANSCRIPT });
    const ta = q("speech-transcript") as HTMLTextAreaElement;
    expect(ta.value).toBe("Merhaba. Nasılsınız?");
    expect(ta.readOnly).toBe(false);
    expect(q("speech-completed")).toBeTruthy();
    expect(q("speech-state")!.textContent).toBe("Tamamlandı");
    // editing keeps the user's text
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
      setter.call(ta, "Düzenlenmiş metin");
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect((q("speech-transcript") as HTMLTextAreaElement).value).toBe("Düzenlenmiş metin");
  });

  it("failure shows the Turkish message; retry reuses the original source and starts a fresh job", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({
      state: "failed",
      error: { code: "SPEECH_TRANSCRIPTION_FAILED", message: "Dikte işlemi tamamlanamadı. Lütfen yeniden deneyin." },
    });
    expect(q("speech-error")!.textContent).toContain("Dikte işlemi tamamlanamadı");
    expect(q("speech-state")!.textContent).toBe("Hata");
    h.invokeMock.mockClear();
    configure(READY, { speech_start_file_job: () => "job-2" });
    await click(q("speech-retry"));
    const start = h.invokeMock.mock.calls.find((c) => c[0] === "speech_start_file_job")!;
    expect(start[1]).toEqual({ path: "C:\\Users\\a\\toplanti.mp3", language: "tr" });
    // not poisoned: the new job's updates are accepted
    await act(async () =>
      h.emit("speech-job-update", { job_id: "job-2", state: "transcribing", elapsed_secs: 1, progress_pct: null, transcript: null, error: null })
    );
    expect(q("speech-state")!.textContent).toBe("Dikte ediliyor");
    expect(q("speech-error")).toBeNull();
  });

  it("cancel: sends the cancel command for the running job and shows İptal edildi", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "transcribing", elapsed_secs: 3 });
    await click(q("speech-cancel"));
    expect(h.invokeMock).toHaveBeenCalledWith("speech_cancel_job", { jobId: "job-1" });
    await update({ state: "cancelled", error: { code: "SPEECH_CANCELLED", message: "Dikte işlemi iptal edildi." } });
    expect(q("speech-state")!.textContent).toBe("İptal edildi");
    expect(q("speech-cancelled")).toBeTruthy();
    expect(q("speech-retry")).toBeTruthy();
  });

  it("no-speech is a distinct, friendly result (not a fake transcript)", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "failed", error: { code: "SPEECH_NO_SPEECH_DETECTED", message: "Konuşma algılanamadı." } });
    expect(q("speech-error")!.textContent).toContain("Konuşma algılanamadı.");
    expect((q("speech-transcript") as HTMLTextAreaElement).value).toBe("");
  });

  it("leaving the page while a job runs cancels it", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "transcribing", elapsed_secs: 2 });
    h.invokeMock.mockClear();
    await act(async () => root.unmount());
    expect(h.invokeMock).toHaveBeenCalledWith("speech_cancel_job", { jobId: "job-1" });
    root = createRoot(container);
  });

  it("unsubscribes from job events on unmount", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    expect(h.listenerCount("speech-job-update")).toBe(1);
    await act(async () => root.unmount());
    await flush();
    expect(h.listenerCount("speech-job-update")).toBe(0);
    root = createRoot(container);
  });
});

describe("timestamps: shown only when the engine provided them", () => {
  it("available: checkbox toggles the spec's [HH:MM:SS] format", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "completed", transcript: TRANSCRIPT });
    const cb = q("speech-timestamps") as HTMLInputElement;
    expect(cb).toBeTruthy();
    await click(cb);
    expect((q("speech-transcript") as HTMLTextAreaElement).value).toBe("[00:00:12] Merhaba.\n[00:00:24] Nasılsınız?");
  });

  it("unavailable: the option is not offered (never fabricated)", async () => {
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "completed", transcript: { ...TRANSCRIPT, timestamps_available: false } });
    expect(q("speech-timestamps")).toBeNull();
    expect((q("speech-transcript") as HTMLTextAreaElement).value).not.toContain("[00:");
  });

  it("toggling after edits asks before discarding them", async () => {
    configure();
    h.askMock.mockResolvedValue(false);
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "completed", transcript: TRANSCRIPT });
    const ta = q("speech-transcript") as HTMLTextAreaElement;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(ta, "benim düzenlemem");
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(q("speech-timestamps"));
    expect(h.askMock).toHaveBeenCalled();
    expect((q("speech-transcript") as HTMLTextAreaElement).value).toBe("benim düzenlemem");
  });
});

describe("text actions", () => {
  it("clear empties the text; copy writes it to the clipboard", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    configure();
    await mount(<SpeechDictationPage />);
    await selectFileAndStart();
    await update({ state: "completed", transcript: TRANSCRIPT });
    await click(q("speech-copy"));
    expect(writeText).toHaveBeenCalledWith("Merhaba. Nasılsınız?");
    await click(q("speech-clear"));
    expect((q("speech-transcript") as HTMLTextAreaElement).value).toBe("");
  });
});

describe("CPU performance heads-up", () => {
  it("shows the backend's note (never a blocker) when the CPU lacks fast instruction sets", async () => {
    configure({ ...READY, performance_note: "Bu bilgisayarın işlemcisi dikte için en hızlı komut kümelerini desteklemiyor; işlem normalden yavaş olabilir." });
    await mount(<SpeechDictationPage />);
    expect(q("speech-performance-note")!.textContent).toContain("yavaş olabilir");
    expect(q("speech-blocked")).toBeNull();
    h.openMock.mockResolvedValue("C:/a/x.mp3");
    await click(q("speech-dropzone"));
    expect((q("speech-start") as HTMLButtonElement).disabled).toBe(false);
  });

  it("shows nothing on a fast CPU", async () => {
    configure({ ...READY, performance_note: null });
    await mount(<SpeechDictationPage />);
    expect(q("speech-performance-note")).toBeNull();
  });
});
