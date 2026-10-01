import { describe, it, expect, beforeEach, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  useStore,
  getFilesToConvert,
  getActiveConversionCandidates,
  commonOutputFormat,
  resolveOutputFormat,
  ConversionFile,
  Capability,
  Category,
} from "./useStore";
import { getBlockedCapability } from "../utils/capabilityGating";

function makeFile(overrides: Partial<ConversionFile> & { id: string; category: string }): ConversionFile {
  return {
    path: `/tmp/${overrides.id}`,
    name: overrides.id,
    extension: "bin",
    size: 100,
    status: "pending",
    progress: 0,
    outputFormat: null,
    outputPath: null,
    error: null,
    etaSecs: null,
    speed: null,
    previewUrl: null,
    previewLoading: false,
    ...overrides,
  } as ConversionFile;
}

describe("getFilesToConvert (Step 3 finding #5)", () => {
  it("switching category away from video clears the video block for an image batch", () => {
    const video = makeFile({ id: "v1", category: "video" });
    const png = makeFile({ id: "p1", category: "image" });

    const whileOnVideo = getFilesToConvert({
      files: [video, png],
      selectedFiles: [],
      activeCategory: "video",
    });
    expect(whileOnVideo.map((f) => f.id)).toEqual(["v1"]);

    const afterSwitchToImage = getFilesToConvert({
      files: [video, png],
      selectedFiles: [],
      activeCategory: "image",
    });
    // The video file must not be part of the batch once the user has
    // switched to Image - it previously stayed in scope and let a blocked
    // video capability disable an unrelated PNG conversion.
    expect(afterSwitchToImage.map((f) => f.id)).toEqual(["p1"]);
  });

  it("'all' category includes every pending file regardless of category", () => {
    const video = makeFile({ id: "v1", category: "video" });
    const png = makeFile({ id: "p1", category: "image" });
    const result = getFilesToConvert({ files: [video, png], selectedFiles: [], activeCategory: "all" });
    expect(result.map((f) => f.id).sort()).toEqual(["p1", "v1"]);
  });

  it("excludes non-pending files even when explicitly selected", () => {
    const done = makeFile({ id: "d1", category: "image", status: "completed" });
    const result = getFilesToConvert({ files: [done], selectedFiles: ["d1"], activeCategory: "all" });
    expect(result).toEqual([]);
  });
});

