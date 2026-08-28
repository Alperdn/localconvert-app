import { useEffect, useState } from "react";
import { motion } from "framer-motion";
import { invoke } from "@tauri-apps/api/core";
import {
  X,
  ShieldCheck,
  WifiOff,
  History,
  Trash2,
  FileImage,
  Loader2,
} from "lucide-react";

interface PrivacyStatus {
  network_dependent_conversion: boolean;
  telemetry_enabled: boolean;
  analytics_enabled: boolean;
  automatic_updates_enabled: boolean;
  conversion_history_enabled: boolean;
  temp_cleanup_enabled: boolean;
  metadata_removal_default_for_images: boolean;
  app_version: string;
}

interface StatusRowProps {
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  detail: string;
  // `good` means the value reflects the privacy-preserving state (e.g.
  // "disabled" for telemetry, "enabled" for temp cleanup) - it is not
  // simply "true".
  good: boolean;
  isDark: boolean;
}

function StatusRow({ icon: Icon, label, detail, good, isDark }: StatusRowProps) {
  return (
    <div className={`flex items-start gap-3 py-3 border-b last:border-b-0 ${
      isDark ? "border-dark-700/50" : "border-dark-100"
    }`}>
      <div className={`mt-0.5 p-1.5 rounded-lg ${
        good
          ? isDark ? "bg-green-900/30 text-green-400" : "bg-green-100 text-green-700"
          : isDark ? "bg-warning-500/10 text-warning-500" : "bg-warning-500/10 text-warning-600"
      }`}>
        <Icon className="w-3.5 h-3.5" />
      </div>
      <div className="flex-1 min-w-0">
        <p className={`text-sm font-medium ${isDark ? "text-white" : "text-dark-900"}`}>
          {label}
        </p>
        <p className={`text-xs mt-0.5 ${isDark ? "text-dark-400" : "text-dark-500"}`}>
          {detail}
        </p>
      </div>
    </div>
  );
}

interface SystemStatusModalProps {
  onClose: () => void;
  isDark: boolean;
}

export function SystemStatusModal({ onClose, isDark }: SystemStatusModalProps) {
  const [status, setStatus] = useState<PrivacyStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<PrivacyStatus>("system_status")
      .then(setStatus)
      .catch((e) => setError(String(e)));
  }, []);

  return (
    <motion.div
      className="fixed inset-0 bg-black/60 backdrop-blur-sm flex items-center justify-center z-50 p-4"
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      onClick={onClose}
    >
      <motion.div
        className={`w-full max-w-md rounded-2xl shadow-2xl overflow-hidden ${
          isDark ? "bg-dark-800 border border-dark-700" : "bg-white border border-dark-100"
        }`}
        initial={{ scale: 0.95, opacity: 0 }}
        animate={{ scale: 1, opacity: 1 }}
        exit={{ scale: 0.95, opacity: 0 }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className={`flex items-center justify-between px-5 py-4 border-b ${
          isDark ? "border-dark-700" : "border-dark-100"
        }`}>
          <div className="flex items-center gap-2">
            <ShieldCheck className="w-5 h-5 text-green-500" />
            <h2 className={`font-semibold ${isDark ? "text-white" : "text-dark-900"}`}>
              Privacy &amp; System Status
            </h2>
          </div>
          <button
            onClick={onClose}
            className={`p-1.5 rounded-lg transition-colors ${
              isDark ? "hover:bg-dark-700 text-dark-400" : "hover:bg-dark-100 text-dark-500"
            }`}
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="px-5 py-2 max-h-[70vh] overflow-y-auto">
          {!status && !error && (
            <div className="flex items-center justify-center py-10">
              <Loader2 className={`w-5 h-5 animate-spin ${isDark ? "text-dark-500" : "text-dark-400"}`} />
            </div>
          )}

          {error && (
            <p className="text-sm text-error-500 py-6">
              Couldn't read system status: {error}
            </p>
          )}

          {status && (
            <>
              <StatusRow
                icon={WifiOff}
                label="Network-dependent conversion"
                detail={status.network_dependent_conversion ? "Enabled" : "Disabled — every conversion runs locally"}
                good={!status.network_dependent_conversion}
                isDark={isDark}
              />
              <StatusRow
                icon={ShieldCheck}
                label="Telemetry"
                detail={status.telemetry_enabled ? "Enabled" : "Disabled — nothing is sent about your usage"}
                good={!status.telemetry_enabled}
                isDark={isDark}
              />
              <StatusRow
                icon={ShieldCheck}
                label="Analytics"
                detail={status.analytics_enabled ? "Enabled" : "Disabled"}
                good={!status.analytics_enabled}
                isDark={isDark}
              />
              <StatusRow
                icon={ShieldCheck}
                label="Automatic updates"
                detail={status.automatic_updates_enabled ? "Enabled" : "Disabled — nothing installs itself"}
                good={!status.automatic_updates_enabled}
                isDark={isDark}
              />
              <StatusRow
                icon={History}
                label="Conversion history"
                detail={status.conversion_history_enabled ? "Enabled" : "Disabled — filenames and paths are not logged"}
                good={!status.conversion_history_enabled}
                isDark={isDark}
              />
              <StatusRow
                icon={Trash2}
                label="Temporary file cleanup"
                detail={status.temp_cleanup_enabled ? "Enabled — per-job working files are removed automatically" : "Disabled"}
                good={status.temp_cleanup_enabled}
                isDark={isDark}
              />
              <StatusRow
                icon={FileImage}
                label="Metadata removal (images)"
                detail={
                  status.metadata_removal_default_for_images
                    ? "Enabled by default for image conversions. Video, document and PDF conversions don't have a metadata-stripping step yet."
                    : "Disabled"
                }
                good={status.metadata_removal_default_for_images}
                isDark={isDark}
              />

              <p className={`text-[11px] pt-3 pb-1 ${isDark ? "text-dark-500" : "text-dark-400"}`}>
                LocalConvert v{status.app_version} · These reflect what this build's code
                actually does, not a general promise.
              </p>
            </>
          )}
        </div>
      </motion.div>
    </motion.div>
  );
}
