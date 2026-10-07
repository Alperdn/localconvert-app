import { afterEach, describe, expect, it, vi } from "vitest";
import { errorFromResponse, WebApiError } from "./client";
import { encodeFileNameHeader } from "./uploads";
import { watchJob, type WebJobSnapshot } from "./jobs";
import {
  extensionOf,
  forgetLocalFile,
  getLocalFile,
  isLocalKey,
  registerBrowserFile,
} from "./localFiles";

describe("errorFromResponse", () => {
  it("keeps only a well-formed server error code", () => {
    const err = errorFromResponse(404, JSON.stringify({ error: { code: "NOT_FOUND", message: "x" } }));
    expect(err).toBeInstanceOf(WebApiError);
    expect(err.code).toBe("NOT_FOUND");
    expect(err.status).toBe(404);
  });

  it("never surfaces raw/unstructured response text as a code", () => {
    expect(errorFromResponse(502, "<html>Bad Gateway C:\\secret\\path</html>").code).toBe("INTERNAL_ERROR");
    expect(errorFromResponse(400, "plain text").code).toBe("PROCESS_FAILED");
    // A code that is not SCREAMING_SNAKE is not trusted either.
    expect(errorFromResponse(400, JSON.stringify({ error: { code: "<script>" } })).code).toBe("PROCESS_FAILED");
  });
});

describe("encodeFileNameHeader", () => {
  it("produces an ASCII header value that round-trips Turkish names", () => {
    const encoded = encodeFileNameHeader("Öğrenci Şöleni ı.jpg");
    expect(/^[\x20-\x7e]*$/.test(encoded)).toBe(true);
    expect(decodeURIComponent(encoded)).toBe("Öğrenci Şöleni ı.jpg");
  });
});

describe("local file registry", () => {
  it("describes a browser File without inventing a filesystem path", () => {
    const file = new File([new Uint8Array([1, 2, 3])], "Fotoğraf.JPG", { type: "image/jpeg" });
    const info = registerBrowserFile(file);
    expect(isLocalKey(info.path)).toBe(true);
    expect(info.path).not.toContain("Fotoğraf");
    expect(info.extension).toBe("jpg");
    expect(info.category).toBe("image");
    expect(info.size).toBe(3);
    expect(getLocalFile(info.path)).toBe(file);
    forgetLocalFile(info.path);
    expect(getLocalFile(info.path)).toBeUndefined();
  });

  it("extracts extensions conservatively", () => {
    expect(extensionOf("a.tar.GZ")).toBe("gz");
    expect(extensionOf(".hidden")).toBe("");
    expect(extensionOf("noext")).toBe("");
  });
});

describe("watchJob (polling path)", () => {
  const snapshot = (seq: number, state: WebJobSnapshot["state"], progress: number | null = null): WebJobSnapshot => ({
    job_id: "11111111-1111-4111-8111-111111111111",
    kind: "convert",
    file_id: "22222222-2222-4222-8222-222222222222",
    output_format: "png",
    state,
    progress_pct: progress,
    error: null,
    result: null,
    created_at_ms: 0,
    updated_at_ms: 0,
    seq,
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("resolves with the terminal snapshot and never reports state going backwards", async () => {
    // jsdom has no EventSource, so watchJob takes its polling path - the
    // same path a browser falls back to when the SSE stream errors.
    const responses = [snapshot(1, "running", 25), snapshot(0, "queued"), snapshot(3, "running", 75), snapshot(4, "completed", 100)];
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify(responses.shift()), { status: 200 }))
    );
    const seen: Array<[number, string]> = [];
    const final = await watchJob("11111111-1111-4111-8111-111111111111", (s) => seen.push([s.seq, s.state]), 1);
    expect(final.state).toBe("completed");
    expect(seen).toEqual([
      [1, "running"],
      [3, "running"],
      [4, "completed"],
    ]);
  });

  it("rejects with the structured server error (e.g. a foreign id is just NOT_FOUND)", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: { code: "NOT_FOUND", message: "x" } }), { status: 404 }))
    );
    await expect(watchJob("33333333-3333-4333-8333-333333333333", () => {}, 1)).rejects.toMatchObject({
      code: "NOT_FOUND",
    });
  });
});
