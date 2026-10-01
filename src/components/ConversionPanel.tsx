import { memo, useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { motion, AnimatePresence } from "framer-motion";
import {
  Play,
  Pause,
  ChevronDown,
  Folder,
  Sliders,
  Zap,
  CheckCircle,
  AlertTriangle,
  Video,
  Lock,
  Unlock,
  Cpu,
  HardDrive,
  Volume2,
  Subtitles,
} from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import toast from "react-hot-toast";
import { useShallow } from "zustand/react/shallow";
import { useStore, ConversionPreset, GpuInfo, getFilesToConvert, getActiveConversionCandidates, commonOutputFormat } from "../store/useStore";
import { t, translateCapabilityState, describeUnavailableCapability } from "../locales";
import { getOutputFormats } from "../types/formats";
import { PresetsSelector } from "./PresetsSelector";
import { capabilityIdForOutputFormat, getBlockedCapability, isCapabilityUsable } from "../utils/capabilityGating";

// Step 3 perf pass (section D): zero props, so React.memo means this
// (large, animation-heavy) panel only re-renders from its own narrowed
// store subscription.
export const ConversionPanel = memo(function ConversionPanel() {
  // Step 3 perf pass (section D): narrowed from a full `useStore()`
  // subscription so this (large, animation-heavy) panel only re-renders
  // when one of these fields actually changes. `capabilities` is included
  // even though it's only read indirectly through `getCapability` - it
  // must stay part of the subscribed slice so the panel re-renders once
  // `loadCapabilities()` resolves at startup, otherwise the Office/video
  // capability gate (Step 3 manual-test fix) could keep showing its
  // pre-load "unknown = usable" state until something else happened to
  // trigger a render.
  const {
    files,
    selectedFiles,
    activeCategory,
    convertFiles,
    cancelConversion,
    settings,
    updateSettings,
    globalOutputFormat,
    setGlobalOutputFormat,
    setOutputFormatForFiles,
    getCapability,
  } = useStore(
    useShallow((s) => ({
      files: s.files,
      selectedFiles: s.selectedFiles,
      activeCategory: s.activeCategory,
      convertFiles: s.convertFiles,
      cancelConversion: s.cancelConversion,
      settings: s.settings,
      updateSettings: s.updateSettings,
      globalOutputFormat: s.globalOutputFormat,
      setGlobalOutputFormat: s.setGlobalOutputFormat,
      setOutputFormatForFiles: s.setOutputFormatForFiles,
      getCapability: s.getCapability,
      capabilities: s.capabilities,
    }))
  );

  const isDark = settings.theme === "dark";
  const [showOptions, setShowOptions] = useState(false);
  const [showFormatDropdown, setShowFormatDropdown] = useState(false);
  const [quality, setQuality] = useState(settings.defaultQuality);
  const [preserveMetadata, setPreserveMetadata] = useState(settings.preserveMetadata);
  const [selectedPreset, setSelectedPreset] = useState<ConversionPreset | null>(null);
  const [hwEncoders, setHwEncoders] = useState<GpuInfo | null>(null);

  useEffect(() => {
    invoke<GpuInfo>("get_hardware_encoders")
      .then(setHwEncoders)
      .catch(console.error);
  }, []);

  // Step 3 manual-test fix (finding #5/C): a preset picked for the
  // previous category (e.g. "YouTube Upload" while on Video) has no
  // meaning once the user switches category - drop the stale indicator
  // instead of leaving it displayed for an unrelated operation.
  useEffect(() => {
    setSelectedPreset(null);
  }, [activeCategory]);

  // Scoped to the active category so the panel always reflects the same
  // set of files the left-nav/FileList is currently showing (Step 3
  // manual-test fix, finding #5) - see getFilesToConvert's doc comment.
  const filesToConvert = getFilesToConvert({ files, selectedFiles, activeCategory });

  // Format-sync fix pass (finding #1): the value this panel displays and
  // highlights as "the" target format is resolved the exact same way
  // convertFiles resolves it for execution - per-file `outputFormat` first,
  // `globalOutputFormat` as fallback. Selecting a format here writes it to
  // every file currently in scope (setOutputFormatForFiles) instead of only
  // to `globalOutputFormat`, so the FileCard picker sitting next to each
  // file reflects the change immediately with no separate sync step.
  const selectedTargetFormat = commonOutputFormat(filesToConvert, globalOutputFormat);

  const handleSelectTargetFormat = (format: string) => {
    setOutputFormatForFiles(filesToConvert.map((f) => f.id), format);
    setGlobalOutputFormat(format);
    setShowFormatDropdown(false);
  };

  const handleSelectPreset = (preset: ConversionPreset) => {
    setSelectedPreset(preset);
    handleSelectTargetFormat(preset.outputFormat);
    setQuality(preset.quality);
    if (preset.options) {
      // Apply preset options
      setShowOptions(true);
    }
  };

  const isVideoContext = filesToConvert.some((f) => f.category === "video");

  // Honesty check (Step 3, section H): the native image pipeline's WebP
  // encoder is lossless-only (no libwebp in the dependency tree - see
  // src-tauri/src/native/image/encode.rs), so the quality slider has no
  // effect for a WebP target and must not be presented as if it does.
  const isWebpImageTarget = filesToConvert.some(
    (f) => f.category === "image" && (f.outputFormat || globalOutputFormat) === "webp"
  );

  const convertingFiles = files.filter((f) => f.status === "converting");
  const completedFiles = files.filter((f) => f.status === "completed");
  const errorFiles = files.filter((f) => f.status === "error");

  // Get unique formats from selected files
  const uniqueExtensions = [
    ...new Set(filesToConvert.map((f) => f.extension.toLowerCase())),
  ];
  
  // Get common output formats
  const commonFormats = uniqueExtensions.length === 1
    ? getOutputFormats(uniqueExtensions[0])
    : [];

  let estimatedSizeMb = 0;
  if (isVideoContext) {
    const totalBitrateMbps = settings.bitrateMode === "CBR" 
      ? settings.videoBitrate 
      : (settings.crf <= 18 ? 15 : settings.crf <= 23 ? 8 : 4);
    const audioBitrateMbps = settings.audioCodec === "No Audio" ? 0 : settings.audioBitrateKbps / 1000;
    const totalDuration = filesToConvert.reduce((acc, f) => acc + (f.duration || 0), 0);
    if (totalDuration > 0) {
      estimatedSizeMb = ((totalBitrateMbps + audioBitrateMbps) * totalDuration) / 8;
    }
  }

  // Step 4 UI-hardening pass (Bug 1): the warning banner (and the Convert
  // button's enabled state) must reflect only the ACTIVE candidates - see
  // getActiveConversionCandidates' doc comment - not every pending file
  // `filesToConvert` would implicitly batch. `filesToConvert` itself stays
  // unscoped by this so the stats/button label/estimated size still reflect
  // the real implicit batch Convert will attempt.
  const activeCandidates = getActiveConversionCandidates({
    files,
    selectedFiles,
    activeCategory,
    globalOutputFormat,
  });

  // Feature Availability Behavior (Step 3, section D/E/F): a file whose
  // conversion depends on a not-yet-bundled engine (Office, FFmpeg, ...)
  // must be gated BEFORE the backend ever spawns a process, not discovered
  // via a failed conversion. `blockedCapability` is the first
  // non-AVAILABLE capability found among the active candidates, if any.
  const blockedCapability = getBlockedCapability(activeCandidates, getCapability);

  const canConvert =
    filesToConvert.length > 0 &&
    filesToConvert.every((f) => f.outputFormat || globalOutputFormat) &&
    !blockedCapability;

  const handleSelectOutputDir = async () => {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
      });
      if (selected && typeof selected === "string") {
        updateSettings({ outputDirectory: selected });
      }
    } catch (error) {
      console.error("Failed to select directory:", error);
    }
  };

  const handleConvert = async () => {
    // Defense in depth: the Convert button is already disabled while
    // `blockedCapability` is set, but never rely solely on a disabled
    // button - re-check here so nothing can reach `convertFiles` (and a
    // real process spawn) for a file whose engine isn't available.
    if (blockedCapability) {
      toast.error(describeUnavailableCapability(blockedCapability));
      return;
    }
    if (!canConvert) {
      toast.error(t("workflow.selectOutputFormatForAll"));
      return;
    }

    try {
      const result = await convertFiles({
        quality,
        preserveMetadata,
        presetResolution: settings.presetResolution,
        customWidth: settings.customWidth,
        customHeight: settings.customHeight,
        videoCodec: settings.videoCodec,
        audioCodec: settings.audioCodec,
        bitrateMode: settings.bitrateMode,
        videoBitrate: settings.videoBitrate,
        crf: settings.crf,
        fps: settings.fps,
        twoPass: settings.twoPass,
        maintainAspectRatio: settings.maintainAspectRatio,
        audioBitrateKbps: settings.audioBitrateKbps,
        audioSampleRate: settings.audioSampleRate,
        volumeDb: settings.volumeDb,
        channelLayout: settings.channelLayout,
        subtitleAction: settings.subtitleAction.startsWith("Burn-in-") ? "Burn Into Video" : settings.subtitleAction,
        subtitleStreamIndex: settings.subtitleAction.startsWith("Burn-in-") ? parseInt(settings.subtitleAction.split("-").pop() || "0") : null,
      });

      // Step 4 UI-acceptance fix pass (Bug 1/B): a conversion attempt must
      // resolve to exactly ONE terminal UI state - convertFiles() never
      // rejects just because an individual file failed (convertSingleFile
      // catches its own errors and records the outcome in file.status), so
      // the toast decision must come from the batch's actual per-file
      // results, not from "the promise resolved without throwing". Never
      // show both a success and an error toast for the same attempt, and
      // never show success when at least one file failed.
      if (result.failed.length > 0) {
        toast.error(
          result.succeeded.length > 0
            ? t("workflow.someConversionsFailed")
            : t("notifications.processFailed")
        );
      } else if (result.succeeded.length > 0) {
        toast.success(t("workflow.conversionCompleted"));
      }
      // result.succeeded.length === 0 && result.failed.length === 0 means
      // every queued file was cancelled mid-flight - no toast either way.
    } catch (error) {
      toast.error(t("workflow.someConversionsFailed"));
    }
  };

  return (
    <div className="h-full flex flex-col glass-panel-heavy rounded-2xl border-0 shadow-lg overflow-hidden isolate relative z-10 w-full">
      {/* Header */}
      <div className={`p-5 border-b shrink-0 ${isDark ? "border-dark-700/50" : "border-dark-100/50"}`}>
        <h3 className={`text-[15px] font-bold tracking-wide uppercase flex items-center gap-2 ${isDark ? "text-white" : "text-dark-900"}`}>
          <div className="w-8 h-8 rounded-lg bg-accent-gradient flex items-center justify-center shadow-glow">
            <Zap className="w-4 h-4 text-white" />
          </div>
          {t("workflow.convert")} <span className="text-brand font-light">Panel</span>
        </h3>
      </div>

      {/* Content */}
      <div className="flex-1 p-5 overflow-y-auto space-y-6 custom-scrollbar">
        {/* Stats */}
        <div className="grid grid-cols-4 gap-3">
          <div className={`rounded-xl p-3 flex flex-col items-center justify-center transition-all duration-300 hover:-translate-y-0.5 hover:shadow-lg ${isDark ? "bg-gradient-to-br from-dark-800/80 to-dark-900/40 ring-1 ring-white/5 shadow-[0_4px_20px_rgba(0,0,0,0.2)]" : "bg-white/60 border border-dark-100 shadow-sm"}`}>
            <p className={`text-2xl font-black tracking-tight ${isDark ? "text-white drop-shadow-md" : "text-dark-900"}`}>{filesToConvert.length}</p>
            <p className={`text-[9px] font-black uppercase tracking-[0.15em] mt-0.5 ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.queued")}</p>
          </div>
          <div className={`rounded-xl p-3 flex flex-col items-center justify-center transition-all duration-300 hover:-translate-y-0.5 hover:shadow-lg ${isDark ? "bg-gradient-to-br from-brand/10 to-brand/5 ring-1 ring-brand/20 shadow-[0_4px_20px_rgba(var(--brand-color-rgb),0.15)]" : "bg-brand/5 border border-brand/20 shadow-sm"}`}>
            <p className="text-2xl font-black tracking-tight text-brand drop-shadow-[0_0_8px_rgba(var(--brand-color-rgb),0.5)]">{convertingFiles.length}</p>
            <p className={`text-[9px] font-black uppercase tracking-[0.15em] mt-0.5 ${isDark ? "text-brand/70" : "text-brand/80"}`}>{t("conversion.active")}</p>
          </div>
          <div className={`rounded-xl p-3 flex flex-col items-center justify-center transition-all duration-300 hover:-translate-y-0.5 hover:shadow-lg ${isDark ? "bg-gradient-to-br from-success-500/10 to-success-500/5 ring-1 ring-success-500/20 shadow-[0_4px_20px_rgba(34,197,94,0.15)]" : "bg-success-500/5 border border-success-500/20 shadow-sm"}`}>
            <p className="text-2xl font-black tracking-tight text-success-500 drop-shadow-[0_0_8px_rgba(34,197,94,0.5)]">{completedFiles.length}</p>
            <p className={`text-[9px] font-black uppercase tracking-[0.15em] mt-0.5 ${isDark ? "text-success-500/70" : "text-success-500/80"}`}>{t("conversion.done")}</p>
          </div>
          <div className={`rounded-xl p-3 flex flex-col items-center justify-center transition-all duration-300 hover:-translate-y-0.5 hover:shadow-lg ${isDark ? "bg-gradient-to-br from-error-500/10 to-error-500/5 ring-1 ring-error-500/20 shadow-[0_4px_20px_rgba(239,68,68,0.15)]" : "bg-error-500/5 border border-error-500/20 shadow-sm"}`}>
            <p className="text-2xl font-black tracking-tight text-error-500 drop-shadow-[0_0_8px_rgba(239,68,68,0.5)]">{errorFiles.length}</p>
            <p className={`text-[9px] font-black uppercase tracking-[0.15em] mt-0.5 ${isDark ? "text-error-500/70" : "text-error-500/80"}`}>{t("conversion.failed")}</p>
          </div>
        </div>

        {/* Global Format Selector */}
        {uniqueExtensions.length === 1 && (
          <div className="space-y-2">
            <label className={`text-[11px] font-bold tracking-widest uppercase ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.targetFormat")}</label>
            <div className="relative">
              <motion.button
                className={`w-full flex items-center justify-between px-4 py-3.5 rounded-xl text-left transition-all duration-300 hover:shadow-lg ${
                  isDark 
                    ? "bg-gradient-to-b from-white/5 to-transparent ring-1 ring-white/10 hover:ring-white/20" 
                    : "bg-white/60 border border-dark-200/50 hover:border-dark-300"
                } ${showFormatDropdown ? (isDark ? "!ring-brand/50 shadow-[0_0_15px_rgba(var(--brand-color-rgb),0.15)]" : "!border-brand") : ""}`}
                onClick={() => setShowFormatDropdown(!showFormatDropdown)}
                whileHover={{ scale: 1.01 }}
                whileTap={{ scale: 0.99 }}
              >
                <span className={selectedTargetFormat
                  ? `${isDark ? "text-white" : "text-dark-900"} font-bold tracking-wide`
                  : isDark ? "text-dark-500" : "text-dark-400 font-medium"
                }>
                  {selectedTargetFormat ? selectedTargetFormat.toUpperCase() : t("conversion.selectFormatPlaceholder")}
                </span>
                <div className={`p-1 rounded-md ${isDark ? "bg-dark-700/50" : "bg-dark-100"}`}>
                  <ChevronDown
                    className={`w-4 h-4 transition-transform ${isDark ? "text-dark-300" : "text-dark-600"} ${
                      showFormatDropdown ? "rotate-180 text-brand" : ""
                    }`}
                  />
                </div>
              </motion.button>

              <AnimatePresence>
                {showFormatDropdown && (
                  <motion.div
                    className={`absolute top-full left-0 right-0 mt-2 rounded-xl shadow-xl z-50 overflow-hidden backdrop-blur-xl border ${
                      isDark ? "bg-dark-800/90 border-dark-600" : "bg-white/90 border-dark-200"
                    }`}
                    initial={{ opacity: 0, scale: 0.95, y: -10 }}
                    animate={{ opacity: 1, scale: 1, y: 0 }}
                    exit={{ opacity: 0, scale: 0.95, y: -10 }}
                    transition={{ type: "spring", stiffness: 400, damping: 30 }}
                  >
                    <div className="p-2 max-h-56 overflow-y-auto custom-scrollbar flex flex-col gap-1">
                      {commonFormats.map((format) => {
                        const capId = capabilityIdForOutputFormat(format);
                        const blocked = capId ? !isCapabilityUsable(getCapability(capId)) : false;
                        return (
                          <motion.button
                            key={format}
                            className={`w-full text-left px-4 py-2.5 rounded-lg text-sm transition-colors flex justify-between items-center ${
                              blocked
                                ? isDark
                                  ? "text-dark-600 cursor-not-allowed opacity-50"
                                  : "text-dark-300 cursor-not-allowed opacity-50"
                                : selectedTargetFormat === format
                                  ? "bg-brand/10 text-brand font-bold"
                                  : isDark
                                    ? "text-dark-300 hover:bg-dark-700/50 hover:text-white"
                                    : "text-dark-600 hover:bg-dark-100 hover:text-dark-900"
                            }`}
                            onClick={() => {
                              if (blocked) return;
                              handleSelectTargetFormat(format);
                            }}
                            whileHover={blocked ? {} : { x: 2 }}
                            title={blocked ? t("workflow.engineMissingShort") : undefined}
                          >
                            <span className="uppercase tracking-wider">{format}</span>
                            {selectedTargetFormat === format && !blocked && <CheckCircle className="w-4 h-4" />}
                          </motion.button>
                        );
                      })}
                    </div>
                  </motion.div>
                )}
              </AnimatePresence>
            </div>
          </div>
        )}

        {/* Quick Presets */}
        {uniqueExtensions.length === 1 && (
          <PresetsSelector
            category={filesToConvert[0]?.category || "all"}
            onSelectPreset={handleSelectPreset}
          />
        )}

        {/* Selected Preset Indicator */}
        {selectedPreset && (
          <motion.div 
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: 'auto' }}
            className={`flex items-center justify-between px-4 py-3 rounded-xl border ${
              isDark ? "bg-amber-500/5 border-amber-500/20" : "bg-amber-50 border-amber-200/50"
            }`}
          >
            <div className="flex flex-col">
              <span className={`text-[10px] font-bold uppercase tracking-wider ${isDark ? "text-amber-500/70" : "text-amber-600/70"}`}>
                {t("conversion.activePreset")}
              </span>
              <span className={`text-sm font-semibold ${isDark ? "text-amber-400" : "text-amber-700"}`}>
                {selectedPreset.name}
              </span>
            </div>
            <button
              className={`px-3 py-1.5 rounded-lg text-xs font-bold transition-all ${
                isDark ? "bg-amber-500/10 hover:bg-amber-500/20 text-amber-500" : "bg-amber-100 hover:bg-amber-200 text-amber-700"
              }`}
              onClick={() => setSelectedPreset(null)}
            >
              {t("conversion.clear")}
            </button>
          </motion.div>
        )}

        {/* Output Directory */}
        <div className="space-y-2">
          <label className={`text-[11px] font-bold tracking-widest uppercase ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.destination")}</label>
          <motion.button
            className={`w-full flex items-center justify-between px-4 py-3.5 rounded-xl text-left transition-all duration-300 group hover:shadow-lg ${
              isDark ? "bg-gradient-to-b from-white/5 to-transparent ring-1 ring-white/10 hover:ring-brand/50 hover:shadow-[0_0_15px_rgba(var(--brand-color-rgb),0.1)]" : "bg-white/60 border border-dark-200/50 hover:border-brand"
            }`}
            onClick={handleSelectOutputDir}
            whileHover={{ scale: 1.01 }}
            whileTap={{ scale: 0.99 }}
          >
            <div className="flex items-center gap-3 overflow-hidden">
              <Folder className={`w-5 h-5 group-hover:text-brand transition-colors shrink-0 ${
                isDark ? "text-dark-400" : "text-dark-500"
              }`} />
              <span className={`text-sm truncate font-medium ${isDark ? "text-dark-300" : "text-dark-700"}`}>
                {settings.outputDirectory || t("conversion.selectFolderPlaceholder")}
              </span>
            </div>
          </motion.button>
        </div>

        {/* Advanced Options Toggle */}
        <motion.button
          className={`w-full flex items-center justify-between px-4 py-3.5 rounded-xl transition-all duration-300 hover:shadow-lg ${
            isDark ? "bg-gradient-to-b from-white/5 to-transparent ring-1 ring-white/10 hover:ring-brand/50" : "bg-white/40 border border-dark-200/50 hover:bg-white/70 hover:border-brand"
          } ${showOptions ? (isDark ? "!ring-brand/50 shadow-[0_0_15px_rgba(var(--brand-color-rgb),0.15)]" : "!border-brand") : ""}`}
          onClick={() => setShowOptions(!showOptions)}
          whileHover={{ scale: 1.01 }}
        >
          <span className={`flex items-center gap-3 text-sm font-semibold tracking-wide ${isDark ? "text-dark-200" : "text-dark-600"}`}>
            <div className={`p-1.5 rounded-md transition-colors ${isDark ? "bg-brand/20 text-brand ring-1 ring-brand/30" : "bg-brand/10 text-brand"}`}>
              <Sliders className="w-4 h-4" />
            </div>
            {t("conversion.advancedConfiguration")}
          </span>
          <ChevronDown
            className={`w-4 h-4 transition-transform ${isDark ? "text-dark-500" : "text-dark-400"} ${
              showOptions ? "rotate-180" : ""
            }`}
          />
        </motion.button>

        {/* Advanced Options */}
        <AnimatePresence>
          {showOptions && (
            <motion.div
              initial={{ opacity: 0, height: 0, y: -10 }}
              animate={{ opacity: 1, height: "auto", y: 0 }}
              exit={{ opacity: 0, height: 0, y: -10 }}
              className="space-y-6 overflow-hidden pt-2"
            >
              {/* Quality Slider */}
              <div className={`space-y-3 p-4 rounded-xl border ${isDark ? "bg-dark-800/30 border-dark-700/50" : "bg-white/50 border-dark-100"}`}>
                <div className="flex justify-between items-center">
                  <label className={`text-[11px] font-bold tracking-widest uppercase ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.conversionQuality")}</label>
                  <span className="text-sm font-bold bg-brand/10 text-brand px-2 py-0.5 rounded-md">{quality}%</span>
                </div>
                <input
                  type="range"
                  min="10"
                  max="100"
                  value={quality}
                  onChange={(e) => setQuality(Number(e.target.value))}
                  className={`w-full h-2 rounded-lg appearance-none cursor-pointer accent-brand ${
                    isDark ? "bg-dark-700" : "bg-dark-200"
                  }`}
                />
                <div className={`flex justify-between text-[10px] font-bold uppercase tracking-wider ${isDark ? "text-dark-500" : "text-dark-400"}`}>
                  <span>{t("conversion.fastCompressed")}</span>
                  <span>{t("conversion.highDetail")}</span>
                </div>
                {isWebpImageTarget && (
                  <p className={`text-[10px] ${isDark ? "text-amber-500/80" : "text-amber-600"}`}>
                    {t("conversion.webpLosslessNote")}
                  </p>
                )}
              </div>

              {/* Preserve Metadata */}
              <label className={`flex items-center justify-between p-4 rounded-xl border cursor-pointer hover:border-brand/50 transition-colors ${
                isDark ? "bg-dark-800/30 border-dark-700/50" : "bg-white/50 border-dark-100"
              }`}>
                <span className={`text-sm font-semibold ${isDark ? "text-dark-300" : "text-dark-700"}`}>{t("conversion.preserveMetadata")}</span>
                <div
                  className={`w-12 h-6 rounded-full transition-colors relative shadow-inner ${
                    preserveMetadata ? "bg-brand" : isDark ? "bg-dark-700" : "bg-dark-200"
                  }`}
                  onClick={() => setPreserveMetadata(!preserveMetadata)}
                >
                  <motion.div
                    className="absolute top-1 w-4 h-4 bg-white rounded-full shadow-md"
                    animate={{ left: preserveMetadata ? 28 : 4 }}
                    transition={{ type: "spring", stiffness: 500, damping: 30 }}
                  />
                </div>
              </label>

              {/* Advanced Video Configuration */}
              {isVideoContext && (
                <div className={`p-4 rounded-xl border space-y-4 ${isDark ? "bg-dark-800/30 border-dark-700/50" : "bg-white/50 border-dark-100"}`}>
                  <h4 className={`text-xs font-bold uppercase tracking-wider flex items-center gap-2 ${isDark ? "text-dark-300" : "text-dark-600"}`}>
                    <Video className="w-4 h-4" /> {t("conversion.videoOptions")}
                  </h4>

                  {/* Hardware Acceleration Toggle */}
                  <label className={`flex items-center justify-between p-3 rounded-lg border cursor-pointer hover:border-brand/50 transition-colors ${
                    isDark ? "bg-dark-800/50 border-dark-700" : "bg-white/70 border-dark-200"
                  }`}>
                    <div>
                      <span className={`text-sm font-semibold flex items-center gap-2 ${isDark ? "text-dark-300" : "text-dark-700"}`}>
                        <Cpu className="w-4 h-4 text-brand" /> {t("conversion.hardwareAcceleration")}
                      </span>
                      <span className={`block text-[10px] mt-1 ${isDark ? "text-dark-500" : "text-dark-400"}`}>
                         {hwEncoders && hwEncoders.available ? `${t("conversion.detected")}: ${hwEncoders.encoders.map(e => e.name).join(', ')}` : t("conversion.noGpuEncoderDetected")}
                      </span>
                    </div>
                    <div
                      className={`w-10 h-5 rounded-full transition-colors relative shadow-inner shrink-0 ${
                        settings.hwAcceleratorEnabled && hwEncoders?.available ? "bg-brand" : isDark ? "bg-dark-700" : "bg-dark-200"
                      } ${!hwEncoders?.available ? "opacity-50 cursor-not-allowed" : ""}`}
                      onClick={(e) => {
                        e.preventDefault();
                        if (hwEncoders?.available) updateSettings({ hwAcceleratorEnabled: !settings.hwAcceleratorEnabled, useGpu: !settings.hwAcceleratorEnabled });
                      }}
                    >
                      <motion.div
                        className="absolute top-0.5 w-4 h-4 bg-white rounded-full shadow-md"
                        animate={{ left: settings.hwAcceleratorEnabled && hwEncoders?.available ? 22 : 2 }}
                        transition={{ type: "spring", stiffness: 500, damping: 30 }}
                      />
                    </div>
                  </label>

                  <div className="grid grid-cols-2 gap-4">
                    {/* Resolution */}
                    <div className="space-y-1.5">
                      <label className={`text-[10px] font-bold uppercase tracking-wider ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.resolution")}</label>
                      <select
                        value={settings.presetResolution}
                        onChange={(e) => updateSettings({ presetResolution: e.target.value })}
                        className={`w-full p-2.5 rounded-lg text-sm font-semibold outline-none transition-all ${
                          isDark ? "bg-dark-900 border border-dark-700 text-white focus:border-brand" : "bg-white border border-dark-200 text-dark-900 focus:border-brand"
                        }`}
                      >
                        <option value="Match Source">Match Source</option>
                        <option value="4K">4K (2160p)</option>
                        <option value="1080p">1080p (FHD)</option>
                        <option value="720p">720p (HD)</option>
                        <option value="480p">480p (SD)</option>
                        <option value="Custom">Custom</option>
                      </select>
                    </div>

                    {/* Frame Rate */}
                    <div className="space-y-1.5">
                      <label className={`text-[10px] font-bold uppercase tracking-wider ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.frameRate")}</label>
                      <select
                        value={settings.fps}
                        onChange={(e) => updateSettings({ fps: e.target.value })}
                        className={`w-full p-2.5 rounded-lg text-sm font-medium outline-none transition-all cursor-pointer ${
                          isDark ? "bg-dark-800 border border-dark-700 text-dark-100 hover:border-brand/50 focus:border-brand focus:ring-1 focus:ring-brand shadow-inner" : "bg-white border border-dark-200 text-dark-900 hover:border-brand/50 focus:border-brand focus:ring-1 focus:ring-brand"
                        }`}
                      >
                        <option value="Match Source">Match Source</option>
                        <option value="24">24 fps</option>
                        <option value="30">30 fps</option>
                        <option value="60">60 fps</option>
                      </select>
                    </div>
                  </div>

                  {/* Custom Resolution Inputs */}
                  {settings.presetResolution === "Custom" && (
                    <div className="grid grid-cols-[1fr_auto_1fr] gap-2 items-end">
                      <div className="space-y-1.5">
                        <label className={`text-[10px] font-bold uppercase tracking-wider ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.widthPx")}</label>
                        <input
                          type="number"
                          value={settings.customWidth}
                          onChange={(e) => updateSettings({ customWidth: parseInt(e.target.value) || 1920 })}
                          className={`w-full p-2.5 rounded-lg text-sm font-semibold outline-none transition-all ${
                            isDark ? "bg-dark-900 border border-dark-700 text-white focus:border-brand" : "bg-white border border-dark-200 text-dark-900 focus:border-brand"
                          }`}
                        />
                      </div>
                      
                      <button
                        className={`mb-1 p-2.5 rounded-lg border transition-all ${
                          settings.maintainAspectRatio 
                            ? "bg-brand/10 border-brand text-brand" 
                            : isDark ? "bg-dark-800 border-dark-700 text-dark-400" : "bg-dark-50 border-dark-200 text-dark-500"
                        }`}
                        onClick={() => updateSettings({ maintainAspectRatio: !settings.maintainAspectRatio })}
                        title={settings.maintainAspectRatio ? t("conversion.unlockAspectRatio") : t("conversion.lockAspectRatio")}
                      >
                        {settings.maintainAspectRatio ? <Lock className="w-4 h-4" /> : <Unlock className="w-4 h-4" />}
                      </button>

                      <div className="space-y-1.5">
                        <label className={`text-[10px] font-bold uppercase tracking-wider ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.heightPx")}</label>
                        <input
                          type="number"
                          value={settings.customHeight}
                          onChange={(e) => updateSettings({ customHeight: parseInt(e.target.value) || 1080 })}
                          className={`w-full p-2.5 rounded-lg text-sm font-semibold outline-none transition-all ${
                            isDark ? "bg-dark-900 border border-dark-700 text-white focus:border-brand" : "bg-white border border-dark-200 text-dark-900 focus:border-brand"
                          }`}
                        />
                      </div>
                    </div>
                  )}

                  <div className="grid grid-cols-2 gap-4">
                    {/* Video Codec */}
                    <div className="space-y-1.5">
                      <label className={`text-[10px] font-bold uppercase tracking-wider flex items-center gap-1 ${isDark ? "text-dark-400" : "text-dark-500"}`}>
                        {t("conversion.videoCodec")}
                        {settings.videoCodec === "AV1" && (
                          <div title="AV1 encoding without hardware acceleration is significantly slower than H.264/H.265.">
                            <AlertTriangle className="w-3 h-3 text-amber-500 cursor-help" />
                          </div>
                        )}
                      </label>
                      <select
                        value={settings.videoCodec}
                        onChange={(e) => updateSettings({ videoCodec: e.target.value })}
                        className={`w-full p-2.5 rounded-lg text-sm font-medium outline-none transition-all cursor-pointer ${
                          isDark ? "bg-dark-800 border border-dark-700 text-dark-100 hover:border-brand/50 focus:border-brand focus:ring-1 focus:ring-brand shadow-inner" : "bg-white border border-dark-200 text-dark-900 hover:border-brand/50 focus:border-brand focus:ring-1 focus:ring-brand"
                        }`}
                      >
                        <option value="H.264">H.264</option>
                        <option value="H.265">H.265 / HEVC</option>
                        <option value="VP9">VP9</option>
                        <option value="AV1">AV1</option>
                      </select>
                    </div>

                    {/* Audio Settings Integration */}
                  </div>
                  
                  {/* Audio Settings Expansion */}
                  <div className={`border-t pt-4 ${isDark ? "border-dark-700/50" : "border-dark-200"}`}>
                    <h4 className={`text-xs font-bold uppercase tracking-wider flex items-center gap-2 mb-3 ${isDark ? "text-dark-300" : "text-dark-600"}`}>
                      <Volume2 className="w-4 h-4" /> {t("conversion.audioSubtitles")}
                    </h4>
                    <div className="grid grid-cols-2 gap-4 mb-4">
                      {/* Audio Codec */}
                      <div className="space-y-1.5">
                        <label className={`text-[10px] font-bold uppercase tracking-wider flex items-center gap-1 ${isDark ? "text-dark-400" : "text-dark-500"}`}>
                          {t("conversion.audioCodec")}
                        </label>
                        <select
                          value={settings.audioCodec}
                          onChange={(e) => updateSettings({ audioCodec: e.target.value })}
                          className={`w-full p-2.5 rounded-lg text-sm font-semibold outline-none transition-all ${
                            isDark ? "bg-dark-900 border border-dark-700 text-white focus:border-brand" : "bg-white border border-dark-200 text-dark-900 focus:border-brand"
                          }`}
                        >
                          <option value="AAC">AAC</option>
                          <option value="MP3">MP3</option>
                          <option value="Opus">Opus</option>
                          <option value="No Audio">No Audio</option>
                        </select>
                      </div>

                      {/* Channels */}
                      <div className="space-y-1.5">
                        <label className={`text-[10px] font-bold uppercase tracking-wider ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.channels")}</label>
                        <select
                          value={settings.channelLayout}
                          onChange={(e) => updateSettings({ channelLayout: e.target.value })}
                          disabled={settings.audioCodec === "No Audio"}
                          className={`w-full p-2.5 rounded-lg text-sm font-semibold outline-none transition-all disabled:opacity-50 ${isDark ? "bg-dark-900 border border-dark-700 text-white" : "bg-white border border-dark-200 text-dark-900"}`}
                        >
                          <option value="Auto">Auto</option>
                          <option value="Stereo">Stereo</option>
                          <option value="Mono">Mono</option>
                          <option value="5.1">5.1 Surround</option>
                        </select>
                      </div>
                    </div>

                    <div className="grid grid-cols-2 gap-4 mb-4">
                      <div className="space-y-1.5">
                        <label className={`text-[10px] font-bold uppercase tracking-wider ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.sampleRate")}</label>
                        <select
                          value={settings.audioSampleRate}
                          onChange={(e) => updateSettings({ audioSampleRate: e.target.value })}
                          disabled={settings.audioCodec === "No Audio"}
                          className={`w-full p-2.5 rounded-lg text-sm font-semibold outline-none transition-all disabled:opacity-50 ${isDark ? "bg-dark-900 border border-dark-700 text-white" : "bg-white border border-dark-200 text-dark-900"}`}
                        >
                          <option value="Match Source">Match Source</option>
                          <option value="44.1kHz">44.1 kHz</option>
                          <option value="48kHz">48 kHz</option>
                        </select>
                      </div>

                      {/* Subtitles Input */}
                      <div className="space-y-1.5">
                        <label className={`text-[10px] font-bold uppercase tracking-wider flex items-center gap-1 ${isDark ? "text-dark-400" : "text-dark-500"}`}>
                          <Subtitles className="w-3 h-3" /> {t("conversion.subtitles")}
                        </label>
                        <select
                          value={settings.subtitleAction}
                          onChange={(e) => updateSettings({ subtitleAction: e.target.value, subtitleStreamIndex: null })}
                          className={`w-full p-2.5 rounded-lg text-sm font-semibold outline-none transition-all ${
                            isDark ? "bg-dark-900 border border-dark-700 text-white focus:border-brand" : "bg-white border border-dark-200 text-dark-900 focus:border-brand"
                          }`}
                        >
                          <option value="No Change">{t("conversion.noChangeCopyTracks")}</option>
                          <option value="Strip All">{t("conversion.stripAll")}</option>
                          {(() => {
                            const firstAudioWithSubs = filesToConvert.find(f => f.subtitles && f.subtitles.length > 0);
                            if (firstAudioWithSubs?.subtitles) {
                               return firstAudioWithSubs.subtitles.map((sub, i) => (
                                  <option key={sub.index} value={`Burn-in-${sub.index}`}>
                                    {t("conversion.burnIn")}: {sub.title || sub.language || `${t("conversion.stream")} ${i + 1}`} ({sub.codec})
                                  </option>
                               ));
                            }
                            return null;
                          })()}
                        </select>
                      </div>
                    </div>

                    <div className="grid grid-cols-2 gap-4">
                      {/* Audio Bitrate */}
                      <div className="space-y-3 pt-1">
                          <div className="flex justify-between items-center gap-2">
                            <label className={`text-[10px] font-bold uppercase tracking-wider truncate min-w-0 ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.audioBitrate")}</label>
                            <span className="text-[11px] font-bold text-brand bg-brand/10 px-1.5 py-0.5 rounded whitespace-nowrap shrink-0">{settings.audioBitrateKbps} kbps</span>
                          </div>
                          <input
                            type="range" min="64" max="320" step="32"
                            value={settings.audioBitrateKbps}
                            onChange={(e) => updateSettings({ audioBitrateKbps: Number(e.target.value) })}
                            disabled={settings.audioCodec === "No Audio"}
                            className={`w-full h-2 rounded-lg appearance-none cursor-pointer accent-brand disabled:opacity-50 ${isDark ? "bg-dark-700" : "bg-dark-200"}`}
                          />
                      </div>

                      {/* Volume Adjust */}
                      <div className="space-y-3 pt-1">
                          <div className="flex justify-between items-center gap-2">
                            <label className={`text-[10px] font-bold uppercase tracking-wider truncate min-w-0 ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.volume")}</label>
                            <span className="text-[11px] font-bold text-brand bg-brand/10 px-1.5 py-0.5 rounded whitespace-nowrap shrink-0">{settings.volumeDb > 0 ? "+" : ""}{settings.volumeDb} dB</span>
                          </div>
                          <input
                            type="range" min="-20" max="20"
                            value={settings.volumeDb}
                            onChange={(e) => updateSettings({ volumeDb: Number(e.target.value) })}
                            disabled={settings.audioCodec === "No Audio"}
                            className={`w-full h-2 rounded-lg appearance-none cursor-pointer accent-brand disabled:opacity-50 ${isDark ? "bg-dark-700" : "bg-dark-200"}`}
                          />
                      </div>
                    </div>
                  </div>

                  {/* Bitrate Mode Toggle */}
                  <div className="flex items-center justify-between gap-4 pt-2">
                    <label className={`text-[10px] font-bold uppercase tracking-wider whitespace-nowrap ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("conversion.bitrateMode")}</label>
                    <div className={`flex rounded-lg overflow-hidden border h-8 ${isDark ? "border-dark-700" : "border-dark-200"}`}>
                      <button 
                        onClick={() => updateSettings({ bitrateMode: "VBR" })}
                        className={`px-3 text-[11px] font-bold transition-all whitespace-nowrap flex items-center justify-center ${settings.bitrateMode === "VBR" ? "bg-brand text-white" : isDark ? "bg-dark-800 text-dark-400 hover:bg-dark-700" : "bg-white text-dark-500 hover:bg-dark-50"}`}
                      >
                        VBR (CRF)
                      </button>
                      <button 
                        onClick={() => updateSettings({ bitrateMode: "CBR" })}
                        className={`px-3 text-[11px] font-bold transition-all whitespace-nowrap flex items-center justify-center ${settings.bitrateMode === "CBR" ? "bg-brand text-white" : isDark ? "bg-dark-800 text-dark-400 hover:bg-dark-700" : "bg-white text-dark-500 hover:bg-dark-50"}`}
                      >
                        CBR (Mbps)
                      </button>
                    </div>
                  </div>

                  {/* Dynamic Slider for CRF or Bitrate */}
                  <div className="space-y-3 pt-2">
                    <div className="flex justify-between items-center">
                      <label className={`text-[10px] font-bold tracking-widest uppercase ${isDark ? "text-dark-400" : "text-dark-500"}`}>
                        {settings.bitrateMode === "VBR" ? t("conversion.crf") : t("conversion.targetBitrate")}
                      </label>
                      <span className="text-sm font-bold bg-brand/10 text-brand px-2 py-0.5 rounded-md">
                        {settings.bitrateMode === "VBR" ? settings.crf : `${settings.videoBitrate} Mbps`}
                      </span>
                    </div>
                    {settings.bitrateMode === "VBR" ? (
                       <>
                        <input
                          type="range"
                          min="0"
                          max="51"
                          value={settings.crf}
                          onChange={(e) => updateSettings({ crf: Number(e.target.value) })}
                          className={`w-full h-2 rounded-lg appearance-none cursor-pointer accent-brand ${isDark ? "bg-dark-700" : "bg-dark-200"}`}
                        />
                        <div className={`flex justify-between text-[10px] font-bold uppercase tracking-wider ${isDark ? "text-dark-500" : "text-dark-400"}`}>
                          <span>{t("conversion.lossless")}</span>
                          <span>{t("conversion.lowest")}</span>
                        </div>
                       </>
                    ) : (
                       <>
                        <input
                          type="range"
                          min="1"
                          max="50"
                          value={settings.videoBitrate}
                          onChange={(e) => updateSettings({ videoBitrate: Number(e.target.value) })}
                          className={`w-full h-2 rounded-lg appearance-none cursor-pointer accent-brand ${isDark ? "bg-dark-700" : "bg-dark-200"}`}
                        />
                        <div className={`flex justify-between text-[10px] font-bold uppercase tracking-wider ${isDark ? "text-dark-500" : "text-dark-400"}`}>
                          <span>1 Mbps</span>
                          <span>50 Mbps</span>
                        </div>
                       </>
                    )}
                  </div>

                  {/* 2-Pass Encoding Toggle */}
                  <label className={`flex items-center justify-between py-2 cursor-pointer group ${settings.bitrateMode === "VBR" ? "opacity-50" : ""}`}>
                    <span className={`text-sm font-semibold ${isDark ? "text-dark-300" : "text-dark-700"}`}>
                      {t("conversion.twoPassEncoding")}
                      <span className={`block text-[10px] mt-0.5 uppercase tracking-wider ${isDark ? "text-dark-500" : "text-dark-400"}`}>
                         {settings.bitrateMode === "VBR" ? t("conversion.twoPassUnavailableVbr") : t("conversion.twoPassDescription")}
                      </span>
                    </span>
                    <div
                      className={`w-12 h-6 rounded-full transition-colors relative shadow-inner shrink-0 ${
                        settings.twoPass && settings.bitrateMode !== "VBR" ? "bg-brand" : isDark ? "bg-dark-700" : "bg-dark-200"
                      }`}
                      onClick={() => settings.bitrateMode !== "VBR" && updateSettings({ twoPass: !settings.twoPass })}
                    >
                      <motion.div
                        className="absolute top-1 w-4 h-4 bg-white rounded-full shadow-md"
                        animate={{ left: settings.twoPass && settings.bitrateMode !== "VBR" ? 28 : 4 }}
                        transition={{ type: "spring", stiffness: 500, damping: 30 }}
                      />
                    </div>
                  </label>
                </div>
              )}
            </motion.div>
          )}
        </AnimatePresence>

        {/* Estimated Output Size - Displayed when files are queued and it's video */}
        {isVideoContext && filesToConvert.length > 0 && estimatedSizeMb > 0 && (
          <div className={`p-4 rounded-r-xl rounded-l-md flex items-center justify-between border-l-4 border-l-brand relative overflow-hidden ${isDark ? "bg-gradient-to-r from-brand/10 via-brand/5 to-transparent ring-1 ring-white/5" : "bg-gradient-to-r from-brand/5 to-white border border-brand/20"}`}>
            <div className="absolute inset-0 bg-[linear-gradient(90deg,transparent_0%,rgba(255,255,255,0.05)_50%,transparent_100%)] w-[200%] animate-[shimmer_3s_infinite_linear] opacity-30"></div>
            <div className="flex items-center gap-3 relative z-10 min-w-0 flex-1">
               <div className={`p-2 rounded-lg shadow-sm shrink-0 ${isDark ? "bg-brand/20 text-brand ring-1 ring-brand/30" : "bg-brand/10 text-brand"}`}>
                 <HardDrive className="w-5 h-5" />
               </div>
               <div className="min-w-0">
                 <h4 className={`text-sm font-black tracking-wide truncate ${isDark ? "text-white drop-shadow-sm" : "text-dark-700"}`}>{t("conversion.estimatedOutputSize")}</h4>
                 <p className={`text-[9px] font-bold uppercase tracking-[0.1em] mt-0.5 truncate ${isDark ? "text-dark-400" : "text-brand/60"}`}>
                   {t("conversion.estimatedOutputBasis")}
                 </p>
               </div>
            </div>
            <div className="text-right flex flex-col items-end relative z-10 ml-3 shrink-0">
               <span className="text-2xl font-black tracking-tighter text-brand drop-shadow-[0_2px_12px_rgba(var(--brand-color-rgb),0.6)] brightness-110 whitespace-nowrap">
                 {estimatedSizeMb < 1000 ? `${estimatedSizeMb.toFixed(1)} MB` : `${(estimatedSizeMb / 1024).toFixed(2)} GB`}
               </span>
               <span className={`block text-[9px] uppercase font-black tracking-[0.2em] mt-0.5 whitespace-nowrap ${isDark ? "text-brand/60" : "text-brand/80"}`}>
                 {t("conversion.total")}
               </span>
            </div>
          </div>
        )}
      </div>

      {/* Footer / CTA Area */}
      <div className={`p-5 border-t shrink-0 ${isDark ? "bg-dark-800/80 border-dark-700/50" : "bg-white border-dark-100/50"} shadow-xl`}>
        {/* Convert Button - always show if there are pending files with formats */}
        {filesToConvert.length > 0 && (
          <motion.button
            className={`relative w-full py-4 rounded-xl font-bold tracking-wide flex items-center justify-center gap-3 overflow-hidden transition-all ${
              canConvert
                ? "bg-accent-gradient text-white btn-glow shadow-md hover:shadow-glow-strong"
                : isDark
                  ? "bg-dark-700 text-dark-500 cursor-not-allowed border border-dark-600"
                  : "bg-dark-100 text-dark-400 cursor-not-allowed border border-dark-200"
            }`}
            onClick={handleConvert}
            disabled={!canConvert}
            whileHover={canConvert ? { scale: 1.02 } : {}}
            whileTap={canConvert ? { scale: 0.98 } : {}}
          >
            {canConvert && (
              <motion.div 
                className="absolute inset-0 bg-white/20"
                initial={{ x: "-100%" }}
                whileHover={{ x: "100%" }}
                transition={{ duration: 0.5, ease: "easeInOut" }}
              />
            )}
            <Play className={`w-5 h-5 ${canConvert ? "fill-white" : ""}`} />
            {t("workflow.processLabel")} {filesToConvert.length} {t("workflow.fileWord")}
          </motion.button>
        )}

        {/* Cancel Button - show when there are active conversions */}
        {convertingFiles.length > 0 && (
          <motion.button
            className="w-full py-4 mt-3 bg-error-500/10 hover:bg-error-500/20 border border-error-500/30 rounded-xl text-error-500 font-bold tracking-wide flex items-center justify-center gap-2 transition-colors shadow-sm"
            onClick={() => cancelConversion()}
            whileHover={{ scale: 1.02 }}
            whileTap={{ scale: 0.98 }}
          >
            <Pause className="w-5 h-5 fill-error-500" />
            {t("workflow.abortLabel")} {convertingFiles.length} {t("workflow.activeLabel")}
          </motion.button>
        )}

        {/* Show message when no pending files but conversions are running */}
        {filesToConvert.length === 0 && convertingFiles.length === 0 && files.length > 0 && (
          <div className="py-4 flex justify-center">
            {completedFiles.length > 0 ? (
              <div className={`flex items-center gap-3 px-4 py-2 rounded-lg font-bold text-sm ${
                isDark ? "bg-success-500/10 text-success-500 border border-success-500/20" : "bg-success-50 text-success-600 border border-success-200/50"
              }`}>
                <CheckCircle className="w-5 h-5" />
                {t("workflow.allOperationsComplete")}
              </div>
            ) : (
              <span className={`text-[11px] font-bold uppercase tracking-widest ${isDark ? "text-dark-500" : "text-dark-400"}`}>
                {t("workflow.awaitingFiles")}
              </span>
            )}
          </div>
        )}

        {/* Warnings */}
        {blockedCapability && filesToConvert.length > 0 && (
          <motion.div
            className={`mt-4 flex items-start gap-2 p-3 rounded-lg text-xs font-semibold ${
              isDark ? "bg-amber-500/10 text-amber-500 border border-amber-500/20" : "bg-amber-50 text-amber-600/90 border border-amber-200"
            }`}
            initial={{ opacity: 0, y: 5 }}
            animate={{ opacity: 1, y: 0 }}
          >
            <AlertTriangle className="w-4 h-4 shrink-0 mt-0.5" />
            <span>
              {translateCapabilityState(blockedCapability.state)}
              {" — "}
              {t("workflow.featureUnavailableTitle")}.
            </span>
          </motion.div>
        )}

        {!canConvert && !blockedCapability && filesToConvert.length > 0 && (
          <motion.div
            className={`mt-4 flex items-center justify-center gap-2 p-3 rounded-lg text-xs font-semibold ${
              isDark ? "bg-amber-500/10 text-amber-500 border border-amber-500/20" : "bg-amber-50 text-amber-600/90 border border-amber-200"
            }`}
            initial={{ opacity: 0, y: 5 }}
            animate={{ opacity: 1, y: 0 }}
          >
            <AlertTriangle className="w-4 h-4" />
            {t("workflow.targetFormatRequired")}
          </motion.div>
        )}
      </div>

      {/* Click outside to close dropdown */}
      {showFormatDropdown && (
        <div
          className="fixed inset-0 z-40"
          onClick={() => setShowFormatDropdown(false)}
        />
      )}
    </div>
  );
});
