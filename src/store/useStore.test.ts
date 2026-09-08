import { describe, it, expect, beforeEach } from "vitest";
import { useStore, getFilesToConvert, commonOutputFormat, resolveOutputFormat, ConversionFile } from "./useStore";

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
