import { useEffect, useState, useRef, lazy, Suspense } from "react";
import { Toaster } from "react-hot-toast";
import { AnimatePresence, motion } from "framer-motion";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

import { useShallow } from "zustand/react/shallow";
import { useStore } from "./store/useStore";
import type { FileInfo } from "./store/useStore";
import { t, translateErrorCode } from "./locales";
import { Header } from "./components/Header";
import { Sidebar } from "./components/Sidebar";
import { FileDropZone } from "./components/FileDropZone";
import { FileList } from "./components/FileList";
import { ConversionPanel } from "./components/ConversionPanel";
import { SettingsModal } from "./components/SettingsModal";
import { SystemStatusModal } from "./components/SystemStatusModal";
import { ToolsSetupModal } from "./components/ToolsSetupModal";
import { ImagePreviewModal } from "./components/ImagePreviewModal";
import { VideoTrimmer } from "./components/VideoTrimmer";
import { useKeyboardShortcuts } from "./hooks/useKeyboardShortcuts";
import { playCompletionSound } from "./utils/sounds";

// Step 3 perf pass (section F): PdfEditor was a static import, so its
// dependency graph (pdf-lib, pdfjs-dist, fabric - ~700KB gzipped combined,
// per the `npm run build` chunk report) was fetched and parsed as part of
// the app's initial load even though the editor only opens from an
// explicit "Edit PDF" click and is otherwise not on the startup path.
// `lazy()` defers that fetch until `pdfEditorFile` is actually set.
const PdfEditor = lazy(() =>
  import("./components/PdfEditor").then((m) => ({ default: m.PdfEditor }))
);

interface DragDropPayload {
  paths: string[];
  position: { x: number; y: number };
}

