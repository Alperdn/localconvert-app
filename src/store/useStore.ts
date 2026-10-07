import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { capabilityIdForFile, isCapabilityUsable } from "../utils/capabilityGating";
import { IS_WEB_RUNTIME } from "../platform/runtime";
import * as webApi from "../api/web";

// Image extensions that can be previewed
const PREVIEWABLE_IMAGE_EXTENSIONS = [
  "jpg", "jpeg", "png", "gif", "webp", "bmp", "ico", "svg", "avif"
];

// Helper to load image preview asynchronously
async function loadImagePreview(path: string): Promise<string | null> {
  // Web runtime: preview the browser's own copy (object URL) - the image is
  // not uploaded just to be previewed.
  if (IS_WEB_RUNTIME) return webApi.getLocalPreviewUrl(path);
  try {
    const dataUrl = await invoke<string>("get_image_preview", { path, maxSize: 200 });
    return dataUrl;
  } catch (error) {
    console.error("Failed to load image preview:", error);
    return null;
  }
}

export interface ToolStatus {
  name: string;
  installed: boolean;
  version: string | null;
  path: string | null;
}

// Mirrors src-tauri/src/capabilities.rs `CapabilityState` (SCREAMING_SNAKE_CASE
// over the wire via serde). The frontend never computes this itself - it is
// always backend-reported, so a capability flipping to AVAILABLE (e.g. once
// Office/FFmpeg are bundled) requires no frontend change.
export type CapabilityState =
  | "AVAILABLE"
  | "ENGINE_MISSING"
  | "NOT_IMPLEMENTED"
  | "DISABLED_BY_POLICY";

export interface Capability {
  id: string;
  state: CapabilityState;
  message: string;
}

export interface FileInfo {
  path: string;
  name: string;
  extension: string;
  size: number;
  category: string;
  duration?: number;
  thumbnail?: string;
  resolution?: string;
  codec?: string;
  subtitles?: { index: number; language?: string; codec?: string; title?: string }[];
}

export interface ConversionFile extends FileInfo {
  id: string;
  status: "pending" | "converting" | "completed" | "error";
  progress: number;
  outputFormat: string | null;
  outputPath: string | null;
  error: string | null;
  etaSecs: number | null;
  speed: number | null;
  previewUrl: string | null;
  previewLoading: boolean;
  /// Web runtime only: the server-generated id of this file's current/last
  /// conversion job (used for cancel/delete/download). Never set on desktop.
  webJobId?: string | null;
}

// Step 4 UI-acceptance fix pass (Bug 1/B): the single authoritative
// outcome of one convertFiles() run, so a caller (ConversionPanel,
// keyboard shortcut) can decide which toast to show from actual per-file
// results instead of "the invoke() promise resolved without throwing" -
// convertSingleFile always catches its own errors and calls setFileStatus,
// so convertFiles() itself never rejects just because a conversion failed.
export interface ConversionBatchResult {
  succeeded: string[];
  failed: string[];
  cancelled: string[];
}

export interface ConversionProgressEvent {
  job_id: string;
  progress: number;
  current_time_secs: number;
  total_duration_secs: number;
  speed: number;
  eta_secs: number | null;
  status: string;
}

export interface ConversionOptions {
  quality?: number;
  width?: number;
  height?: number;
  bitrate?: string;
  fps?: number | string;
  preserveMetadata?: boolean;
  startTime?: string;
  endTime?: string;
  compressionLevel?: number;
  useGpu?: boolean;
  gpuEncoder?: string;
  
  // Advanced Video Settings
  presetResolution?: string;
  customWidth?: number;
  customHeight?: number;
  videoCodec?: string;
  audioCodec?: string;
  bitrateMode?: "CBR" | "VBR";
  videoBitrate?: number; // In Mbps
  crf?: number; // 0-51
  twoPass?: boolean;
  // Advanced Audio & Subtitles
  maintainAspectRatio?: boolean;
  audioBitrateKbps?: number;
  audioSampleRate?: string;
  volumeDb?: number;
  channelLayout?: string;
  subtitleAction?: string;
  subtitleStreamIndex?: number | null;
}

export interface GpuEncoder {
  name: string;
  codec: string;
  vendor: string;
  description: string;
}

export interface GpuInfo {
  available: boolean;
  encoders: GpuEncoder[];
  preferred_encoder: string | null;
}

export interface Settings {
  outputDirectory: string;
  // `theme` is always resolved to an actual "dark" | "light" - every
  // component in the app branches on this directly (`settings.theme ===
  // "dark"`), so it must never literally hold "system". The user's actual
  // choice (which may be "system") lives in `themePreference`; the store
  // keeps `theme` in sync with it (and with the OS color scheme, live,
  // when the preference is "system") - see the init/subscribe block at
  // the bottom of this file. This means adding System-preference support
  // required touching only this file, not the ~15 components that read
  // `settings.theme`.
  theme: "dark" | "light";
  themePreference: "dark" | "light" | "system";
  defaultQuality: number;
  preserveMetadata: boolean;
  useGpu: boolean;
  gpuEncoder: string | null;
  parallelProcessing: boolean;
  maxParallelConversions: number;
  // New settings
  playCompletionSound: boolean;
  outputFilenameTemplate: string;
  watchFolders: string[];
  contextMenuEnabled: boolean;
  customFfmpegParams: string;
  // Advanced Video Settings
  presetResolution: string;
  customWidth: number;
  customHeight: number;
  videoCodec: string;
  audioCodec: string;
  bitrateMode: "CBR" | "VBR";
  videoBitrate: number; // Mbps
  crf: number; // 0-51
  fps: string;
  twoPass: boolean;
  // Advanced Audio & Subtitle Settings
  maintainAspectRatio: boolean;
  audioBitrateKbps: number;
  audioSampleRate: string;
  volumeDb: number;
  channelLayout: string;
  hwAcceleratorEnabled: boolean;
  subtitleAction: string;
  subtitleStreamIndex: number | null;
}