// Step 4 UI-hardening pass, Bug 1: the capability warning banner (and
// therefore the Convert button's enabled state) must be driven by the
// ACTIVE conversion candidates, not merely every pending file
// `getFilesToConvert` would implicitly batch - see
// getActiveConversionCandidates' doc comment for the full rationale.
// `findBlockedCapability` mirrors exactly what ConversionPanel does:
// resolve the active candidates, then run them through the same
// `getBlockedCapability` gate the real panel uses.
describe("getActiveConversionCandidates + getBlockedCapability (Step 4 Bug 1)", () => {
  const ENGINE_MISSING: Capability = {
    id: "video_conversion",
    state: "ENGINE_MISSING",
    message: "FFmpeg not bundled",
  };
  const getCapability = (id: string) => (id === "video_conversion" ? ENGINE_MISSING : undefined);

  const findBlockedCapability = (state: {
    files: ConversionFile[];
    selectedFiles: string[];
    activeCategory: Category;
    globalOutputFormat: string | null;
  }) => getBlockedCapability(getActiveConversionCandidates(state), getCapability);

  it("TEST 1: an unselected video file with no chosen format does not raise a capability warning", () => {
    const video = makeFile({ id: "v1", category: "video" });
    const blocked = findBlockedCapability({
      files: [video],
      selectedFiles: [],
      activeCategory: "all",
      globalOutputFormat: null,
    });
    expect(blocked).toBeUndefined();
  });

  it("TEST 2: an explicitly selected video file does raise the warning when its engine is missing", () => {
    const video = makeFile({ id: "v1", category: "video" });
    const blocked = findBlockedCapability({
      files: [video],
      selectedFiles: ["v1"],
      activeCategory: "all",
      globalOutputFormat: null,
    });
    expect(blocked).toBe(ENGINE_MISSING);
  });

  it("TEST 3: selecting a PNG while an unrelated unselected video remains loaded shows no video warning", () => {
    const video = makeFile({ id: "v1", category: "video" });
    const png = makeFile({ id: "p1", category: "image" });
    const blocked = findBlockedCapability({
      files: [video, png],
      selectedFiles: ["p1"],
      activeCategory: "all",
      globalOutputFormat: null,
    });
    expect(blocked).toBeUndefined();
  });

  it("TEST 4: switching category away from the blocked file's category immediately clears the warning", () => {
    const video = makeFile({ id: "v1", category: "video", outputFormat: "mp4" });
    const png = makeFile({ id: "p1", category: "image", outputFormat: "jpg" });

    const onVideoCategory = findBlockedCapability({
      files: [video, png],
      selectedFiles: [],
      activeCategory: "video",
      globalOutputFormat: null,
    });
    expect(onVideoCategory).toBe(ENGINE_MISSING);

    const onImageCategory = findBlockedCapability({
      files: [video, png],
      selectedFiles: [],
      activeCategory: "image",
      globalOutputFormat: null,
    });
    expect(onImageCategory).toBeUndefined();
  });

  it("TEST 5: completed/error files never contribute to the current capability warning", () => {
    const doneVideo = makeFile({ id: "v1", category: "video", status: "completed", outputFormat: "mp4" });
    const erroredVideo = makeFile({ id: "v2", category: "video", status: "error", outputFormat: "mp4" });
    const png = makeFile({ id: "p1", category: "image", outputFormat: "jpg" });

    const blocked = findBlockedCapability({
      files: [doneVideo, erroredVideo, png],
      selectedFiles: [],
      activeCategory: "all",
      globalOutputFormat: null,
    });
    expect(blocked).toBeUndefined();
  });

  it("a no-selection video does count once it has been assigned an output format (per-file or global)", () => {
    const video = makeFile({ id: "v1", category: "video", outputFormat: "mp4" });
    const blocked = findBlockedCapability({
      files: [video],
      selectedFiles: [],
      activeCategory: "all",
      globalOutputFormat: null,
    });
    expect(blocked).toBe(ENGINE_MISSING);
  });
});

describe("useStore selection/category reset behavior (Step 3 findings #5/#6)", () => {
  beforeEach(() => {
    useStore.setState({
      files: [],
      selectedFiles: [],
      activeCategory: "all",
      globalOutputFormat: null,
    });
  });

  it("drops a file from selectedFiles once it completes, so a new unselected file is still picked up", () => {
    const a = makeFile({ id: "a", category: "image", status: "pending" });
    useStore.setState({ files: [a], selectedFiles: ["a"] });

    useStore.getState().setFileStatus("a", "completed", 100, undefined, "/out/a.png");
    expect(useStore.getState().selectedFiles).toEqual([]);

    // A brand new file, never explicitly selected, must now be part of the
    // next batch instead of being silently excluded (previously required
    // pressing "Temizle" to recover).
    const b = makeFile({ id: "b", category: "image", status: "pending" });
    useStore.setState((s) => ({ files: [...s.files, b] }));

    const batch = getFilesToConvert(useStore.getState());
    expect(batch.map((f) => f.id)).toEqual(["b"]);
  });

  it("drops a file from selectedFiles on error too, not just on success", () => {
    const a = makeFile({ id: "a", category: "video", status: "pending" });
    useStore.setState({ files: [a], selectedFiles: ["a"] });

    useStore.getState().setFileStatus("a", "error", 0, "PROCESS_FAILED");
    expect(useStore.getState().selectedFiles).toEqual([]);
  });

  it("clears globalOutputFormat when switching to a different category", () => {
    useStore.setState({ activeCategory: "video", globalOutputFormat: "mp4" });
    useStore.getState().setActiveCategory("image");
    expect(useStore.getState().globalOutputFormat).toBeNull();
  });

  it("keeps globalOutputFormat when re-selecting the same category", () => {
    useStore.setState({ activeCategory: "image", globalOutputFormat: "webp" });
    useStore.getState().setActiveCategory("image");
    expect(useStore.getState().globalOutputFormat).toBe("webp");
  });
});

