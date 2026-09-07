// Small, typed i18n foundation. Default locale is tr-TR (Turkish) - see
// CLAUDE.md. English is kept only so a future language switcher has
// somewhere to switch to; there is no UI for it yet, and adding one is a
// future step (do not wire a selector into Settings as part of this one).
//
// Deliberately not a full i18n framework (no ICU plural rules, no
// pluralization engine, no lazy-loaded bundles) - the app is small enough
// that a flat typed dictionary + safe fallback covers every reachable
// string, and pulling in a larger library isn't justified.

import tr from "./tr";
import en from "./en";

export type Locale = "tr-TR" | "en-US";

export const DEFAULT_LOCALE: Locale = "tr-TR";

// Recursively widen every literal string leaf of `typeof tr` to `string`,
// so `Dictionary` describes the *shape* (namespaces/keys) of the
// dictionary without pinning every value to tr.ts's exact Turkish text.
type Widen<T> = T extends string ? string : { [K in keyof T]: Widen<T[K]> };
type Dictionary = Widen<typeof tr>;

// No `as unknown as Dictionary` cast here on purpose (Step 3, section M):
// `en` must structurally satisfy `Dictionary` (same namespaces/keys as
// tr.ts, values just typed as `string`), so a key present in tr.ts but
// missing/renamed in en.ts is a `tsc` build failure, not a runtime
// "undefined" surprise discovered later. Keep it this way - re-adding a
// cast here would silently defeat that check.
const dictionaries: Record<Locale, Dictionary> = {
  "tr-TR": tr,
  "en-US": en,
};

let currentLocale: Locale = DEFAULT_LOCALE;

export function setLocale(locale: Locale) {
  currentLocale = locale;
}

export function getLocale(): Locale {
  return currentLocale;
}

type PathsToStrings<T, Prefix extends string = ""> = {
  [K in keyof T & string]: T[K] extends string
    ? `${Prefix}${K}`
    : PathsToStrings<T[K], `${Prefix}${K}.`>;
}[keyof T & string];

/// Every valid "namespace.key" path into the dictionary, e.g. "workflow.convert".
export type TranslationKey = PathsToStrings<Dictionary>;

function lookup(dict: Dictionary, path: string): string | undefined {
  const parts = path.split(".");
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let node: any = dict;
  for (const part of parts) {
    if (node == null || typeof node !== "object") return undefined;
    node = node[part];
  }
  return typeof node === "string" ? node : undefined;
}

/**
 * Translate a key for the current locale. Never returns `undefined`/`null`/
 * "undefined" to the UI: falls back to the Turkish (default) dictionary,
 * then to English, then finally to the raw key itself so a missing
 * translation is visibly wrong in review rather than silently blank.
 */
export function t(key: TranslationKey): string {
  return (
    lookup(dictionaries[currentLocale], key) ??
    lookup(dictionaries[DEFAULT_LOCALE], key) ??
    lookup(dictionaries["en-US"], key) ??
    key
  );
}

/** Turkish user-facing text for a backend `CapabilityState`. */
export function translateCapabilityState(state: string): string {
  return lookup(dictionaries[currentLocale], `capabilityStates.${state}`) ?? state;
}

/**
 * Turkish user-facing text for a backend capability id (e.g.
 * "office_to_pdf", "video_conversion"). Falls back to the raw id (never
 * undefined) so an unmapped future capability degrades to something
 * visible rather than blank/undefined.
 */
export function translateCapabilityCategory(id: string): string {
  return lookup(dictionaries[currentLocale], `capabilityCategories.${id}`) ?? id;
}

/**
 * Turkish "<feature> — <state>" text for a capability that's blocking an
 * action (e.g. "Video Kırpma — Gerekli bileşen bulunamadı"). This is the
 * ONLY sanctioned way to describe an unavailable capability to the user -
 * never render a `Capability.message` field directly (it's backend/English
 * and only guaranteed not to leak a path or executable name, not to be
 * Turkish or user-facing copy).
 */
export function describeUnavailableCapability(cap: { id: string; state: string }): string {
  return `${translateCapabilityCategory(cap.id)} — ${translateCapabilityState(cap.state)}`;
}

// Known engine code prefixes (`EngineId::code_prefix()` on the Rust side -
// see src-tauri/src/engines/engine_id.rs). Used only to strip a prefix off
// an engine-scoped error code (e.g. "FFMPEG_PROCESS_FAILED") so it can
// match the generic suffix entry ("PROCESS_FAILED") in errorCodes below.
// This list is UI-only best-effort - an unrecognized prefix just falls
// through to the generic process-failed message, never to raw/undefined
// text.
const KNOWN_ENGINE_PREFIXES = ["OFFICE", "FFMPEG", "FFPROBE", "TESSERACT", "MAGICK"];

/**
 * Turkish user-facing text for a stable backend error code (the
 * `CODE: message` shape produced by `EngineError`/`ImageNativeError`).
 * Codes not in the map (an untranslated engine message, raw stderr, ...)
 * fall back to a generic Turkish "process failed" message rather than
 * leaking the raw English/technical string into the UI.
 */
export function translateErrorCode(codeOrMessage: string): string {
  const code = codeOrMessage.split(":")[0].trim();

  const direct = lookup(dictionaries[currentLocale], `errorCodes.${code}`);
  if (direct) return direct;

  // Try stripping a known engine prefix, e.g. "FFMPEG_PROCESS_FAILED" ->
  // "PROCESS_FAILED", "OFFICE_ENGINE_NOT_AVAILABLE" is already covered by
  // an exact key above, but other engine+kind combinations aren't all
  // enumerated individually.
  for (const prefix of KNOWN_ENGINE_PREFIXES) {
    if (code.startsWith(`${prefix}_`)) {
      const suffix = code.slice(prefix.length + 1);
      const viaSuffix = lookup(dictionaries[currentLocale], `errorCodes.${suffix}`);
      if (viaSuffix) return viaSuffix;
    }
  }

  return lookup(dictionaries[currentLocale], "notifications.processFailed")!;
}
