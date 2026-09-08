import { defineConfig } from "vitest/config";
import path from "node:path";

// Minimal test runner for the pure store/selector logic covered by Step 3's
// manual-test fix pass (see src/store/useStore.test.ts,
// src/utils/capabilityGating.test.ts). Not wired into `npm run build` /
// `npm run tauri build` - run explicitly via `npm test`.
export default defineConfig({
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
  },
});