describe("output format single source of truth (format-sync fix pass, finding #1)", () => {
  beforeEach(() => {
    useStore.setState({ files: [], selectedFiles: [], activeCategory: "all", globalOutputFormat: null });
  });

  it("resolveOutputFormat prefers the per-file value over the global fallback", () => {
    expect(resolveOutputFormat({ outputFormat: "jpg" }, "png")).toBe("jpg");
    expect(resolveOutputFormat({ outputFormat: null }, "png")).toBe("png");
  });

  it("TEST 1: selecting JPEG on a PNG FileCard is immediately reflected as the panel's common format", () => {
    const png = makeFile({ id: "p1", category: "image", extension: "png" });
    useStore.setState({ files: [png] });

    // Simulates the FileCard's format dropdown calling setFileOutputFormat.
    useStore.getState().setFileOutputFormat("p1", "jpg");

    const filesToConvert = getFilesToConvert(useStore.getState());
    expect(commonOutputFormat(filesToConvert, useStore.getState().globalOutputFormat)).toBe("jpg");
  });

  it("TEST 2: selecting PNG from ConversionPanel writes back to the file's own outputFormat", () => {
    const jpg = makeFile({ id: "j1", category: "image", extension: "jpg", outputFormat: "webp" });
    useStore.setState({ files: [jpg] });

    // Simulates ConversionPanel's handleSelectTargetFormat.
    useStore.getState().setOutputFormatForFiles(["j1"], "png");

    expect(useStore.getState().files[0].outputFormat).toBe("png");
  });

  it("applying a shared target format updates every file currently in scope, not just one", () => {
    const a = makeFile({ id: "a", category: "image", extension: "png" });
    const b = makeFile({ id: "b", category: "image", extension: "png" });
    useStore.setState({ files: [a, b] });

    useStore.getState().setOutputFormatForFiles(["a", "b"], "webp");

    expect(useStore.getState().files.map((f) => f.outputFormat)).toEqual(["webp", "webp"]);
  });

  it("TEST 4: a newly added compatible file inherits the shared format without re-selection", () => {
    const a = makeFile({ id: "a", category: "image", extension: "png", outputFormat: "jpg" });
    useStore.setState({ files: [a], globalOutputFormat: "jpg" });

    const b = makeFile({ id: "b", category: "image", extension: "png" }); // no explicit choice yet
    useStore.setState((s) => ({ files: [...s.files, b] }));

    const filesToConvert = getFilesToConvert(useStore.getState());
    expect(commonOutputFormat(filesToConvert, useStore.getState().globalOutputFormat)).toBe("jpg");
  });

  it("TEST 5: mismatched per-file formats never resolve to a single silently-forced common value", () => {
    const a = makeFile({ id: "a", category: "image", extension: "png", outputFormat: "jpg" });
    const b = makeFile({ id: "b", category: "image", extension: "gif", outputFormat: "webp" });
    expect(commonOutputFormat([a, b], null)).toBeNull();
  });

  it("TEST 6: convertFiles resolves the same per-file value the UI displayed (outputFormat wins over global)", () => {
    const a = makeFile({ id: "a", category: "image", extension: "png", outputFormat: "webp" });
    useStore.setState({ files: [a], globalOutputFormat: "jpg" });
    // This is exactly the resolution rule convertSingleFile uses internally
    // (`file.outputFormat || globalOutputFormat`) - asserted here via the
    // shared pure helper so UI and execution can never diverge.
    expect(resolveOutputFormat(useStore.getState().files[0], useStore.getState().globalOutputFormat)).toBe("webp");
  });
});