export interface ConversionPreset {
  id: string;
  name: string;
  description: string;
  category: "video" | "audio" | "image" | "document";
  outputFormat: string;
  quality: number;
  options: ConversionOptions;
}

// Built-in presets
export const CONVERSION_PRESETS: ConversionPreset[] = [
  // Video presets
  {
    id: "web-video",
    name: "Web Optimized",
    description: "Smaller file size for web streaming",
    category: "video",
    outputFormat: "mp4",
    quality: 70,
    options: { bitrate: "2M", fps: 30 },
  },
  {
    id: "youtube",
    name: "YouTube Upload",
    description: "Optimal settings for YouTube",
    category: "video",
    outputFormat: "mp4",
    quality: 85,
    options: { bitrate: "8M", fps: 30 },
  },
  {
    id: "instagram-story",
    name: "Instagram Story",
    description: "9:16 vertical video for stories",
    category: "video",
    outputFormat: "mp4",
    quality: 80,
    options: { width: 1080, height: 1920, bitrate: "4M" },
  },
  {
    id: "twitter-video",
    name: "Twitter/X Video",
    description: "Under 512MB, max 2:20",
    category: "video",
    outputFormat: "mp4",
    quality: 75,
    options: { bitrate: "5M" },
  },
  {
    id: "discord-8mb",
    name: "Discord (<8MB)",
    description: "Compressed for Discord free tier",
    category: "video",
    outputFormat: "mp4",
    quality: 50,
    options: { bitrate: "1M" },
  },
  {
    id: "gif-conversion",
    name: "GIF",
    description: "Convert video to animated GIF",
    category: "video",
    outputFormat: "gif",
    quality: 80,
    options: { fps: 15, width: 480 },
  },
  // Audio presets
  {
    id: "podcast",
    name: "Podcast",
    description: "Mono MP3 for podcasts",
    category: "audio",
    outputFormat: "mp3",
    quality: 70,
    options: { bitrate: "128k" },
  },
  {
    id: "music-hq",
    name: "Music HQ",
    description: "High quality FLAC",
    category: "audio",
    outputFormat: "flac",
    quality: 100,
    options: {},
  },
  {
    id: "voice-memo",
    name: "Voice Memo",
    description: "Small file size for voice",
    category: "audio",
    outputFormat: "mp3",
    quality: 50,
    options: { bitrate: "64k" },
  },
  // Image presets
  {
    id: "web-image",
    name: "Web Optimized",
    description: "WebP for fast loading",
    category: "image",
    outputFormat: "webp",
    quality: 80,
    options: {},
  },
  {
    id: "social-media",
    name: "Social Media",
    description: "JPEG optimized for sharing",
    category: "image",
    outputFormat: "jpg",
    quality: 85,
    options: {},
  },
  {
    id: "print-quality",
    name: "Print Quality",
    description: "High quality PNG",
    category: "image",
    outputFormat: "png",
    quality: 100,
    options: {},
  },
  {
    id: "thumbnail",
    name: "Thumbnail",
    description: "Small preview image",
    category: "image",
    outputFormat: "jpg",
    quality: 70,
    options: { width: 320, height: 240 },
  },
  // Document presets
  {
    id: "pdf-compress",
    name: "PDF Compressed",
    description: "Smaller PDF for email",
    category: "document",
    outputFormat: "pdf",
    quality: 60,
    options: { compressionLevel: 8 },
  },
  {
    id: "pdf-print",
    name: "PDF Print Ready",
    description: "High quality for printing",
    category: "document",
    outputFormat: "pdf",
    quality: 100,
    options: {},
  },
];

// Device presets with specific resolutions
export const DEVICE_PRESETS: ConversionPreset[] = [
  {
    id: "iphone-video",
    name: "iPhone",
    description: "1080p H.264 for iOS",
    category: "video",
    outputFormat: "mp4",
    quality: 80,
    options: { width: 1920, height: 1080, bitrate: "8M" },
  },
  {
    id: "android-video",
    name: "Android",
    description: "720p for most Android devices",
    category: "video",
    outputFormat: "mp4",
    quality: 75,
    options: { width: 1280, height: 720, bitrate: "5M" },
  },
  {
    id: "ps5-video",
    name: "PlayStation 5",
    description: "4K HDR compatible",
    category: "video",
    outputFormat: "mp4",
    quality: 90,
    options: { width: 3840, height: 2160, bitrate: "20M" },
  },
  {
    id: "xbox-video",
    name: "Xbox Series X",
    description: "4K compatible",
    category: "video",
    outputFormat: "mp4",
    quality: 90,
    options: { width: 3840, height: 2160, bitrate: "20M" },
  },
  {
    id: "roku-video",
    name: "Roku",
    description: "Compatible with all Roku devices",
    category: "video",
    outputFormat: "mp4",
    quality: 80,
    options: { width: 1920, height: 1080, bitrate: "8M" },
  },
  {
    id: "chromecast-video",
    name: "Chromecast",
    description: "Optimized for Chromecast",
    category: "video",
    outputFormat: "mp4",
    quality: 80,
    options: { width: 1920, height: 1080, bitrate: "8M" },
  },
];

export type Category =
  | "all"
  | "video"
  | "audio"
  | "image"
  | "document"
  | "spreadsheet"
  | "presentation"
  | "ebook"
  | "archive"
  | "vector"
  | "font";

