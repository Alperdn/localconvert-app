import { memo, useCallback, useMemo } from "react";
import { motion, AnimatePresence, Reorder, useDragControls } from "framer-motion";
import { Plus, Trash2, CheckSquare, Square, Upload, GripVertical, Layers } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../store/useStore";
import { FileCard } from "./FileCard";
import type { FileInfo, ConversionFile } from "../store/useStore";
import { t } from "../locales";

// Step 3 perf pass (section D): takes no props - React.memo means FileList
// only re-renders from its own narrowed store subscription, not merely
// because App.tsx re-rendered for something unrelated.
export const FileList = memo(function FileList() {
  // Step 3 perf pass (section D): narrowed from a full `useStore()` -
  // FileList renders the whole file grid, so an unnecessary re-render here
  // is one of the more expensive ones in the app.
  const {
    files,
    activeCategory,
    selectedFiles,
    selectAllFiles,
    deselectAllFiles,
    clearFiles,
    addFiles,
    reorderFiles,
    theme,
  } = useStore(
    useShallow((s) => ({
      files: s.files,
      activeCategory: s.activeCategory,
      selectedFiles: s.selectedFiles,
      selectAllFiles: s.selectAllFiles,
      deselectAllFiles: s.deselectAllFiles,
      clearFiles: s.clearFiles,
      addFiles: s.addFiles,
      reorderFiles: s.reorderFiles,
      theme: s.settings.theme,
    }))
  );

  const isDark = theme === "dark";

  // For reordering, we need to work with the full files array
  const filteredFiles = useMemo(() => 
    activeCategory === "all"
      ? files
      : files.filter((f) => f.category === activeCategory),
    [files, activeCategory]
  );

  const handleReorder = useCallback((reorderedFiltered: ConversionFile[]) => {
    if (activeCategory === "all") {
      // Direct reorder when showing all files
      reorderFiles(reorderedFiltered);
    } else {
      // When filtered, we need to merge back into the original array
      const filteredIds = new Set(reorderedFiltered.map(f => f.id));
      const otherFiles = files.filter(f => !filteredIds.has(f.id));
      
      // Insert filtered files in their new order at the start
      reorderFiles([...reorderedFiltered, ...otherFiles]);
    }
  }, [activeCategory, files, reorderFiles]);

  const allSelected =
    filteredFiles.length > 0 &&
    filteredFiles.every((f) => selectedFiles.includes(f.id));

  const handleAddMore = useCallback(async () => {
    try {
      const selected = await open({
        multiple: true,
        directory: false,
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
      console.error("Failed to open file dialog:", error);
    }
  }, [addFiles]);

  return (
    <div className="h-full flex flex-col relative z-10 glass-panel-heavy rounded-2xl overflow-hidden shadow-lg border-0 w-full">
      {/* Header */}
      <div className={`p-5 flex items-center justify-between border-b ${isDark ? "border-dark-700/50" : "border-dark-100/50"}`}>
        <div className="flex items-center gap-3">
          <div className="w-8 h-8 rounded-lg bg-brand/10 text-brand flex items-center justify-center shrink-0">
            <Layers className="w-4 h-4" />
          </div>
          <h2 className={`text-[15px] font-bold tracking-wide uppercase ${isDark ? "text-white" : "text-dark-900"}`}>
            {activeCategory === "all" ? t("nav.all") : t(`nav.${activeCategory}` as const)}
          </h2>
          <span className={`ml-2 px-2.5 py-0.5 rounded-full text-[11px] font-bold tracking-widest ${
            isDark ? "bg-dark-800 text-dark-400 border border-dark-700" : "bg-white text-dark-500 border border-dark-100 shadow-sm"
          }`}>
            {filteredFiles.length} {filteredFiles.length !== 1 ? t("conversion.itemPlural") : t("conversion.itemSingular")}
          </span>
        </div>
        <div className="flex items-center gap-2">
          <motion.button
            className={`flex items-center gap-2 px-3 py-1.5 rounded-lg text-xs font-bold transition-all border ${
              isDark
                ? "bg-dark-800/40 border-dark-700 hover:border-dark-600 text-dark-300 hover:text-white"
                : "bg-white/60 border-dark-100 hover:border-dark-200 text-dark-600 hover:text-dark-900"
            }`}
            onClick={() => (allSelected ? deselectAllFiles() : selectAllFiles())}
            whileHover={{ scale: 1.02 }}
            whileTap={{ scale: 0.98 }}
            disabled={filteredFiles.length === 0}
          >
            {allSelected ? (
              <CheckSquare className="w-3.5 h-3.5 text-brand" />
            ) : (
              <Square className="w-3.5 h-3.5" />
            )}
            {allSelected ? t("fileList.deselect") : t("fileList.selectAll")}
          </motion.button>
          
          <div className={`w-px h-6 mx-1 ${isDark ? "bg-dark-700/50" : "bg-dark-200/50"}`}></div>

          <motion.button
            className={`flex items-center gap-2 px-3 py-1.5 rounded-lg text-xs font-bold transition-all border ${
              isDark
                ? "bg-brand/10 border-brand/20 hover:border-brand/40 text-brand hover:bg-brand/20"
                : "bg-brand/5 border-brand/20 hover:border-brand/40 text-brand hover:bg-brand/10"
            }`}
            onClick={handleAddMore}
            whileHover={{ scale: 1.02 }}
            whileTap={{ scale: 0.98 }}
          >
            <Plus className="w-3.5 h-3.5" />
            {t("fileList.add")}
          </motion.button>
          <motion.button
            className={`flex items-center gap-2 px-3 py-1.5 rounded-lg text-xs font-bold transition-all border ${
              isDark
                ? "bg-error-500/10 border-error-500/20 hover:border-error-500/40 text-error-500 hover:bg-error-500/20"
                : "bg-error-500/5 border-error-500/20 hover:border-error-500/40 text-error-500 hover:bg-error-500/10"
            }`}
            onClick={clearFiles}
            whileHover={{ scale: 1.02 }}
            whileTap={{ scale: 0.98 }}
            disabled={filteredFiles.length === 0}
          >
            <Trash2 className="w-3.5 h-3.5" />
            {t("conversion.clear")}
          </motion.button>
        </div>
      </div>

      {/* File Grid */}
      <div className="flex-1 overflow-y-auto p-4 custom-scrollbar">
        {filteredFiles.length === 0 ? (
          <div className="h-full flex flex-col items-center justify-center opacity-70">
            <div className={`w-24 h-24 rounded-full flex items-center justify-center mb-6 border-2 border-dashed ${
              isDark ? "bg-dark-800/50 border-dark-700" : "bg-white/50 border-dark-200"
            }`}>
              <Upload className={`w-10 h-10 ${isDark ? "text-dark-500" : "text-dark-400"}`} />
            </div>
            <p className={`text-lg font-bold tracking-wide ${isDark ? "text-dark-400" : "text-dark-500"}`}>{t("fileList.queueEmpty")}</p>
            <p className={`text-sm mt-2 ${isDark ? "text-dark-500" : "text-dark-400"}`}>
              {activeCategory === "all"
                ? t("fileList.addFilesToStart")
                : `${t("fileList.addCategoryFiles")} ${t(`nav.${activeCategory}` as const)}`}
            </p>
          </div>
        ) : (
          // Step 4 UI-hardening pass (Bug 2): a grid item's automatic
          // min-width is "auto" by default, which resolves to its
          // min-content size - and a long, unbroken filename's min-content
          // IS its full unwrapped width (that's exactly what makes
          // `truncate` possible in the first place: `white-space: nowrap`).
          // Without `min-w-0` here and on each Reorder.Item below, that
          // full width bubbled up and set the grid track wider than the
          // scroll container, forcing a horizontal scrollbar instead of
          // letting FileCard's own `truncate` actually do its job.
          <Reorder.Group
            axis="y"
            values={filteredFiles}
            onReorder={handleReorder}
            className="grid gap-3 min-w-0"
          >
            <AnimatePresence mode="popLayout">
              {filteredFiles.map((file, index) => (
                <SortableFileCard key={file.id} file={file} index={index} isDark={isDark} />
              ))}
            </AnimatePresence>
          </Reorder.Group>
        )}
      </div>

      {/* Drag hint */}
      <AnimatePresence>
        {filteredFiles.length > 1 && (
          <motion.div 
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: 'auto' }}
            exit={{ opacity: 0, height: 0 }}
            className={`py-2 px-4 text-center border-t backdrop-blur-md ${
              isDark ? "bg-dark-800/40 border-dark-700/50 text-dark-500" : "bg-white/40 border-dark-100 text-dark-400"
            }`}
          >
            <p className="text-[10px] font-bold uppercase tracking-widest flex items-center justify-center gap-2">
              <GripVertical className="w-3 h-3" />
              {t("fileList.dragToReorder")}
            </p>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
});

// FileCard drag-handle fix (manual test finding "six dots misaligned,
// overlapping card border"): the old handle was absolutely positioned
// outside the card's own edge (`-translate-x-3` against a `w-5` icon,
// i.e. only partially offset), so it visually collided with the card's
// border/frame instead of sitting inside it. Rendering the handle as a
// normal flex child *inside* FileCard's own padded row - a fixed-size hit
// area, not raw dots pinned to the edge - keeps it vertically centered,
// clear of the border, and stable across window widths since it's part of
// layout flow rather than absolute positioning.
//
// This also required `useDragControls` per item + `dragListener={false}`
// on Reorder.Item so only the handle (not the whole card) starts a drag -
// previously ANY point on the card was a valid drag start, which is why
// TEST D calls out "whole card not accidentally draggable from unrelated
// areas". `useDragControls` is a hook, so it needs its own component
// (one per list item) rather than being called inline in `.map()`.
const SortableFileCard = memo(function SortableFileCard({
  file,
  index,
  isDark,
}: {
  file: ConversionFile;
  index: number;
  isDark: boolean;
}) {
  const dragControls = useDragControls();

  return (
    <Reorder.Item
      value={file}
      dragListener={false}
      dragControls={dragControls}
      initial={{ opacity: 0, scale: 0.95, y: 10 }}
      animate={{ opacity: 1, scale: 1, y: 0 }}
      exit={{ opacity: 0, scale: 0.95, height: 0, overflow: "hidden" }}
      transition={{ delay: index * 0.02, type: "spring", stiffness: 400, damping: 30 }}
      className="relative group outline-none min-w-0"
      whileDrag={{
        scale: 1.02,
        boxShadow: isDark ? "0 10px 30px rgba(0,0,0,0.5)" : "0 10px 30px rgba(0,0,0,0.1)",
        zIndex: 10,
      }}
    >
      <FileCard file={file} dragControls={dragControls} />
    </Reorder.Item>
  );
});
