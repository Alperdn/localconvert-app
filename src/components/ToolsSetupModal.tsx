import { useEffect } from "react";
import { motion } from "framer-motion";
import {
  X,
  Check,
  AlertTriangle,
  Ban,
  RefreshCw,
  Wrench,
  Info,
} from "lucide-react";
import toast from "react-hot-toast";
import { useStore } from "../store/useStore";
import type { CapabilityState } from "../store/useStore";
import { t, translateCapabilityState, translateCapabilityCategory } from "../locales";

interface ToolsSetupModalProps {
  onClose: () => void;
}

// Product decision (Step 3, section J): LocalConvert ships its required
// conversion engines bundled with the app - end users are never asked to
// find, download, or install a third-party tool themselves, and this panel
// must not suggest otherwise. It is a READ-ONLY status view driven entirely
// by the backend capability model (see src-tauri/src/capabilities.rs); it
// used to offer "Install"/"Download" buttons that opened external download
// pages, which has been removed as misleading manual-install UX now that
// bundling is the actual plan. There is deliberately no download/install
// action left anywhere in this component.
const CAPABILITY_ICON: Record<CapabilityState, React.ComponentType<{ className?: string }>> = {
  AVAILABLE: Check,
  ENGINE_MISSING: AlertTriangle,
  NOT_IMPLEMENTED: AlertTriangle,
  DISABLED_BY_POLICY: Ban,
};

export function ToolsSetupModal({ onClose }: ToolsSetupModalProps) {
  const { capabilities, capabilitiesLoaded, loadCapabilities, settings } = useStore();
  const isDark = settings.theme === "dark";

  useEffect(() => {
    if (!capabilitiesLoaded) {
      loadCapabilities();
    }
  }, [capabilitiesLoaded, loadCapabilities]);

  const handleRefresh = async () => {
    toast.loading(t("toolsSetup.checking"), { id: "refresh-capabilities" });
    await loadCapabilities();
    toast.success(t("toolsSetup.checked"), { id: "refresh-capabilities" });
  };

  const availableCount = capabilities.filter((c) => c.state === "AVAILABLE").length;

  return (
    <motion.div
      className="fixed inset-0 bg-black/60 backdrop-blur-sm flex items-center justify-center z-50"
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      onClick={onClose}
    >
      <motion.div
        className={`rounded-2xl border w-full max-w-2xl max-h-[80vh] overflow-hidden shadow-2xl flex flex-col ${
          isDark ? "bg-dark-800 border-dark-700" : "bg-white border-gray-200"
        }`}
        initial={{ scale: 0.95, opacity: 0 }}
        animate={{ scale: 1, opacity: 1 }}
        exit={{ scale: 0.95, opacity: 0 }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className={`flex items-center justify-between p-6 border-b ${isDark ? "border-dark-700" : "border-gray-200"}`}>
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-xl bg-accent-gradient flex items-center justify-center">
              <Wrench className="w-5 h-5 text-white" />
            </div>
            <div>
              <h2 className={`text-xl font-semibold ${isDark ? "text-white" : "text-gray-900"}`}>{t("toolsSetup.title")}</h2>
              <p className={`text-sm ${isDark ? "text-dark-400" : "text-gray-500"}`}>
                {availableCount} / {capabilities.length}
              </p>
            </div>
          </div>
          <div className="flex items-center gap-2">
            <motion.button
              className={`flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm ${
                isDark ? "bg-dark-700 text-dark-300 hover:text-white" : "bg-gray-100 text-gray-600 hover:text-gray-900"
              }`}
              onClick={handleRefresh}
              whileHover={{ scale: 1.02 }}
              whileTap={{ scale: 0.98 }}
            >
              <RefreshCw className="w-4 h-4" />
              {t("toolsSetup.refresh")}
            </motion.button>
            <motion.button
              className={`p-2 rounded-lg transition-colors ${
                isDark ? "hover:bg-dark-700 text-dark-400 hover:text-white" : "hover:bg-gray-100 text-gray-500 hover:text-gray-900"
              }`}
              onClick={onClose}
              whileHover={{ scale: 1.1 }}
              whileTap={{ scale: 0.9 }}
            >
              <X className="w-5 h-5" />
            </motion.button>
          </div>
        </div>

        {/* Info banner - explains the bundling model, never suggests manual install */}
        <div className="mx-6 mt-6 p-4 bg-accent-500/10 border border-accent-500/20 rounded-xl">
          <div className="flex items-start gap-3">
            <Info className="w-5 h-5 text-accent-500 flex-shrink-0 mt-0.5" />
            <div>
              <p className={`text-sm ${isDark ? "text-dark-200" : "text-gray-700"}`}>{t("toolsSetup.bundledNote")}</p>
              <p className={`text-xs mt-1 font-medium ${isDark ? "text-dark-400" : "text-gray-500"}`}>{t("toolsSetup.noManualInstallNote")}</p>
            </div>
          </div>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-6">
          <div className="space-y-3">
            {capabilities.map((cap, index) => {
              const Icon = CAPABILITY_ICON[cap.state] ?? AlertTriangle;
              const available = cap.state === "AVAILABLE";
              return (
                <motion.div
                  key={cap.id}
                  className={`rounded-xl p-4 border ${
                    isDark ? "bg-dark-700/50" : "bg-gray-50"
                  } ${available ? "border-success-500/20" : "border-transparent"}`}
                  initial={{ opacity: 0, y: 20 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ delay: index * 0.03 }}
                >
                  <div className="flex items-center gap-4">
                    <div
                      className={`w-10 h-10 rounded-xl flex items-center justify-center ${
                        available
                          ? "bg-success-600/20 text-success-500"
                          : isDark
                            ? "bg-dark-600 text-dark-400"
                            : "bg-gray-200 text-gray-500"
                      }`}
                    >
                      <Icon className="w-5 h-5" />
                    </div>

                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className={`font-medium ${isDark ? "text-white" : "text-gray-900"}`}>
                          {translateCapabilityCategory(cap.id)}
                        </span>
                        <span
                          className={`px-2 py-0.5 rounded-full text-xs ${
                            available
                              ? "bg-success-600/20 text-success-500"
                              : "bg-accent-600/20 text-accent-500"
                          }`}
                        >
                          {translateCapabilityState(cap.state)}
                        </span>
                      </div>
                    </div>
                  </div>
                </motion.div>
              );
            })}

            {capabilities.length === 0 && (
              <p className={`text-sm text-center py-8 ${isDark ? "text-dark-400" : "text-gray-500"}`}>
                {t("toolsSetup.checking")}
              </p>
            )}
          </div>
        </div>

        {/* Footer */}
        <div className={`p-6 border-t ${isDark ? "border-dark-700" : "border-gray-200"}`}>
          <div className="flex items-center justify-end">
            <motion.button
              className="px-6 py-2.5 rounded-xl bg-accent-gradient text-white font-medium"
              onClick={onClose}
              whileHover={{ scale: 1.02 }}
              whileTap={{ scale: 0.98 }}
            >
              {t("toolsSetup.close")}
            </motion.button>
          </div>
        </div>
      </motion.div>
    </motion.div>
  );
}