interface Store {
  // Files
  files: ConversionFile[];
  selectedFiles: string[];
  addFiles: (files: FileInfo[]) => void;
  removeFile: (id: string) => void;
  clearFiles: () => void;
  selectFile: (id: string) => void;
  deselectFile: (id: string) => void;
  selectAllFiles: () => void;
  deselectAllFiles: () => void;
  setFileOutputFormat: (id: string, format: string) => void;
  setOutputFormatForFiles: (ids: string[], format: string) => void;
  setFileStatus: (
    id: string,
    status: ConversionFile["status"],
    progress?: number,
    error?: string,
    outputPath?: string,
    etaSecs?: number | null,
    speed?: number | null
  ) => void;
  retryFile: (id: string) => void;
  reorderFiles: (files: ConversionFile[]) => void;

  // Category
  activeCategory: Category;
  setActiveCategory: (category: Category) => void;

  // Top-level view. "dictation" (Ses Dikte, speech-to-text) is a separate
  // feature from the audio *conversion* category and never shares its
  // file list / conversion state.
  activeView: "convert" | "dictation";
  setActiveView: (view: "convert" | "dictation") => void;

  // Tools
  tools: ToolStatus[];
  toolsChecked: boolean;
  checkTools: () => Promise<void>;

  // Capabilities (backend-computed feature availability - see
  // src-tauri/src/capabilities.rs). Source of truth lives in Rust; the
  // frontend only renders it.
  capabilities: Capability[];
  capabilitiesLoaded: boolean;
  loadCapabilities: () => Promise<void>;
  getCapability: (id: string) => Capability | undefined;

  // GPU
  gpuInfo: GpuInfo | null;
  detectGpu: () => Promise<void>;

  // Conversion
  isConverting: boolean;
  activeConversions: Set<string>;
  currentJobId: string | null;
  convertFiles: (options?: ConversionOptions) => Promise<ConversionBatchResult>;
  cancelConversion: (jobId?: string) => Promise<void>;

  // Settings
  settings: Settings;
  updateSettings: (settings: Partial<Settings>) => void;

  // Output format
  globalOutputFormat: string | null;
  setGlobalOutputFormat: (format: string | null) => void;

  // Image preview
  previewImageId: string | null;
  setPreviewImageId: (id: string | null) => void;

  // PDF Editor
  pdfEditorFile: { path: string; name: string } | null;
  openPdfEditor: (path: string, name: string) => void;
  closePdfEditor: () => void;

  // Video Trimmer
  videoTrimmerFile: { path: string; name: string; id: string } | null;
  openVideoTrimmer: (path: string, name: string, id: string) => void;
  closeVideoTrimmer: () => void;
}

const generateId = () => Math.random().toString(36).substring(2, 15);

// ── Single source of truth for "which files does the right-hand
// ConversionPanel currently operate on" (Step 3 manual-test fix pass,
// finding #5) ────────────────────────────────────────────────────────────
// Previously ConversionPanel/useKeyboardShortcuts/convertFiles each
// re-derived this independently from `files` + `selectedFiles`, entirely
// ignoring `activeCategory`. That let a blocked capability from a file in
// one category (e.g. a video with ENGINE_MISSING) keep blocking conversion
// of files in a different, unrelated category (e.g. PNG->JPEG) after the
// user switched the left-nav category, because the batch never actually
// changed - only the *visible* FileList did. Scoping this helper to
// `activeCategory` keeps the right panel and the left nav showing the same
// set of files at all times.
export function getFilesToConvert(state: {
  files: ConversionFile[];
  selectedFiles: string[];
  activeCategory: Category;
}): ConversionFile[] {
  const inCategory = (f: ConversionFile) =>
    state.activeCategory === "all" || f.category === state.activeCategory;

  const pool =
    state.selectedFiles.length > 0
      ? state.files.filter((f) => state.selectedFiles.includes(f.id) && f.status === "pending")
      : state.files.filter((f) => f.status === "pending");

  return pool.filter(inCategory);
}

// ── Single source of truth for "which of the current conversion candidates
// are ACTIVE enough to justify a capability warning" (Step 4 UI-hardening
// pass, Bug 1) ─────────────────────────────────────────────────────────────
// getFilesToConvert's own "no explicit selection" fallback deliberately
// keeps EVERY pending file in scope (see its doc comment and the "a brand
// new file, never explicitly selected, must now be part of the next batch"
// test) - that's the correct, already-tested set of files Convert will
// actually attempt. But using it directly to decide whether to show the
// amber "engine missing" banner meant a file the user had only just added -
// never selected, never given an output format, never touched at all - was
// treated as if the user had already decided to convert it: dropping a
// video while working on something else in the "All" view immediately
// raised an FFmpeg warning for a file nobody had acted on yet, and the
// warning stayed pinned on that video even after switching attention to an
// unrelated PNG, because the PNG had no reason to change the picture.
//
// An explicit selection is always enough on its own - the user checked that
// file's box on purpose, so `getFilesToConvert`'s selection-scoped pool is
// used unchanged. With no explicit selection, a file only counts once it
// has a resolved output format (per-file `outputFormat`, or the shared
// `globalOutputFormat`) - some sign the user actually engaged with it,
// rather than merely having added it to the list.
export function getActiveConversionCandidates(state: {
  files: ConversionFile[];
  selectedFiles: string[];
  activeCategory: Category;
  globalOutputFormat: string | null;
}): ConversionFile[] {
  const candidates = getFilesToConvert(state);
  if (state.selectedFiles.length > 0) return candidates;
  return candidates.filter((f) => resolveOutputFormat(f, state.globalOutputFormat));
}

