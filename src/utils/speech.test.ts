import { describe, it, expect } from "vitest";
import {
  availabilityOf,
  availabilityOfCapability,
  fileNameOf,
  formatElapsed,
  formatFileSize,
  formatTimestamp,
  hasSupportedAudioExtension,
  renderTranscript,
  toSpeechError,
} from "./speech";

describe("audio extension allowlist (file picker / drop hint)", () => {
  it("accepts exactly WAV, MP3, M4A, AAC, FLAC, OGG (any case, any path style)", () => {
    for (const p of ["C:\\a\\b.WAV", "/x/y.mp3", "d.M4A", "e.aac", "f.flac", "g.OGG"]) {
      expect(hasSupportedAudioExtension(p), p).toBe(true);
    }
  });
  it("rejects everything else, including double extensions and no extension", () => {
    for (const p of ["a.mp4", "a.wma", "a.opus", "a.txt", "a", "a.mp3.exe", "C:\\dir.mp3\\file", "a.webm"]) {
      expect(hasSupportedAudioExtension(p), p).toBe(false);
    }
  });
  it("fileNameOf handles both separators", () => {
    expect(fileNameOf("C:\\Users\\a\\kayıt.mp3")).toBe("kayıt.mp3");
    expect(fileNameOf("/home/a/k.wav")).toBe("k.wav");
  });
});

describe("formatting", () => {
  it("elapsed time is mm:ss and h:mm:ss", () => {
    expect(formatElapsed(0)).toBe("00:00");
    expect(formatElapsed(138)).toBe("02:18"); // the spec's example: "Geçen süre: 02:18"
    expect(formatElapsed(3600)).toBe("1:00:00");
    expect(formatElapsed(-5)).toBe("00:00");
  });
  it("file sizes", () => {
    expect(formatFileSize(512)).toBe("512 B");
    expect(formatFileSize(1536)).toBe("1.5 KB");
    expect(formatFileSize(5 * 1024 * 1024)).toBe("5.0 MB");
  });
  it("timestamps are [HH:MM:SS]", () => {
    expect(formatTimestamp(12_000)).toBe("[00:00:12]");
    expect(formatTimestamp(3_725_000)).toBe("[01:02:05]");
  });
});

describe("transcript rendering", () => {
  const segs = [
    { start_ms: 12_000, end_ms: 20_000, text: "Toplantımızın ilk gündem maddesi..." },
    { start_ms: 24_000, end_ms: 30_000, text: "Siber güvenlik çalışmaları..." },
  ];
  it("plain mode joins segments", () => {
    expect(renderTranscript(segs, false)).toBe("Toplantımızın ilk gündem maddesi... Siber güvenlik çalışmaları...");
  });
  it("timestamp mode matches the spec example format", () => {
    expect(renderTranscript(segs, true)).toBe(
      "[00:00:12] Toplantımızın ilk gündem maddesi...\n[00:00:24] Siber güvenlik çalışmaları..."
    );
  });
});

describe("availability labels come from backend state, never assumed", () => {
  it("engine status -> Hazır / Bileşen eksik / Henüz desteklenmiyor", () => {
    expect(availabilityOf("AVAILABLE")).toBe("ready");
    for (const s of ["ENGINE_MISSING", "MODEL_MISSING", "ENGINE_INVALID", "RUNTIME_MISSING", "AUDIO_PREP_UNAVAILABLE"] as const) {
      expect(availabilityOf(s), s).toBe("missing");
    }
    expect(availabilityOf("CPU_UNSUPPORTED")).toBe("unsupported");
  });
  it("capability state mapping", () => {
    expect(availabilityOfCapability("AVAILABLE")).toBe("ready");
    expect(availabilityOfCapability("ENGINE_MISSING")).toBe("missing");
    expect(availabilityOfCapability("NOT_IMPLEMENTED")).toBe("unsupported");
  });
});

describe("Turkish error localization / no raw error leakage", () => {
  it("passes through the backend's structured Turkish message", () => {
    const e = toSpeechError({ code: "SPEECH_RESOURCE_LIMIT", message: "Ses dosyası çok büyük." });
    expect(e).toEqual({ code: "SPEECH_RESOURCE_LIMIT", message: "Ses dosyası çok büyük." });
  });
  it("never surfaces a raw string / Error / path as a message", () => {
    for (const raw of [
      "C:\\Users\\x\\AppData\\whisper-cli.exe: failed",
      new Error("stderr: assertion failed at ggml.c"),
      undefined,
      null,
      42,
    ]) {
      const e = toSpeechError(raw);
      expect(e.code).toBe("SPEECH_TRANSCRIPTION_FAILED");
      expect(e.message).toMatch(/hata/i);
      expect(e.message).not.toMatch(/whisper|ggml|stderr|\\/);
    }
  });
});
