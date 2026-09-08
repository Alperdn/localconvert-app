import { describe, it, expect } from "vitest";
import {
  capabilityIdForFile,
  getBlockedCapability,
  isCapabilityUsable,
} from "./capabilityGating";
import type { Capability } from "../store/useStore";

const cap = (id: string, state: Capability["state"]): Capability => ({
  id,
  state,
  message: "",
});

const file = (category: string, extension: string) => ({ category, extension });

describe("capabilityIdForFile", () => {
  it("maps video files to video_conversion", () => {
    expect(capabilityIdForFile(file("video", "mp4"))).toBe("video_conversion");
  });

  it("maps Office document extensions to office_to_pdf", () => {
    expect(capabilityIdForFile(file("document", "docx"))).toBe("office_to_pdf");
  });

  it("does not gate native image formats", () => {
    expect(capabilityIdForFile(file("image", "png"))).toBeNull();
    expect(capabilityIdForFile(file("image", "jpg"))).toBeNull();
  });
});

describe("getBlockedCapability (Step 3 finding #3/#5)", () => {
  const getCapability = (caps: Capability[]) => (id: string) =>
    caps.find((c) => c.id === id);

  it("blocks when the only file's engine is missing", () => {
    const caps = [cap("office_to_pdf", "ENGINE_MISSING")];
    const blocked = getBlockedCapability([file("document", "docx")], getCapability(caps));
    expect(blocked?.id).toBe("office_to_pdf");
  });

  it("never blocks a native image operation, even if an unrelated video capability is missing", () => {
    // Regression test for Step 3 finding #5: a blocked video capability
    // must not leak into an unrelated PNG/JPEG batch.
    const caps = [cap("video_conversion", "ENGINE_MISSING")];
    const blocked = getBlockedCapability([file("image", "png")], getCapability(caps));
    expect(blocked).toBeUndefined();
  });

  it("is undefined when every file's capability is AVAILABLE or ungated", () => {
    const caps = [cap("video_conversion", "AVAILABLE")];
    const blocked = getBlockedCapability(
      [file("video", "mp4"), file("image", "png")],
      getCapability(caps)
    );
    expect(blocked).toBeUndefined();
  });

  it("treats an unmodeled capability as usable (unknown != blocked)", () => {
    expect(isCapabilityUsable(undefined)).toBe(true);
  });
});