// ── Single source of truth for "what target format is this file actually
// going to convert to" (Step 3 format-sync fix pass, finding #1) ─────────
// `file.outputFormat` (per-file, set from the FileCard picker) always wins
// when present; `globalOutputFormat` is only a fallback default for a file
// that has no explicit choice yet. This exact fallback already governed
// which format `convertFiles` actually sent to the backend - the bug was
// that ConversionPanel's picker displayed/wrote `globalOutputFormat`
// directly instead of resolving through this same rule, so it could show
// a stale/different value than the FileCard sitting right next to it.
export function resolveOutputFormat(
  file: Pick<ConversionFile, "outputFormat">,
  globalOutputFormat: string | null
): string | null {
  return file.outputFormat || globalOutputFormat;
}

/**
 * The format ConversionPanel should display/highlight as "selected" for the
 * current batch: the resolved format shared by every file in scope, or
 * `null` if the batch is empty or the files don't agree (e.g. one file's
 * per-file choice was changed independently) - never silently picks one.
 */
export function commonOutputFormat(
  files: Pick<ConversionFile, "outputFormat">[],
  globalOutputFormat: string | null
): string | null {
  if (files.length === 0) return globalOutputFormat;
  const resolved = new Set(files.map((f) => resolveOutputFormat(f, globalOutputFormat)));
  return resolved.size === 1 ? [...resolved][0] : null;
}