// Step 4 UI-acceptance fix pass: Bug 1 (contradictory success/error UI) and
// Bug 2 (ConversionPanel unrecoverable after error) - see CLAUDE.md-adjacent
// task notes / commit history for the manual repro. These tests cover the
// authoritative terminal-state model (setFileStatus), the explicit retry
// path (retryFile), and convertFiles' per-batch result contract that the
// toast decision in ConversionPanel now relies on instead of "the promise
// resolved".
describe("terminal conversion state (Step 4 Bug 1/D: single authoritative result)", () => {
  beforeEach(() => {
    useStore.setState({ files: [], selectedFiles: [], activeCategory: "all", globalOutputFormat: null });
  });

  it("a successful terminal transition clears any stale error from a previous attempt", () => {
    const a = makeFile({ id: "a", category: "document", status: "error", error: "PROCESS_FAILED" });
    useStore.setState({ files: [a] });

    useStore.getState().setFileStatus("a", "completed", 100, undefined, "/out/a.pdf");

    const file = useStore.getState().files[0];
    expect(file.status).toBe("completed");
    expect(file.error).toBeNull();
  });

  it("starting a new attempt (converting) clears any stale error before the result is known", () => {
    const a = makeFile({ id: "a", category: "document", status: "error", error: "PROCESS_FAILED" });
    useStore.setState({ files: [a] });

    useStore.getState().setFileStatus("a", "converting", 0);

    expect(useStore.getState().files[0].error).toBeNull();
  });

  it("a failed terminal transition sets error and status together, exactly once", () => {
    const a = makeFile({ id: "a", category: "document", status: "converting" });
    useStore.setState({ files: [a] });

    useStore.getState().setFileStatus("a", "error", 0, "PROCESS_FAILED");

    const file = useStore.getState().files[0];
    expect(file.status).toBe("error");
    expect(file.error).toBe("PROCESS_FAILED");
  });
});

describe("retryFile (Step 4 Bug 2/E/F: explicit retry semantics)", () => {
  beforeEach(() => {
    useStore.setState({ files: [], selectedFiles: [], activeCategory: "all", globalOutputFormat: null });
  });

  it("moves an errored file back to pending and clears its stale terminal result", () => {
    const a = makeFile({
      id: "a",
      category: "document",
      extension: "docx",
      status: "error",
      error: "PROCESS_FAILED",
      outputFormat: "pdf",
      outputPath: null,
      progress: 0,
    });
    useStore.setState({ files: [a], selectedFiles: [] });

    useStore.getState().retryFile("a");

    const file = useStore.getState().files[0];
    expect(file.status).toBe("pending");
    expect(file.error).toBeNull();
    // The chosen output format must survive a retry - the user shouldn't
    // have to re-pick PDF just because the previous attempt failed.
    expect(file.outputFormat).toBe("pdf");
  });

  it("an errored file can re-enter conversion candidates only after retryFile (finding #6)", () => {
    const a = makeFile({ id: "a", category: "document", status: "error", error: "PROCESS_FAILED" });
    useStore.setState({ files: [a], selectedFiles: [] });

    // Before retry: a genuinely terminal error is not a candidate, even if
    // explicitly (re)selected - this is unchanged, intentional behavior.
    useStore.getState().selectFile("a");
    expect(getFilesToConvert(useStore.getState())).toEqual([]);

    useStore.getState().retryFile("a");

    const batch = getFilesToConvert(useStore.getState());
    expect(batch.map((f) => f.id)).toEqual(["a"]);
  });

  it("retrying re-selects the file so it isn't excluded by an unrelated existing selection (section K)", () => {
    const a = makeFile({ id: "a", category: "image", status: "error", error: "PROCESS_FAILED" });
    const b = makeFile({ id: "b", category: "image", status: "pending" });
    useStore.setState({ files: [a, b], selectedFiles: ["b"] });

    useStore.getState().retryFile("a");

    // selectedFiles is non-empty (still has "b"), which puts
    // getFilesToConvert into "selection-only" mode - "a" must be part of
    // that selection now, not silently dropped from the batch.
    expect(useStore.getState().selectedFiles).toEqual(expect.arrayContaining(["a", "b"]));
    const batch = getFilesToConvert(useStore.getState());
    expect(batch.map((f) => f.id).sort()).toEqual(["a", "b"]);
  });

  it("failed file + new valid file in a different category: the old error never blocks the new one (section H/I)", () => {
    const officeFile = makeFile({ id: "doc1", category: "document", status: "error", error: "PROCESS_FAILED" });
    useStore.setState({ files: [officeFile], selectedFiles: [], activeCategory: "document" });

    // Switch category and add a brand-new valid file, exactly like the
    // manual repro (Office error -> Image -> add PNG -> choose JPEG).
    useStore.getState().setActiveCategory("image");
    const png = makeFile({ id: "p1", category: "image", extension: "png", status: "pending", outputFormat: "jpg" });
    useStore.setState((s) => ({ files: [...s.files, png] }));

    const batch = getFilesToConvert(useStore.getState());
    expect(batch.map((f) => f.id)).toEqual(["p1"]);
    // The Office file is still there, still errored, but out of scope.
    expect(useStore.getState().files.find((f) => f.id === "doc1")?.status).toBe("error");
  });
});

