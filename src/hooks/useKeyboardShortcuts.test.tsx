// @vitest-environment jsdom
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react";

const h = vi.hoisted(() => ({ openMock: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: (...a: unknown[]) => h.openMock(...a) }));
vi.mock("react-hot-toast", () => ({ default: { success: vi.fn(), error: vi.fn() } }));

import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { useStore } from "../store/useStore";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function Harness() {
  useKeyboardShortcuts();
  return null;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(async () => {
  h.openMock.mockReset();
  h.openMock.mockResolvedValue(null);
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => root.render(<Harness />));
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  useStore.setState({ activeView: "convert" });
});

function press(key: string, opts: KeyboardEventInit = {}) {
  return act(async () => {
    document.body.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, ...opts }));
  });
}

describe("converter keyboard shortcuts vs. Ses Dikte", () => {
  it("Ctrl+O opens the converter's file dialog on the conversion view", async () => {
    useStore.setState({ activeView: "convert" });
    await press("o", { ctrlKey: true });
    expect(h.openMock).toHaveBeenCalledTimes(1);
  });

  it("Ctrl+O does NOT open the converter's dialog while on Ses Dikte", async () => {
    useStore.setState({ activeView: "dictation" });
    await press("o", { ctrlKey: true });
    expect(h.openMock).not.toHaveBeenCalled();
  });

  it("Enter never starts a conversion from the Ses Dikte page", async () => {
    const convertFiles = vi.fn();
    useStore.setState({
      activeView: "dictation",
      convertFiles,
      files: [{ id: "a", path: "C:/x.mp4", name: "x.mp4", extension: "mp4", size: 1, category: "video", status: "pending", progress: 0, outputFormat: "mp3", outputPath: null, error: null, etaSecs: null, speed: null, previewUrl: null, previewLoading: false }],
    });
    await press("Enter");
    expect(convertFiles).not.toHaveBeenCalled();
    useStore.setState({ files: [] });
  });

  it("Delete/Backspace never removes queued converter files from the Ses Dikte page", async () => {
    const removeFile = vi.fn();
    useStore.setState({ activeView: "dictation", selectedFiles: ["a"], removeFile });
    await press("Delete");
    await press("Backspace");
    expect(removeFile).not.toHaveBeenCalled();
    useStore.setState({ selectedFiles: [] });
  });
});