export const useStore = create<Store>((set, get) => ({
  // Files
  files: [],
  selectedFiles: [],

  addFiles: (newFiles) => {
    const filesWithId: ConversionFile[] = newFiles.map((f) => {
      const ext = f.extension.toLowerCase();
      const isPreviewable = PREVIEWABLE_IMAGE_EXTENSIONS.includes(ext);
      return {
        ...f,
        id: generateId(),
        status: "pending",
        progress: 0,
        outputFormat: null,
        outputPath: null,
        error: null,
        etaSecs: null,
        speed: null,
        previewUrl: null,
        previewLoading: isPreviewable, // Show loading state for images
      };
    });
    
    // Add files immediately
    set((state) => ({ files: [...state.files, ...filesWithId] }));
    
    // Load image previews asynchronously in the background
    filesWithId.forEach((file) => {
      if (file.previewLoading) {
        loadImagePreview(file.path).then((previewUrl) => {
          set((state) => ({
            files: state.files.map((f) =>
              f.id === file.id ? { ...f, previewUrl, previewLoading: false } : f
            ),
          }));
        });
      } else if (file.category === "video" && !IS_WEB_RUNTIME) {
        // Fetch metadata
        invoke<{
          duration: number | null;
          resolution: string | null;
          codec: string | null;
        }>("get_video_metadata", { path: file.path })
          .then((meta) => {
            set((state) => ({
              files: state.files.map((f) =>
                f.id === file.id
                  ? {
                      ...f,
                      duration: meta.duration ?? f.duration,
                      resolution: meta.resolution ?? f.resolution,
                      codec: meta.codec ?? f.codec,
                    }
                  : f
              ),
            }));
          })
          .catch((err) => console.error("Failed to load video metadata:", err));

        // Fetch thumbnail
        invoke<string>("get_video_thumbnail", {
          path: file.path,
          timeSecs: 1.0,
          width: 320,
        })
          .then((thumbnail) => {
            set((state) => ({
              files: state.files.map((f) =>
                f.id === file.id ? { ...f, thumbnail } : f
              ),
            }));
          })
          .catch((err) => console.error("Failed to load video thumbnail:", err));
      }
    });
  },

  removeFile: (id) => {
    if (IS_WEB_RUNTIME) {
      const file = get().files.find((f) => f.id === id);
      if (file) releaseWebFile(file);
    }
    set((state) => ({
      files: state.files.filter((f) => f.id !== id),
      selectedFiles: state.selectedFiles.filter((fid) => fid !== id),
    }));
  },

  clearFiles: () => {
    if (IS_WEB_RUNTIME) get().files.forEach(releaseWebFile);
    set({ files: [], selectedFiles: [] });
  },

  selectFile: (id) => {
    set((state) => ({
      selectedFiles: state.selectedFiles.includes(id)
        ? state.selectedFiles
        : [...state.selectedFiles, id],
    }));
  },

  deselectFile: (id) => {
    set((state) => ({
      selectedFiles: state.selectedFiles.filter((fid) => fid !== id),
    }));
  },

  selectAllFiles: () => {
    set((state) => ({
      selectedFiles: state.files.map((f) => f.id),
    }));
  },

  deselectAllFiles: () => {
    set({ selectedFiles: [] });
  },

  setFileOutputFormat: (id, format) => {
    set((state) => ({
      files: state.files.map((f) =>
        f.id === id ? { ...f, outputFormat: format } : f
      ),
    }));
  },

  // Step 3 format-sync fix pass (finding #1/A): the ConversionPanel's
  // shared format picker applies to every file currently in scope in one
  // batched update, rather than N sequential setFileOutputFormat calls
  // (N re-renders) or a second, independent "globalOutputFormat is the
  // real value" state that FileCard has to be kept in sync with.
  setOutputFormatForFiles: (ids, format) => {
    const idSet = new Set(ids);
    set((state) => ({
      files: state.files.map((f) =>
        idSet.has(f.id) ? { ...f, outputFormat: format } : f
      ),
    }));
  },

  setFileStatus: (id, status, progress = 0, error = undefined, outputPath = undefined, etaSecs = undefined, speed = undefined) => {
    set((state) => ({
      files: state.files.map((f) =>
        f.id === id
          ? {
              ...f,
              status,
              progress,
              // Step 4 UI-acceptance fix pass (Bug 1/D): a terminal result
              // must resolve exactly once and never carry a previous
              // attempt's leftovers. Only an "error" transition is allowed
              // to (re)populate `error` - every other status (converting,
              // completed, pending) always clears it, so a retry that
              // succeeds can never leave stale error text sitting next to
              // a "completed" card (FileCard renders `file.error`
              // independently of `file.status`).
              error: status === "error" ? (error ?? f.error) : null,
              outputPath: outputPath ?? f.outputPath,
              etaSecs: etaSecs ?? f.etaSecs,
              speed: speed ?? f.speed,
            }
          : f
      ),
      // Step 3 manual-test fix (finding #6): once a file leaves "pending"
      // (completed or errored), drop it from the explicit selection too.
      // Otherwise a stale non-empty `selectedFiles` kept forcing
      // `getFilesToConvert` into "selection-only" mode, which silently
      // excluded every newly-added file (never selected) from the next
      // batch - the user had no way to start a new conversion without
      // pressing "Temizle" to reset selection entirely.
      selectedFiles:
        status === "completed" || status === "error"
          ? state.selectedFiles.filter((fid) => fid !== id)
          : state.selectedFiles,
    }));
  },

  // Step 4 UI-acceptance fix pass (Bug 2): explicit, single retry path for
  // an errored file. `getFilesToConvert` intentionally only ever considers
  // `status === "pending"` files (see its own doc comment) - an "error"
  // file is a genuine terminal state, not a candidate, and stays that way
  // forever unless something explicitly moves it back to "pending". Before
  // this action existed, nothing did: deselecting/reselecting an errored
  // file only touched `selectedFiles`, never `status`, so the file could
  // never re-enter `getFilesToConvert` and the ConversionPanel was stuck
  // showing "Dosya bekleniyor" with no way out except the global "Temizle".
  // retryFile is the single, deliberate transition error -> pending: it
  // clears the stale terminal result (error text, output path, progress,
  // speed/eta) and re-selects the file so it's picked up by the very next
  // batch regardless of what else is currently selected (avoids the
  // "selection-only mode" trap - see getFilesToConvert/section K).
  retryFile: (id) => {
    set((state) => ({
      files: state.files.map((f) =>
        f.id === id
          ? {
              ...f,
              status: "pending",
              progress: 0,
              error: null,
              outputPath: null,
              etaSecs: null,
              speed: null,
            }
          : f
      ),
      selectedFiles: state.selectedFiles.includes(id)
        ? state.selectedFiles
        : [...state.selectedFiles, id],
    }));
  },

  reorderFiles: (newFiles) => {
    set({ files: newFiles });
  },

  // Category
  activeView: "convert",
  setActiveView: (view) => set({ activeView: view }),
  activeCategory: "all",
  setActiveCategory: (category) =>
    set((state) => ({
      activeCategory: category,
      activeView: "convert",
      // Step 3 manual-test fix (finding #5/C): a target format chosen for
      // the previous category is almost never valid for the new one (e.g.
      // "mp4" selected while on Video, then switching to Image) - clear it
      // so the format picker can't silently keep an invalid stale value.
      // Per-file `outputFormat` selections are untouched: they belong to
      // the file, not the category, and files outside the new category
      // simply aren't part of `getFilesToConvert` until switched back to.
      globalOutputFormat:
        category !== state.activeCategory ? null : state.globalOutputFormat,
    })),

  // Tools
  tools: [],
  toolsChecked: false,

  checkTools: async () => {
    // Web runtime: there are no locally installed tools to check - what the
    // server can do is reported by loadCapabilities().
    if (IS_WEB_RUNTIME) {
      set({ tools: [], toolsChecked: true });
      return;
    }
    try {
      const tools = await invoke<ToolStatus[]>("check_tools");
      set({ tools, toolsChecked: true });
    } catch (error) {
      console.error("Failed to check tools:", error);
      set({ toolsChecked: true });
    }
  },

  // Capabilities
  capabilities: [],
  capabilitiesLoaded: false,

  loadCapabilities: async () => {
    try {
      const capabilities = IS_WEB_RUNTIME
        ? await webApi.getWebCapabilities()
        : await invoke<Capability[]>("get_capabilities");
      set({ capabilities, capabilitiesLoaded: true });
    } catch (error) {
      console.error("Failed to load capabilities:", error);
      set({ capabilitiesLoaded: true });
    }
  },

  getCapability: (id) => get().capabilities.find((c) => c.id === id),

  // GPU
  gpuInfo: null,

  detectGpu: async () => {
    // Hardware/encoder choice is server policy in the web runtime.
    if (IS_WEB_RUNTIME) return;
    try {
      const gpuInfo = await invoke<GpuInfo>("detect_gpu");
      set({ gpuInfo });
      // Auto-enable GPU if available and set preferred encoder
      if (gpuInfo.available && gpuInfo.preferred_encoder) {
        const { settings } = get();
        if (settings.gpuEncoder === null) {
          set({
            settings: {
              ...settings,
              useGpu: true,
              gpuEncoder: gpuInfo.preferred_encoder,
            },
          });
        }
      }
    } catch (error) {
      console.error("Failed to detect GPU:", error);
    }
  },

  // Conversion
  isConverting: false,
  activeConversions: new Set<string>(),
  currentJobId: null,

  convertFiles: async (options = {}) => {
    const { settings, globalOutputFormat, setFileStatus, getCapability } =
      get();

    // Scoped to the active category, same as ConversionPanel/keyboard
    // shortcut - see getFilesToConvert's doc comment (Step 3 finding #5).
    const candidateFiles = getFilesToConvert(get());

    // Defense in depth (Step 3, section E): never rely solely on a
    // disabled button or a keyboard-shortcut guard to keep a blocked file
    // from reaching a real process spawn. Any file whose capability isn't
    // AVAILABLE is dropped here unconditionally, no matter how this action
    // was triggered.
    const filesToConvert = candidateFiles.filter((f) => {
      const capId = capabilityIdForFile(f);
      return !capId || isCapabilityUsable(getCapability(capId));
    });

    const batchResult: ConversionBatchResult = { succeeded: [], failed: [], cancelled: [] };

    if (filesToConvert.length === 0) return batchResult;

    // Add files to active conversions
    const newActiveConversions = new Set(get().activeConversions);
    filesToConvert.forEach(f => newActiveConversions.add(f.id));
    set({ isConverting: true, activeConversions: newActiveConversions });

    // Set up progress listener (only if not already listening). The web
    // runtime gets progress per job over SSE instead (see webApi.watchJob).
    let unlisten: UnlistenFn | null = null;
    try {
      if (!IS_WEB_RUNTIME) unlisten = await listen<ConversionProgressEvent>("conversion-progress", (event) => {
        const progress = event.payload;
        const currentFile = get().files.find((f) => f.id === progress.job_id);
        if (currentFile && currentFile.status === "converting") {
          setFileStatus(
            progress.job_id,
            "converting",
            progress.progress,
            undefined,
            undefined,
            progress.eta_secs,
            progress.speed
          );
        }
      });
    } catch (e) {
      console.error("Failed to set up progress listener:", e);
    }

    // Helper function to convert a single file
    const convertSingleFile = async (file: ConversionFile) => {
      // Check if this specific file was cancelled
      if (!get().activeConversions.has(file.id)) return;

      const outputFormat = file.outputFormat || globalOutputFormat;
      if (!outputFormat) return;

      if (IS_WEB_RUNTIME) {
        const outcome = await convertSingleFileWeb(file, outputFormat, {
          quality: options.quality ?? settings.defaultQuality,
          width: options.width,
          height: options.height,
        });
        const updatedActive = new Set(get().activeConversions);
        updatedActive.delete(file.id);
        set({ activeConversions: updatedActive });
        batchResult[outcome].push(file.id);
        return;
      }

      // Use the file ID as the job ID for tracking
      const jobId = file.id;

      setFileStatus(file.id, "converting", 0, undefined, undefined, null, null);

      try {
        const result = await invoke<{
          success: boolean;
          output_path: string | null;
          error: string | null;
          duration_ms: number;
        }>("convert_file", {
          inputPath: file.path,
          outputFormat: outputFormat,
          outputDir: settings.outputDirectory,
          options: {
            quality: options.quality ?? settings.defaultQuality,
            preserve_metadata: options.preserveMetadata ?? settings.preserveMetadata,
            use_gpu: options.useGpu ?? settings.useGpu,
            gpu_encoder: options.gpuEncoder ?? settings.gpuEncoder,
            ...options,
          },
          jobId: jobId,
        });

        // Remove from active conversions
        const updatedActive = new Set(get().activeConversions);
        updatedActive.delete(file.id);
        set({ activeConversions: updatedActive });

        // Check if this conversion was cancelled
        if (result.error === "Conversion cancelled") {
          setFileStatus(file.id, "pending", 0, undefined, undefined, null, null);
          batchResult.cancelled.push(file.id);
          return;
        }

        // Step 4 UI-acceptance fix pass (Bug 1/B): the backend's `success`
        // boolean plus its authoritative `output_path` is the ONLY thing
        // that decides completed vs. error here - never inferred from the
        // invoke() promise merely resolving (that's true even when the
        // process launched fine but the engine itself reported failure).
        if (result.success) {
          setFileStatus(file.id, "completed", 100, undefined, result.output_path ?? undefined, null, null);
          batchResult.succeeded.push(file.id);
        } else {
          setFileStatus(file.id, "error", 0, result.error ?? "PROCESS_FAILED", undefined, null, null);
          batchResult.failed.push(file.id);
        }
      } catch (error) {
        const errorStr = String(error);
        // Remove from active conversions
        const updatedActive = new Set(get().activeConversions);
        updatedActive.delete(file.id);
        set({ activeConversions: updatedActive });

        // Don't show error for cancelled conversions
        if (errorStr.includes("cancelled")) {
          setFileStatus(file.id, "pending", 0, undefined, undefined, null, null);
          batchResult.cancelled.push(file.id);
        } else {
          setFileStatus(file.id, "error", 0, errorStr, undefined, null, null);
          batchResult.failed.push(file.id);
        }
      }
    };

    // Check if parallel processing is enabled
    const useParallel = settings.parallelProcessing;
    const maxParallel = settings.maxParallelConversions || 4;

    // Separate files into categories: videos (sequential) vs others (can be parallel)
    const videoCategories = ["video"];
    const videoFiles = filesToConvert.filter(f => videoCategories.includes(f.category));
    const otherFiles = filesToConvert.filter(f => !videoCategories.includes(f.category));

    if (useParallel && otherFiles.length > 0) {
      // Process non-video files in parallel batches
      const processBatch = async (batch: ConversionFile[]) => {
        await Promise.all(batch.map(file => convertSingleFile(file)));
      };

      // Process in chunks of maxParallel
      for (let i = 0; i < otherFiles.length; i += maxParallel) {
        const batch = otherFiles.slice(i, i + maxParallel);
        await processBatch(batch);
      }

      // Process video files sequentially after (they're CPU/GPU intensive)
      for (const file of videoFiles) {
        set({ currentJobId: file.id });
        await convertSingleFile(file);
      }
    } else {
      // Sequential processing (original behavior)
      for (const file of filesToConvert) {
        set({ currentJobId: file.id });
        await convertSingleFile(file);
      }
    }

    // Clean up listener
    if (unlisten) {
      unlisten();
    }

    // Only set isConverting to false if no more active conversions
    const remainingActive = get().activeConversions;
    if (remainingActive.size === 0) {
      set({ isConverting: false, currentJobId: null });
    }

    return batchResult;
  },

  cancelConversion: async (jobId?: string) => {
    const { activeConversions } = get();

    // Web runtime: cancellation is authoritative on the server. The file's
    // status follows the job's real terminal state (via watchJob) instead of
    // being reset optimistically here.
    if (IS_WEB_RUNTIME) {
      const ids = jobId ? [jobId] : [...activeConversions];
      ids.forEach(requestWebCancel);
      return;
    }

    if (jobId) {
      // Cancel specific job
      try {
        await invoke("cancel_conversion", { jobId });
      } catch (error) {
        console.error("Failed to cancel conversion:", error);
      }
      
      // Remove from active conversions
      const updatedActive = new Set(activeConversions);
      updatedActive.delete(jobId);
      set({ activeConversions: updatedActive });
      
      // Reset that file to pending
      set((state) => ({
        files: state.files.map((f) =>
          f.id === jobId ? { ...f, status: "pending", progress: 0 } : f
        ),
        isConverting: updatedActive.size > 0,
      }));
    } else {
      // Cancel all active conversions
      for (const activeJobId of activeConversions) {
        try {
          await invoke("cancel_conversion", { jobId: activeJobId });
        } catch (error) {
          console.error("Failed to cancel conversion:", error);
        }
      }
      
      set({ isConverting: false, currentJobId: null, activeConversions: new Set() });
      
      // Reset all converting files to pending
      set((state) => ({
        files: state.files.map((f) =>
          f.status === "converting" ? { ...f, status: "pending", progress: 0 } : f
        ),
      }));
    }
  },

  // Settings
  settings: {
    outputDirectory: "",
    theme: "dark",
    themePreference: "dark",
    defaultQuality: 85,
    // Phase 1 privacy default: strip metadata unless the user opts to
    // keep it (currently enforced for image conversions - see the Rust
    // side's ConversionOptions::default() and system_status command).
    preserveMetadata: false,
    useGpu: false,
    gpuEncoder: null,
    parallelProcessing: true,
    maxParallelConversions: 4,
    playCompletionSound: true,
    outputFilenameTemplate: "{name}_{preset}",
    watchFolders: [],
    contextMenuEnabled: false,
    customFfmpegParams: "",
    // Advanced Video Settings Defaults
    presetResolution: "Match Source",
    customWidth: 1920,
    customHeight: 1080,
    videoCodec: "H.264",
    audioCodec: "AAC",
    bitrateMode: "VBR",
    videoBitrate: 8,
    crf: 23,
    fps: "Match Source",
    twoPass: false,
    // Advanced Audio & Subtitle Settings
    maintainAspectRatio: true,
    audioBitrateKbps: 192,
    audioSampleRate: "Match Source",
    volumeDb: 0,
    channelLayout: "Auto",
    hwAcceleratorEnabled: false,
    subtitleAction: "No Change",
    subtitleStreamIndex: null,
  },

  updateSettings: (newSettings) => {
    set((state) => ({
      settings: { ...state.settings, ...newSettings },
    }));
  },

  // Output format
  globalOutputFormat: null,
  setGlobalOutputFormat: (format) => set({ globalOutputFormat: format }),

  // Image preview
  previewImageId: null,
  setPreviewImageId: (id) => set({ previewImageId: id }),

  // PDF Editor
  pdfEditorFile: null,
  openPdfEditor: (path, name) => set({ pdfEditorFile: { path, name } }),
  closePdfEditor: () => set({ pdfEditorFile: null }),

  // Video Trimmer
  videoTrimmerFile: null,
  openVideoTrimmer: (path, name, id) => set({ videoTrimmerFile: { path, name, id } }),
  closeVideoTrimmer: () => set({ videoTrimmerFile: null }),
}));

