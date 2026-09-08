// Maps a file's category/extension to the backend capability id that gates
// it, so the UI can disable a feature BEFORE a conversion is attempted
// instead of letting the user hit "Convert" and only finding out from a
// failed process spawn. Source of truth for the actual state is always the
// backend (`get_capabilities` / src-tauri/src/capabilities.rs) - this file
// only encodes "which capability id applies to this file", it never invents
// availability itself.

import type { Capability, ConversionFile } from "../store/useStore";

const OFFICE_EXTENSIONS = new Set([
  "doc", "docx", "odt", "rtf",
  "xls", "xlsx", "ods",
  "ppt", "pptx", "odp",
]);

/**
 * The capability id (if any) that gates converting this file. Returns null
 * for files whose conversion doesn't depend on a not-yet-bundled engine
 * (e.g. the native image pipeline, native ZIP, native PDF text editing).
 */
export function capabilityIdForFile(file: Pick<ConversionFile, "category" | "extension">): string | null {
  const ext = file.extension.toLowerCase();

  if (file.category === "video") return "video_conversion";
  if (file.category === "audio") return "audio_extraction";

  if (
    (file.category === "document" || file.category === "spreadsheet" || file.category === "presentation") &&
    OFFICE_EXTENSIONS.has(ext)
  ) {
    return "office_to_pdf";
  }

  if (ext === "svg") return "svg_rasterization";
  if (ext === "avif" || ext === "psd") return "advanced_image_formats";
  if (ext === "heic" || ext === "heif") return "heic_conversion";

  return null;
}

/**
 * The capability id (if any) that gates converting TO this output format,
 * regardless of the source file. Used to keep format-picker dropdowns from
 * offering an option that would predictably fail (Step 3, section H) - e.g.
 * AVIF output needs ImageMagick (`advanced_image_formats`), which isn't
 * bundled yet.
 */
export function capabilityIdForOutputFormat(format: string): string | null {
  const fmt = format.toLowerCase();
  if (fmt === "avif" || fmt === "psd") return "advanced_image_formats";
  if (fmt === "heic" || fmt === "heif") return "heic_conversion";
  return null;
}

export function isCapabilityUsable(capability: Capability | undefined): boolean {
  // Unknown (not yet loaded / not modeled) capabilities are treated as
  // usable so we never block a feature the backend hasn't reported an
  // opinion on - only an explicit non-AVAILABLE state disables anything.
  if (!capability) return true;
  return capability.state === "AVAILABLE";
}

/**
 * The single source of truth for "is ANY of these files blocked by a
 * not-yet-available engine". Every entry point that can trigger a real
 * conversion (the Convert button, the Enter keyboard shortcut, the
 * convertFiles store action itself as defense-in-depth) MUST run this same
 * check before a process can be spawned - Step 3 manual testing found the
 * Enter shortcut bypassed the button's guard entirely, letting an Office
 * conversion start with no engine available. Recomputed fresh from the
 * current file list and current capabilities on every call - never cache
 * the result, since a stale cached "blocked" value is exactly what caused
 * the video/image cross-contamination bug (Step 3 finding #5).
 */
export function getBlockedCapability(
  files: Pick<ConversionFile, "category" | "extension">[],
  getCapability: (id: string) => Capability | undefined
): Capability | undefined {
  for (const file of files) {
    const capId = capabilityIdForFile(file);
    if (!capId) continue;
    const cap = getCapability(capId);
    if (!isCapabilityUsable(cap)) return cap;
  }
  return undefined;
}
