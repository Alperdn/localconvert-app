// Browser-side file selection for the web runtime.
//
// The store's `FileInfo.path` is a filesystem path on desktop. In the web
// runtime there is no path: the browser holds a `File` object. We keep the
// `File` in this module-level registry (outside Zustand, so the store stays
// plain data) and give the store an opaque local key instead - `local:<id>`
// - which is never sent to the server. The server only ever sees the bytes
// (upload) and returns its own file id.

import { getCategoryForFormat } from "../../types/formats";
import type { FileInfo } from "../../store/useStore";

const LOCAL_PREFIX = "local:";
const files = new Map<string, File>();
const previewUrls = new Map<string, string>();

function newLocalKey(): string {
  const random =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : Math.random().toString(36).slice(2);
  return `${LOCAL_PREFIX}${random}`;
}

export function isLocalKey(path: string): boolean {
  return path.startsWith(LOCAL_PREFIX);
}

export function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 && dot < name.length - 1 ? name.slice(dot + 1).toLowerCase() : "";
}

/// Registers a browser `File` and describes it the way the store expects.
export function registerBrowserFile(file: File): FileInfo {
  const key = newLocalKey();
  files.set(key, file);
  const extension = extensionOf(file.name);
  return {
    path: key,
    name: file.name,
    extension,
    size: file.size,
    category: getCategoryForFormat(extension),
  };
}

export function getLocalFile(key: string): File | undefined {
  return files.get(key);
}

/// Object URL for an in-browser preview - the image never leaves the
/// browser just to be previewed.
export function getLocalPreviewUrl(key: string): string | null {
  const existing = previewUrls.get(key);
  if (existing) return existing;
  const file = files.get(key);
  if (!file || typeof URL === "undefined" || !URL.createObjectURL) return null;
  const url = URL.createObjectURL(file);
  previewUrls.set(key, url);
  return url;
}

export function forgetLocalFile(key: string): void {
  const url = previewUrls.get(key);
  if (url && typeof URL !== "undefined" && URL.revokeObjectURL) URL.revokeObjectURL(url);
  previewUrls.delete(key);
  files.delete(key);
}

/// Opens the browser's native file chooser. Must be called from a user
/// gesture (click / keydown). Resolves with the chosen files (possibly none).
export function pickBrowserFiles(): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    input.style.display = "none";
    input.addEventListener(
      "change",
      () => {
        resolve(input.files ? Array.from(input.files) : []);
        input.remove();
      },
      { once: true }
    );
    input.addEventListener("cancel", () => {
      resolve([]);
      input.remove();
    });
    document.body.appendChild(input);
    input.click();
  });
}

/// Files from a drag-and-drop event (directories are skipped: a dropped
/// folder shows up as a zero-type entry the browser cannot read as a file).
export function filesFromDataTransfer(dataTransfer: DataTransfer | null): File[] {
  if (!dataTransfer) return [];
  return Array.from(dataTransfer.files).filter((f) => f.size > 0 || f.type !== "");
}