// ── Web runtime conversion path ───────────────────────────────────────────
// upload (server assigns file_id) -> create job (server assigns job_id) ->
// follow the job's snapshots (SSE, polling fallback) -> download URL.
// The browser never sends a path; the server owns every id and file.

interface WebInFlight {
  abort: AbortController;
  jobId: string | null;
  cancelled: boolean;
}

/// Conversions currently uploading or running, keyed by the store file id.
const webInFlight = new Map<string, WebInFlight>();

function setWebJobId(fileId: string, webJobId: string | null) {
  useStore.setState((s) => ({
    files: s.files.map((f) => (f.id === fileId ? { ...f, webJobId } : f)),
  }));
}

async function convertSingleFileWeb(
  file: ConversionFile,
  outputFormat: string,
  options: { quality?: number; width?: number; height?: number }
): Promise<keyof ConversionBatchResult> {
  const { setFileStatus } = useStore.getState();
  const local = webApi.getLocalFile(file.path);
  if (!local) {
    setFileStatus(file.id, "error", 0, "FILE_NOT_AVAILABLE", undefined, null, null);
    return "failed";
  }

  const entry: WebInFlight = { abort: new AbortController(), jobId: null, cancelled: false };
  webInFlight.set(file.id, entry);
  setFileStatus(file.id, "converting", 0, undefined, undefined, null, null);

  // Progress: first half = real upload bytes, second half = the job's real
  // (stage-based) progress reported by the server.
  try {
    const uploaded = await webApi.uploadFile(
      local,
      (fraction) => setFileStatus(file.id, "converting", Math.round(fraction * 50), undefined, undefined, null, null),
      entry.abort.signal
    );
    if (entry.cancelled) {
      setFileStatus(file.id, "pending", 0, undefined, undefined, null, null);
      return "cancelled";
    }

    const jobOptions: webApi.WebConvertOptions = {};
    if (options.quality !== undefined) {
      jobOptions.quality = Math.min(100, Math.max(1, Math.round(options.quality)));
    }
    if (options.width && options.height) {
      jobOptions.width = Math.round(options.width);
      jobOptions.height = Math.round(options.height);
    }

    const created = await webApi.createConvertJob(uploaded.file_id, outputFormat, jobOptions);
    entry.jobId = created.job_id;
    setWebJobId(file.id, created.job_id);
    if (entry.cancelled) void webApi.cancelJob(created.job_id).catch(() => {});

    const final = await webApi.watchJob(created.job_id, (snapshot) => {
      if (!webApi.isTerminal(snapshot.state)) {
        const jobPct = snapshot.progress_pct ?? 0;
        setFileStatus(file.id, "converting", 50 + Math.round(jobPct / 2), undefined, undefined, null, null);
      }
    });

    switch (final.state) {
      case "completed":
        setFileStatus(file.id, "completed", 100, undefined, webApi.jobDownloadUrl(final.job_id), null, null);
        return "succeeded";
      case "cancelled":
        setFileStatus(file.id, "pending", 0, undefined, undefined, null, null);
        return "cancelled";
      default:
        setFileStatus(file.id, "error", 0, final.error?.code ?? "PROCESS_FAILED", undefined, null, null);
        return "failed";
    }
  } catch (error) {
    const code = error instanceof webApi.WebApiError ? error.code : "PROCESS_FAILED";
    if (code === "CANCELLED" || entry.cancelled) {
      setFileStatus(file.id, "pending", 0, undefined, undefined, null, null);
      return "cancelled";
    }
    setFileStatus(file.id, "error", 0, code, undefined, null, null);
    return "failed";
  } finally {
    webInFlight.delete(file.id);
  }
}