function App() {
  // Step 3 perf pass (section D): narrowed from a full `useStore()` - App
  // wraps everything, and its children that take no props (Sidebar,
  // FileList, ConversionPanel) are now `React.memo`'d, so keeping App's
  // own re-render frequency down actually matters for them again (a memo'd
  // child still gets re-invoked if its memo-less parent hands it new JSX
  // for an unrelated reason on every render... in this app's case it
  // doesn't, since those three take zero props, but narrowing here still
  // avoids re-running App's own body - the drag-drop listener setup,
  // startup-file loading, the completion-sound effect, and the dropzone
  // vs. filelist AnimatePresence swap - for state changes App doesn't
  // actually care about, e.g. a settings field it never reads).
  const {
    files,
    checkTools,
    theme,
    playCompletionSound: playCompletionSoundSetting,
    addFiles,
    isConverting,
    pdfEditorFile,
    closePdfEditor,
    videoTrimmerFile,
    closeVideoTrimmer,
    loadCapabilities,
    detectGpu,
  } = useStore(
    useShallow((s) => ({
      files: s.files,
      checkTools: s.checkTools,
      theme: s.settings.theme,
      playCompletionSound: s.settings.playCompletionSound,
      addFiles: s.addFiles,
      isConverting: s.isConverting,
      pdfEditorFile: s.pdfEditorFile,
      closePdfEditor: s.closePdfEditor,
      videoTrimmerFile: s.videoTrimmerFile,
      closeVideoTrimmer: s.closeVideoTrimmer,
      loadCapabilities: s.loadCapabilities,
      detectGpu: s.detectGpu,
    }))
  );
  const [showSettings, setShowSettings] = useState(false);
  const [showSystemStatus, setShowSystemStatus] = useState(false);
  const [showTools, setShowTools] = useState(false);
  const prevConvertingRef = useRef(isConverting);

  // Enable keyboard shortcuts
  useKeyboardShortcuts();

  // Play sound when conversions complete
  useEffect(() => {
    const wasConverting = prevConvertingRef.current;
    prevConvertingRef.current = isConverting;

    // If we just finished converting and sound is enabled
    if (wasConverting && !isConverting && playCompletionSoundSetting) {
      const completedFiles = files.filter((f) => f.status === "completed");
      if (completedFiles.length > 0) {
        playCompletionSound();
      }
    }
  }, [isConverting, files, playCompletionSoundSetting]);

  // Theme (including "system" OS-preference resolution and live updates)
  // is applied to <html class="dark"> centrally in useStore.ts, where the
  // resolved value is computed - this component just reads the already-
  // resolved settings.theme below.

  // Listen for Tauri native drag-drop events
  useEffect(() => {
    const unlistenDrop = listen<DragDropPayload>("tauri://drag-drop", async (event) => {
      const paths = event.payload.paths;
      if (paths && paths.length > 0) {
        // Process dropped files
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
    });

    return () => {
      unlistenDrop.then((unlisten) => unlisten());
    };
  }, [addFiles]);

  useEffect(() => {
    // Check tools on first load
    checkTools().then(() => {
      // After checking tools, determine if this is first run
      const hasRunBefore = localStorage.getItem("localconvert_has_run");
      if (!hasRunBefore) {
        setShowTools(true);
        localStorage.setItem("localconvert_has_run", "true");
      }
    });
    // Detect GPU encoders
    detectGpu();

    // Fetch backend-computed feature capabilities (see
    // src-tauri/src/capabilities.rs) once at startup, so the UI can gate
    // unavailable features (Office/video/OCR/...) before the user ever
    // tries them, instead of only discovering it after a failed
    // conversion attempt.
    loadCapabilities();

    // NOTE (Phase 1 - Secure Desktop Foundation):
    // Automatic update checking/downloading/installing has been intentionally
    // removed. This app must not make network requests during normal startup,
    // and must never silently download+install+relaunch without explicit user
    // action. The updater plugin itself has been removed from the Rust side
    // (lib.rs / Cargo.toml) and from tauri.conf.json, so there is no code path
    // left that can perform this even by accident. If update checking is
    // reintroduced later, it must be an explicit, user-initiated action (e.g.
    // a "Check for updates" button) with a confirmation step before install.

    // Check for startup files (from context menu or drag-drop to app icon)
    const loadStartupFiles = async () => {
      try {
        const startupPaths = await invoke<string[]>("get_startup_files");
        if (startupPaths && startupPaths.length > 0) {
          const fileInfos: FileInfo[] = [];
          for (const path of startupPaths) {
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
        console.error("Failed to load startup files:", error);
      }
    };
    loadStartupFiles();
  }, [checkTools, detectGpu, addFiles, loadCapabilities]);

  const isDark = theme === "dark";

  return (
    <div className={`h-screen w-screen overflow-hidden flex flex-col transition-colors duration-500 ${
      isDark ? "bg-dark-gradient text-dark-100" : "bg-light-gradient text-dark-900"
    }`}>
      {/* Decorative background blur blobs. These sit behind every
          `glass-panel`/`glass-panel-heavy` element (Sidebar, FileList,
          ConversionPanel, Header), which all use `backdrop-filter` - that
          forces the browser to resample whatever is visually behind them
          on every frame the backdrop actually changes. An infinite opacity
          pulse under a large `filter: blur()` is exactly that: continuous,
          full-screen, forever, even with the app otherwise idle. That
          backdrop-filter recompute cost is very likely the dominant cause
          of the reported idle sluggishness/hover lag (Step 3 perf pass,
          section F) - kept the animation (still ambient/soft), but roughly
          halved the blur radius (blur cost scales with radius^2) and
          slowed the cycle to reduce how often that recompute happens. */}
      <div className="absolute top-0 left-0 w-full h-full overflow-hidden pointer-events-none z-0">
        <div
          className={`absolute top-[-10%] left-[-10%] w-[40%] h-[40%] rounded-full mix-blend-screen filter blur-[56px] opacity-30 animate-pulse-slow ${isDark ? 'bg-accent-900' : 'bg-accent-100'}`}
          style={{ animationDuration: "6s" }}
        ></div>
        <div
          className={`absolute bottom-[-10%] right-[-10%] w-[50%] h-[50%] rounded-full mix-blend-screen filter blur-[64px] opacity-20 animate-pulse-slow ${isDark ? 'bg-dark-700' : 'bg-dark-200'}`}
          style={{ animationDuration: "6s", animationDelay: '2s' }}
        ></div>
      </div>

      <div className="relative z-10 flex flex-col h-full w-full">
        <Toaster
          position="bottom-right"
          toastOptions={{
            className: isDark 
              ? "!bg-dark-800 !text-white !border !border-dark-600 !shadow-2xl !shadow-black/50 !backdrop-blur-md"
              : "!bg-white/90 !text-dark-900 !border !border-dark-200 !shadow-xl !backdrop-blur-md",
            duration: 4000,
            style: isDark ? {
              background: "rgba(16, 42, 70, 0.85)",
              color: "#fff",
              border: "1px solid rgba(255, 255, 255, 0.1)",
            } : {
              background: "rgba(255, 255, 255, 0.9)",
              color: "#07182B",
              border: "1px solid rgba(7, 24, 43, 0.1)",
            },
            success: {
              iconTheme: {
                primary: "#22c55e",
                secondary: "#fff",
              },
            },
            error: {
              iconTheme: {
                primary: "#ef4444",
                secondary: "#fff",
              },
            },
          }}
        />

        {/* Header */}
        <div className="px-4 pt-4 shrink-0">
          <Header
            onSettingsClick={() => setShowSettings(true)}
            onPrivacyClick={() => setShowSystemStatus(true)}
            onToolsClick={() => setShowTools(true)}
            onHelpClick={() => setShowTools(true)}
          />
        </div>

        {/* Main Content */}
        <div className="flex-1 flex overflow-hidden p-4 gap-4">
          {/* Sidebar */}
          <div className="shrink-0">
            <Sidebar />
          </div>

          {/* Main Area */}
          <main className="flex-1 flex flex-col overflow-hidden glass-panel rounded-2xl border-0 shadow-xl relative animate-fadeIn group">
            <AnimatePresence mode="wait">
              {files.length === 0 ? (
                <motion.div
                  key="dropzone"
                  initial={{ opacity: 0, scale: 0.98 }}
                  animate={{ opacity: 1, scale: 1 }}
                  exit={{ opacity: 0, scale: 0.98 }}
                  transition={{ duration: 0.3, ease: "easeOut" }}
                  className="absolute inset-0 flex items-center justify-center p-6"
                >
                  <FileDropZone />
                </motion.div>
              ) : (
                <motion.div
                  key="filelist"
                  initial={{ opacity: 0, y: 10 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={{ opacity: 0, y: -10 }}
                  transition={{ duration: 0.3, ease: "easeOut" }}
                  className="absolute inset-0 flex flex-col overflow-hidden p-6"
                >
                  <div className="flex-1 overflow-hidden flex gap-6">
                    {/* File List */}
                    <div className="flex-1 overflow-hidden">
                      <FileList />
                    </div>

                    {/* Conversion Panel */}
                    <div className="w-80 flex-shrink-0 flex flex-col h-full rounded-xl">
                      <ConversionPanel />
                    </div>
                  </div>
                </motion.div>
              )}
            </AnimatePresence>
          </main>
        </div>
      </div>

      {/* Modals */}
      <AnimatePresence>
        {showSettings && (
          <SettingsModal onClose={() => setShowSettings(false)} />
        )}
        {showSystemStatus && (
          <SystemStatusModal onClose={() => setShowSystemStatus(false)} isDark={isDark} />
        )}
        {showTools && <ToolsSetupModal onClose={() => setShowTools(false)} />}
      </AnimatePresence>

      {/* Image Preview Modal - manages its own visibility via store */}
      <ImagePreviewModal />

      {/* PDF Editor Modal - lazy-loaded, see the PdfEditor import above */}
      <AnimatePresence>
        {pdfEditorFile && (
          <Suspense fallback={null}>
            <PdfEditor
              filePath={pdfEditorFile.path}
              fileName={pdfEditorFile.name}
              onClose={closePdfEditor}
              isDark={isDark}
            />
          </Suspense>
        )}
      </AnimatePresence>

      {/* Video Trimmer Modal */}
      <AnimatePresence>
        {videoTrimmerFile && (
          <VideoTrimmer
            filePath={videoTrimmerFile.path}
            onClose={closeVideoTrimmer}
            onTrim={async (startTime, endTime, outputFormat) => {
              try {
                // Generate output path with _trimmed suffix and selected format
                const inputPath = videoTrimmerFile.path;
                const lastDot = inputPath.lastIndexOf(".");
                const basePath = lastDot > 0 ? inputPath.substring(0, lastDot) : inputPath;
                const outputPath = `${basePath}_trimmed.${outputFormat}`;

                // Format times as HH:MM:SS.mmm
                const formatTime = (secs: number) => {
                  const hours = Math.floor(secs / 3600);
                  const mins = Math.floor((secs % 3600) / 60);
                  const seconds = secs % 60;
                  return `${hours.toString().padStart(2, "0")}:${mins.toString().padStart(2, "0")}:${seconds.toFixed(3).padStart(6, "0")}`;
                };

                const { toast } = await import("react-hot-toast");
                toast.loading(t("workflow.trimmingVideo"), { id: "trim-video" });

                await invoke("trim_video", {
                  inputPath,
                  outputPath,
                  startTime: formatTime(startTime),
                  endTime: formatTime(endTime),
                });

                toast.success(t("workflow.videoTrimmedSuccess"), { id: "trim-video" });
                closeVideoTrimmer();

                // Optionally add the trimmed file to the file list
                try {
                  const info = await invoke<FileInfo>("get_file_info", { path: outputPath });
                  addFiles([info]);
                } catch {
                  // File info failed, but trim was successful
                }
              } catch (error) {
                const { toast } = await import("react-hot-toast");
                toast.error(`${t("workflow.failedToTrimVideo")}: ${translateErrorCode(String(error))}`, { id: "trim-video" });
              }
            }}
          />
        )}
      </AnimatePresence>
    </div>
  );
}

export default App;
