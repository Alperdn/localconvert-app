import { describe, it, expect } from "vitest";
import pageSrc from "../components/speech/SpeechDictationPage.tsx?raw";
import hookSrc from "../hooks/useSpeechJob.ts?raw";
import utilsSrc from "./speech.ts?raw";
import typesSrc from "../types/speech.ts?raw";
import trSrc from "../locales/tr.ts?raw";
import packageJson from "../../package.json?raw";
import tauriConf from "../../src-tauri/tauri.conf.json?raw";

// Ses Dikte is local-only. This is a static audit of the frontend code that
// implements it: no network primitives, no external URLs, no cloud SDKs, no
// "fall back to online" wording. (The backend has the equivalent Rust test.)

const SPEECH_SOURCES: Record<string, string> = {
  "src/components/speech/SpeechDictationPage.tsx": pageSrc,
  "src/hooks/useSpeechJob.ts": hookSrc,
  "src/utils/speech.ts": utilsSrc,
  "src/types/speech.ts": typesSrc,
};

describe("no network / cloud fallback in Ses Dikte (frontend)", () => {
  for (const [file, src] of Object.entries(SPEECH_SOURCES)) {
    it(`${file} uses no network primitive and no external URL`, () => {
      for (const banned of [
        /\bfetch\s*\(/,
        /XMLHttpRequest/,
        /\bWebSocket\b/,
        /\bEventSource\b/,
        /sendBeacon/,
        /https?:\/\//,
        /webkitSpeechRecognition/,
        /\bSpeechRecognition\b/, // the browser Web Speech API sends audio to a cloud service
      ]) {
        expect(banned.test(src), `${file} matches ${banned}`).toBe(false);
      }
    });
  }

  it("the microphone is not touched anywhere in this phase (getUserMedia never referenced in code)", () => {
    const stripComments = (src: string) => src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
    for (const [file, src] of Object.entries(SPEECH_SOURCES)) {
      expect(stripComments(src), file).not.toMatch(/getUserMedia|MediaRecorder|AudioWorklet/);
    }
  });

  it("no cloud speech SDK is a dependency", () => {
    const pkg = JSON.parse(packageJson);
    const deps = Object.keys({ ...pkg.dependencies, ...pkg.devDependencies }).join(" ").toLowerCase();
    for (const banned of ["openai", "@azure", "aws-sdk", "@aws-sdk", "@google-cloud", "assemblyai", "deepgram", "speechmatics"]) {
      expect(deps.includes(banned), `dependency matches ${banned}`).toBe(false);
    }
  });

  it("the webview CSP still forbids external connections", () => {
    const conf = JSON.parse(tauriConf);
    const csp = conf.app.security.csp as Record<string, string>;
    expect(csp["connect-src"]).toBe("'self' ipc: http://ipc.localhost");
    expect(csp["connect-src"]).not.toMatch(/https?:\/\/(?!ipc\.localhost)/);
    expect(csp["default-src"]).toBe("'self'");
  });

  it("user-facing strings never mention an online fallback", () => {
    const speechBlock = trSrc.slice(trSrc.indexOf("speech: {"), trSrc.indexOf("sidebar: {"));
    expect(speechBlock.length).toBeGreaterThan(200);
    expect(speechBlock).not.toMatch(/çevrimiçi|online|bulut|cloud|sunucu/i);
    expect(speechBlock).not.toMatch(/100\s*%/);
  });
});