function requestWebCancel(fileId: string) {
  const entry = webInFlight.get(fileId);
  if (entry) {
    entry.cancelled = true;
    entry.abort.abort();
    if (entry.jobId) void webApi.cancelJob(entry.jobId).catch(() => {});
    return;
  }
  // Not started yet (still waiting its turn in the batch): drop it so the
  // batch loop skips it.
  const { activeConversions } = useStore.getState();
  if (activeConversions.has(fileId)) {
    const updated = new Set(activeConversions);
    updated.delete(fileId);
    useStore.setState({ activeConversions: updated });
  }
}

/// Removing a file in the web runtime also releases everything tied to it:
/// any running conversion is cancelled, its server job (and output) is
/// deleted, and the browser-side copy/preview is dropped.
function releaseWebFile(file: ConversionFile) {
  const entry = webInFlight.get(file.id);
  if (entry) {
    entry.cancelled = true;
    entry.abort.abort();
  }
  const jobId = entry?.jobId ?? file.webJobId;
  if (jobId) void webApi.deleteJob(jobId).catch(() => {});
  webApi.forgetLocalFile(file.path);
}

// ── Theme resolution ─────────────────────────────────────────────────────
// `themePreference` ("dark" | "light" | "system") is what the user picks
// in Settings and what gets persisted. `settings.theme` is always the
// resolved "dark" | "light" value that every component in the app reads
// directly - this function is the only place "system" gets turned into
// an actual value, via the OS color-scheme media query.
function resolveTheme(pref: "dark" | "light" | "system"): "dark" | "light" {
  if (pref === "system") {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  return pref;
}

function applyResolvedThemeToDocument(theme: "dark" | "light") {
  if (theme === "dark") {
    document.documentElement.classList.add("dark");
  } else {
    document.documentElement.classList.remove("dark");
  }
}

// Initialize output directory (desktop only - the web runtime has no output
// folder; results are downloaded through the browser).
if (!IS_WEB_RUNTIME) {
  invoke<string>("get_default_output_dir").then((dir) => {
    useStore.getState().updateSettings({ outputDirectory: dir });
  });
}

// Load saved theme preference. `localconvert_theme` (singular resolved
// value) is the old storage key from before "system" support existed;
// still read as a fallback so an existing install's choice carries over
// instead of silently resetting to the default.
const savedPreference =
  (localStorage.getItem("localconvert_theme_preference") as
    | "dark"
    | "light"
    | "system"
    | null) ?? (localStorage.getItem("localconvert_theme") as "dark" | "light" | null);

const initialPreference = savedPreference ?? "dark"; // matches the store's own default
const initialTheme = resolveTheme(initialPreference);
useStore.getState().updateSettings({
  themePreference: initialPreference,
  theme: initialTheme,
});
applyResolvedThemeToDocument(initialTheme);

// Live-update when the OS color scheme changes, but only while the user's
// preference is actually "system" - otherwise an explicit dark/light
// choice would get silently overridden by an OS-level toggle.
window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
  const { themePreference } = useStore.getState().settings;
  if (themePreference === "system") {
    const resolved = resolveTheme("system");
    useStore.getState().updateSettings({ theme: resolved });
  }
});

// Persist the preference and re-resolve (+ apply to <html class="dark">)
// whenever either the preference or the resolved theme changes. Uses a
// single direct setState rather than calling updateSettings a second time
// from inside the subscriber, so there's no dependence on how Zustand
// happens to order re-entrant notifications.
useStore.subscribe((state, prevState) => {
  if (state.settings.themePreference !== prevState.settings.themePreference) {
    localStorage.setItem("localconvert_theme_preference", state.settings.themePreference);
    const resolved = resolveTheme(state.settings.themePreference);
    if (resolved !== state.settings.theme) {
      useStore.setState((s) => ({ settings: { ...s.settings, theme: resolved } }));
      return; // the setState above re-enters this subscriber for the theme change
    }
  }
  if (state.settings.theme !== prevState.settings.theme) {
    applyResolvedThemeToDocument(state.settings.theme);
  }
});
