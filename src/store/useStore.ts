import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

// Image extensions that can be previewed
const PREVIEWABLE_IMAGE_EXTENSIONS = [
  "jpg", "jpeg", "png", "gif", "webp", "bmp", "ico", "svg", "avif"
];

// Helper to load image preview asynchronously
async function loadImagePreview(path: string): Promise<string | null> {
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
  showPrivacyBadge: boolean;
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
  setFileStatus: (
    id: string,
    status: ConversionFile["status"],
    progress?: number,
    error?: string,
    outputPath?: string,
    etaSecs?: number | null,
    speed?: number | null
  ) => void;
  reorderFiles: (files: ConversionFile[]) => void;

  // Category
  activeCategory: Category;
  setActiveCategory: (category: Category) => void;

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
  convertFiles: (options?: ConversionOptions) => Promise<void>;
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
      } else if (file.category === "video") {
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
    set((state) => ({
      files: state.files.filter((f) => f.id !== id),
      selectedFiles: state.selectedFiles.filter((fid) => fid !== id),
    }));
  },

  clearFiles: () => {
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

  setFileStatus: (id, status, progress = 0, error = undefined, outputPath = undefined, etaSecs = undefined, speed = undefined) => {
    set((state) => ({
      files: state.files.map((f) =>
        f.id === id
          ? {
              ...f,
              status,
              progress,
              error: error ?? f.error,
              outputPath: outputPath ?? f.outputPath,
              etaSecs: etaSecs ?? f.etaSecs,
              speed: speed ?? f.speed,
            }
          : f
      ),
    }));
  },

  reorderFiles: (newFiles) => {
    set({ files: newFiles });
  },

  // Category
  activeCategory: "all",
  setActiveCategory: (category) => set({ activeCategory: category }),

  // Tools
  tools: [],
  toolsChecked: false,

  checkTools: async () => {
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
      const capabilities = await invoke<Capability[]>("get_capabilities");
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
    const { files, selectedFiles, settings, globalOutputFormat, setFileStatus } =
      get();

    // Get pending files to convert (exclude already converting files)
    const filesToConvert = selectedFiles.length > 0
      ? files.filter((f) => selectedFiles.includes(f.id) && f.status === "pending")
      : files.filter((f) => f.status === "pending");

    if (filesToConvert.length === 0) return;

    // Add files to active conversions
    const newActiveConversions = new Set(get().activeConversions);
    filesToConvert.forEach(f => newActiveConversions.add(f.id));
    set({ isConverting: true, activeConversions: newActiveConversions });

    // Set up progress listener (only if not already listening)
    let unlisten: UnlistenFn | null = null;
    try {
      unlisten = await listen<ConversionProgressEvent>("conversion-progress", (event) => {
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
          return;
        }

        if (result.success) {
          setFileStatus(file.id, "completed", 100, undefined, result.output_path ?? undefined, null, null);
        } else {
          setFileStatus(file.id, "error", 0, result.error ?? "PROCESS_FAILED", undefined, null, null);
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
        } else {
          setFileStatus(file.id, "error", 0, errorStr, undefined, null, null);
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
  },

  cancelConversion: async (jobId?: string) => {
    const { activeConversions } = get();
    
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
    showPrivacyBadge: true,
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

// Initialize output directory
invoke<string>("get_default_output_dir").then((dir) => {
  useStore.getState().updateSettings({ outputDirectory: dir });
});

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