describe("convertFiles batch result contract (Step 4 Bug 1/B/C)", () => {
  beforeEach(() => {
    useStore.setState({
      files: [],
      selectedFiles: [],
      activeCategory: "all",
      globalOutputFormat: null,
      isConverting: false,
      activeConversions: new Set(),
      currentJobId: null,
    });
    vi.mocked(invoke).mockReset();
  });

  it("a fully successful batch reports the file as succeeded, using the backend's authoritative output_path", () => {
    const a = makeFile({ id: "a", category: "document", extension: "docx", outputFormat: "pdf", status: "pending" });
    useStore.setState({ files: [a] });

    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "convert_file") {
        return Promise.resolve({
          success: true,
          output_path: "/out/a (1).pdf", // collision-renamed by the backend
          error: null,
          duration_ms: 10,
        });
      }
      return Promise.resolve(undefined);
    });

    return useStore.getState().convertFiles().then((result) => {
      expect(result.succeeded).toEqual(["a"]);
      expect(result.failed).toEqual([]);
      const file = useStore.getState().files[0];
      expect(file.status).toBe("completed");
      expect(file.outputPath).toBe("/out/a (1).pdf");
      expect(file.error).toBeNull();
    });
  });

  it("a failed conversion never reports as succeeded, and the file ends in error only", () => {
    const a = makeFile({ id: "a", category: "document", extension: "docx", outputFormat: "pdf", status: "pending" });
    useStore.setState({ files: [a] });

    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "convert_file") {
        return Promise.resolve({
          success: false,
          output_path: null,
          error: "PROCESS_FAILED",
          duration_ms: 10,
        });
      }
      return Promise.resolve(undefined);
    });

    return useStore.getState().convertFiles().then((result) => {
      expect(result.failed).toEqual(["a"]);
      expect(result.succeeded).toEqual([]);
      const file = useStore.getState().files[0];
      expect(file.status).toBe("error");
      expect(file.error).toBe("PROCESS_FAILED");
    });
  });

  it("failure -> retry -> success ends in completed with no stale error, and the batch reports success only", () => {
    const a = makeFile({ id: "a", category: "document", extension: "docx", outputFormat: "pdf", status: "pending" });
    useStore.setState({ files: [a] });

    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "convert_file") {
        return Promise.resolve({ success: false, output_path: null, error: "PROCESS_FAILED", duration_ms: 5 });
      }
      return Promise.resolve(undefined);
    });

    return useStore
      .getState()
      .convertFiles()
      .then((firstResult) => {
        expect(firstResult.failed).toEqual(["a"]);
        expect(useStore.getState().files[0].status).toBe("error");

        useStore.getState().retryFile("a");
        expect(useStore.getState().files[0].status).toBe("pending");
        expect(useStore.getState().files[0].error).toBeNull();

        vi.mocked(invoke).mockImplementation((cmd: string) => {
          if (cmd === "convert_file") {
            return Promise.resolve({ success: true, output_path: "/out/a.pdf", error: null, duration_ms: 5 });
          }
          return Promise.resolve(undefined);
        });

        return useStore.getState().convertFiles();
      })
      .then((secondResult) => {
        expect(secondResult.succeeded).toEqual(["a"]);
        expect(secondResult.failed).toEqual([]);
        const file = useStore.getState().files[0];
        expect(file.status).toBe("completed");
        expect(file.error).toBeNull();
        expect(file.outputPath).toBe("/out/a.pdf");
      });
  });
});
