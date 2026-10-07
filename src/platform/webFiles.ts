// Web runtime: adds browser-selected/dropped files to the converter list.
// Shared by the drop zone, the file list's "add" button, the window-level
// drop handler and the Ctrl+O shortcut, so all four behave identically.

import { useStore } from "../store/useStore";
import { pickBrowserFiles, registerBrowserFile } from "../api/web";

export function addBrowserFiles(files: File[]): number {
  if (files.length === 0) return 0;
  useStore.getState().addFiles(files.map(registerBrowserFile));
  return files.length;
}

export async function pickAndAddBrowserFiles(): Promise<number> {
  return addBrowserFiles(await pickBrowserFiles());
}
