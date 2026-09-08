import { memo } from "react";
import { motion } from "framer-motion";
import {
  Files,
  Video,
  Music,
  Image,
  FileText,
  Table,
  Presentation,
  BookOpen,
  Archive,
  PenTool,
  Type,
} from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { useStore, Category } from "../store/useStore";
import { CATEGORIES } from "../types/formats";
import { t } from "../locales";

const iconMap: Record<string, React.ComponentType<{ className?: string }>> = {
  Files,
  Video,
  Music,
  Image,
  FileText,
  Table,
  Presentation,
  BookOpen,
  Archive,
  PenTool,
  Type,
};

// Step 3 perf pass (section D): takes no props, so React.memo means this
// only re-renders from its OWN (now narrow) store subscription - it no
// longer gets re-invoked just because App.tsx re-rendered for an unrelated
// reason (e.g. a conversion progress tick changing `files`), which is what
// let "the app feels sluggish" symptom bleed into the sidebar even outside
// active conversions.
export const Sidebar = memo(function Sidebar() {
  // Step 3 perf pass (section D): `useStore()` with no selector subscribes
  // to the ENTIRE store, so this component used to re-render on every
  // unrelated state change anywhere in the app (a conversion progress
  // tick, a capability load, GPU detection, ...) - not just on the fields
  // it actually renders. A narrow `useShallow` selector only re-renders
  // Sidebar when one of these four fields actually changes.
  const { activeCategory, setActiveCategory, files, theme } = useStore(
    useShallow((s) => ({
      activeCategory: s.activeCategory,
      setActiveCategory: s.setActiveCategory,
      files: s.files,
      theme: s.settings.theme,
    }))
  );
  const isDark = theme === "dark";

  const categories = Object.entries(CATEGORIES) as [Category, typeof CATEGORIES[keyof typeof CATEGORIES]][];

  const getCategoryCount = (category: Category) => {
    if (category === "all") return files.length;
    return files.filter((f) => f.category === category).length;
  };

  return (
    <aside className="w-64 h-full flex flex-col glass-panel rounded-2xl border-0 shadow-lg overflow-hidden z-10 relative">
      <div className="p-5 flex-1 overflow-y-auto custom-scrollbar">
        <h2 className={`text-xs font-bold uppercase tracking-widest mb-4 pl-2 ${
          isDark ? "text-dark-500" : "text-dark-400"
        }`}>
          {t("nav.categories")}
        </h2>
        <nav className="space-y-1.5 relative">
          {categories.map(([key, data]) => {
            const Icon = iconMap[data.icon] || Files;
            const count = getCategoryCount(key);
            const isActive = activeCategory === key;

            return (
              <motion.button
                key={key}
                className={`w-full flex items-center gap-3 px-4 py-3 rounded-xl text-sm font-medium transition-colors duration-200 relative overflow-hidden ${
                  isActive
                    ? "text-white"
                    : isDark
                      ? "text-dark-300 hover:text-white hover:bg-dark-700/40"
                      : "text-dark-600 hover:text-dark-900 hover:bg-white/50"
                }`}
                onClick={() => setActiveCategory(key)}
                // Hover feedback used to be driven by Framer Motion
                // animating `backgroundColor` (JS-interpolated, non-GPU,
                // repaints every frame of the hover transition) on top of
                // an already-redundant CSS `hover:bg-*` class fighting for
                // the same property - that double animation was the direct
                // cause of the reported laggy Sidebar hover response (Step
                // 3 perf pass, section E). `x` is the only thing Framer
                // needs to animate here; it moves via `transform`, which is
                // GPU-compositable, and the background tint is left to the
                // plain CSS `hover:` class above.
                whileHover={!isActive ? { x: 4 } : undefined}
                whileTap={{ scale: 0.98 }}
              >
                {isActive && (
                  <motion.div
                    layoutId="activeCategoryBg"
                    className="absolute inset-0 bg-accent-gradient opacity-90 backdrop-blur-sm -z-10"
                    transition={{ type: "spring", stiffness: 300, damping: 25 }}
                  />
                )}
                
                <Icon className={`w-4 h-4 z-10 ${isActive ? "text-white" : data.color}`} />
                <span className="flex-1 text-left z-10">{t(`nav.${key}`)}</span>
                
                {count > 0 && (
                  <span
                    className={`px-2.5 py-0.5 rounded-full text-[11px] font-bold z-10 transition-colors ${
                      isActive
                        ? "bg-white/20 text-white"
                        : isDark
                          ? "bg-dark-800 text-dark-400"
                          : "bg-dark-100 text-dark-500"
                    }`}
                  >
                    {count}
                  </span>
                )}
              </motion.button>
            );
          })}
        </nav>
      </div>

      {/* Quick Stats */}
      <div className="p-4 shrink-0">
        <div className={`rounded-xl p-4 backdrop-blur-md border ${
          isDark ? "bg-dark-800/40 border-dark-700/50" : "bg-white/40 border-dark-100"
        }`}>
          <p className={`text-[10px] font-bold uppercase tracking-widest mb-3 ${isDark ? "text-dark-500" : "text-dark-400"}`}>{t("sidebar.sessionStats")}</p>
          <div className="space-y-2">
            <div className="flex justify-between items-center text-sm">
              <span className={isDark ? "text-dark-400" : "text-dark-500"}>{t("sidebar.files")}</span>
              <span className={`font-semibold bg-dark-100/50 dark:bg-dark-800/50 px-2 py-0.5 rounded-md ${isDark ? "text-white" : "text-dark-900"}`}>{files.length}</span>
            </div>
            <div className="flex justify-between items-center text-sm">
              <span className={isDark ? "text-dark-400" : "text-dark-500"}>{t("sidebar.completed")}</span>
              <span className="text-success-500 font-semibold bg-success-500/10 px-2 py-0.5 rounded-md">
                {files.filter((f) => f.status === "completed").length}
              </span>
            </div>
            <div className="flex justify-between items-center text-sm">
              <span className={isDark ? "text-dark-400" : "text-dark-500"}>{t("sidebar.errors")}</span>
              <span className="text-error-500 font-semibold bg-error-500/10 px-2 py-0.5 rounded-md">
                {files.filter((f) => f.status === "error").length}
              </span>
            </div>
          </div>
        </div>
      </div>
    </aside>
  );
});
