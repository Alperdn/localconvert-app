import { memo } from "react";
import { Settings, ShieldCheck, Wrench, Zap, Minus, Square, X } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useStore } from "../store/useStore";
import { t } from "../locales";

interface HeaderProps {
  onSettingsClick: () => void;
  onPrivacyClick: () => void;
  onToolsClick: () => void;
}

// Shared class for the small icon buttons - CSS-only hover/active feedback
// (transform + colors), no Framer Motion gesture recognizer per button. That
// recognizer was doing hit-testing on every pointer move across the header,
// which is what made rapid cursor movement across these buttons feel
// delayed (manual test finding "top-right small buttons still feel
// laggy"). `transition-transform`/`transition-colors` instead of
// `transition-all` keeps the browser from re-evaluating properties (like
// the inherited backdrop-filter) that never actually change on hover.
const iconButtonClass = (isDark: boolean) =>
  `p-2 rounded-lg transition-transform transition-colors duration-150 will-change-transform hover:-translate-y-px hover:scale-105 active:scale-95 ${
    isDark ? "hover:bg-dark-700/50 text-dark-400 hover:text-white" : "hover:bg-dark-100/50 text-dark-500 hover:text-brand"
  }`;

// Step 3 perf pass (section D): Header is always mounted above everything
// else. `memo` alone doesn't help if the parent hands it fresh callback
// props every render, so this only pays off paired with the `useCallback`
// wrapping in App.tsx - together they stop Header (and the backdrop-blur
// layer it paints) from re-rendering on every file/progress-tick update
// that App.tsx's `files` subscription produces during conversion (manual
// test finding "no major degradation" during file activity, TEST B).
export const Header = memo(function Header({ onSettingsClick, onPrivacyClick, onToolsClick }: HeaderProps) {
  const appWindow = getCurrentWindow();
  // Step 3 perf pass (section D): the header is always mounted and rendered
  // above everything else, so subscribing to the whole store here meant
  // every unrelated state change re-rendered it too. Only `settings.theme`
  // is actually read.
  const isDark = useStore((s) => s.settings.theme) === "dark";

  const handleMinimize = () => appWindow.minimize();
  const handleMaximize = () => appWindow.toggleMaximize();
  const handleClose = () => appWindow.close();

  return (
    <header className={`relative z-50 h-12 flex items-center justify-between select-none transition-shadow duration-300 rounded-xl glass-panel ${
      isDark ? "shadow-lg shadow-black/20" : "shadow-md shadow-dark-900/5"
    }`}>
      {/* Draggable region - Logo area */}
      <div
        className="flex items-center gap-3 px-4 h-full flex-1 rounded-l-xl"
        data-tauri-drag-region
      >
        <div className="w-7 h-7 rounded-lg bg-accent-gradient flex items-center justify-center pointer-events-none shadow-glow">
          <Zap className="w-4 h-4 text-white" />
        </div>
        <span className={`text-sm font-semibold tracking-wide pointer-events-none ${isDark ? "text-white" : "text-dark-900"}`}>
          Local<span className="text-brand">Convert</span>
        </span>
      </div>

      {/* Right side controls */}
      <div className="flex items-center h-full px-2">
        {/* Action buttons */}
        <div className="flex items-center gap-1 px-3">
          <button
            className={iconButtonClass(isDark)}
            onClick={onToolsClick}
            title={t("header.conversionTools")}
          >
            <Wrench className="w-4 h-4" />
          </button>
          <button
            className={iconButtonClass(isDark)}
            onClick={onPrivacyClick}
            title={t("header.privacyAndStatus")}
          >
            <ShieldCheck className="w-4 h-4" />
          </button>
          <button
            className={iconButtonClass(isDark)}
            onClick={onSettingsClick}
            title={t("header.settings")}
          >
            <Settings className="w-4 h-4" />
          </button>
        </div>

        {/* Separator */}
        <div className={`w-px h-5 mx-1 ${isDark ? "bg-dark-700/50" : "bg-dark-200/50"}`} />

        {/* Window controls */}
        <div className="flex items-center h-full ml-1">
          <button
            className={`h-full px-3 rounded-md transition-colors flex items-center justify-center ${
              isDark ? "hover:bg-dark-700/50 text-dark-400 hover:text-white" : "hover:bg-dark-100/50 text-dark-500 hover:text-dark-900"
            }`}
            onClick={handleMinimize}
            title={t("header.minimize")}
          >
            <Minus className="w-4 h-4" />
          </button>
          <button
            className={`h-full px-3 rounded-md transition-colors flex items-center justify-center ${
              isDark ? "hover:bg-dark-700/50 text-dark-400 hover:text-white" : "hover:bg-dark-100/50 text-dark-500 hover:text-dark-900"
            }`}
            onClick={handleMaximize}
            title={t("header.maximize")}
          >
            <Square className="w-3 h-3" />
          </button>
          <button
            className={`h-full px-3 rounded-md hover:bg-error-500 hover:text-white transition-colors flex items-center justify-center ${
              isDark ? "text-dark-400" : "text-dark-500"
            }`}
            onClick={handleClose}
            title={t("header.close")}
          >
            <X className="w-4 h-4" />
          </button>
        </div>
      </div>
    </header>
  );
});
