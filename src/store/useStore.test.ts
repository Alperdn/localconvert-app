import { describe, it, expect, beforeEach } from "vitest";
import { useStore, getFilesToConvert, ConversionFile } from "./useStore";

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
