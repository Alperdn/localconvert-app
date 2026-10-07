import { useEffect, useCallback } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import toast from "react-hot-toast";
import { useStore, FileInfo, getFilesToConvert } from "../store/useStore";
import { getBlockedCapability } from "../utils/capabilityGating";
import { describeUnavailableCapability } from "../locales";
import { IS_WEB_RUNTIME } from "../platform/runtime";
import { pickAndAddBrowserFiles } from "../platform/webFiles";

export function useKeyboardShortcuts() {
  const {
    files,
    selectedFiles,
    activeCategory,
    selectAllFiles,
    deselectAllFiles,
    removeFile,
    clearFiles,
    convertFiles,
    isConverting,
    cancelConversion,
    addFiles,
    globalOutputFormat,
    getCapability,
  } = useStore();

  const handleOpenFiles = useCallback(async () => {
    if (IS_WEB_RUNTIME) {
      await pickAndAddBrowserFiles();
      return;
    }
    try {
      const selected = await open({
        multiple: true,
        filters: [
          {
            name: "All Supported Files",
            extensions: [
              // Video
              "mp4", "webm", "mov", "avi", "mkv", "wmv", "flv", "mpeg", "3gp",
              // Audio
              "mp3", "wav", "flac", "aac", "ogg", "wma", "m4a", "aiff", "opus",
              // Image
              "png", "jpg", "jpeg", "webp", "avif", "gif", "bmp", "tiff", "ico", "svg", "heic",
              // Document
              "pdf", "docx", "doc", "txt", "rtf", "odt", "html", "md", "epub",
              // Archive
              "zip", "7z", "rar", "tar", "gz",
            ],
          },
        ],
      });

      if (selected) {
        const paths = Array.isArray(selected) ? selected : [selected];
        const fileInfos: FileInfo[] = [];

        for (const path of paths) {
          try {
            const info = await invoke<FileInfo>("get_file_info", { path });
            fileInfos.push(info);
          } catch (error) {
            console.error(`Failed to get info for ${path}:`, error);
          }
        }

        if (fileInfos.length > 0) {
          addFiles(fileInfos);
        }
      }
    } catch (error) {
      console.error("Failed to open files:", error);
    }
  }, [addFiles]);

  const handleKeyDown = useCallback(
    (event: KeyboardEvent) => {
      // These are the CONVERTER's shortcuts (open files into the conversion list,
      // Enter = start conversion, Delete = remove queued files, ...). They must be
      // inert on the Ses Dikte page, or Enter/Ctrl+O there would act on files the
      // user cannot even see.
      if (useStore.getState().activeView === "dictation") return;

      // Don't trigger shortcuts when typing in inputs
      const target = event.target as HTMLElement;
      if (
        target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.isContentEditable
      ) {
        return;
      }

      const isMod = event.ctrlKey || event.metaKey;

      // Ctrl/Cmd + O: Open files
      if (isMod && event.key === "o") {
        event.preventDefault();
        handleOpenFiles();
        return;
      }

      // Ctrl/Cmd + A: Select all files
      if (isMod && event.key === "a" && files.length > 0) {
        event.preventDefault();
        selectAllFiles();
        return;
      }

      // Escape: Deselect all or cancel conversion
      if (event.key === "Escape") {
        event.preventDefault();
        if (isConverting) {
          cancelConversion();
        } else if (selectedFiles.length > 0) {
          deselectAllFiles();
        }
        return;
      }

      // Delete/Backspace: Remove selected files
      if ((event.key === "Delete" || event.key === "Backspace") && selectedFiles.length > 0) {
        event.preventDefault();
        selectedFiles.forEach((id) => removeFile(id));
        return;
      }

      // Ctrl/Cmd + Shift + Delete: Clear all files
      if (isMod && event.shiftKey && event.key === "Delete") {
        event.preventDefault();
        clearFiles();
        return;
      }

      // Enter: Start conversion
      if (event.key === "Enter" && !isConverting && files.length > 0) {
        const filesToConvert = getFilesToConvert({ files, selectedFiles, activeCategory });

        // Same pre-execution capability gate as the Convert button (Step 3,
        // section E) - this shortcut used to call convertFiles() straight
        // away with no capability check at all, so pressing Enter with an
        // Office file queued and no Office engine available would spawn a
        // real conversion process and only fail afterward.
        const blockedCapability = getBlockedCapability(filesToConvert, getCapability);
        if (blockedCapability) {
          event.preventDefault();
          toast.error(describeUnavailableCapability(blockedCapability));
          return;
        }

        const canConvert = filesToConvert.every(
          (f) => f.outputFormat || globalOutputFormat
        );

        if (canConvert && filesToConvert.length > 0) {
          event.preventDefault();
          convertFiles();
        }
        return;
      }

      // Ctrl/Cmd + Shift + A: Deselect all
      if (isMod && event.shiftKey && event.key === "A") {
        event.preventDefault();
        deselectAllFiles();
        return;
      }
    },
    [
      files,
      selectedFiles,
      activeCategory,
      selectAllFiles,
      deselectAllFiles,
      removeFile,
      clearFiles,
      convertFiles,
      isConverting,
      cancelConversion,
      handleOpenFiles,
      globalOutputFormat,
      getCapability,
    ]
  );

  useEffect(() => {
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [handleKeyDown]);

  return { handleOpenFiles };
}

// Keyboard shortcut hints for display
export const KEYBOARD_SHORTCUTS = [
  { keys: ["Ctrl", "O"], description: "Open files" },
  { keys: ["Ctrl", "A"], description: "Select all" },
  { keys: ["Ctrl", "Shift", "A"], description: "Deselect all" },
  { keys: ["Delete"], description: "Remove selected" },
  { keys: ["Enter"], description: "Start conversion" },
  { keys: ["Escape"], description: "Cancel / Deselect" },
];
