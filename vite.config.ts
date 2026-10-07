import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**", "**/crates/**"],
    },
    // Web runtime (`npm run dev:web`): the browser talks to meb-server
    // through this same-origin proxy, so the session cookie and every API
    // call stay on the page's origin (no CORS). Unused by the Tauri app.
    proxy: {
      "/api": { target: "http://127.0.0.1:8787", changeOrigin: false },
    },
  },
  optimizeDeps: {
    include: ["pdfjs-dist"],
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks: {
          pdfjs: ["pdfjs-dist"],
          fabric: ["fabric"],
          pdflib: ["pdf-lib"],
        },
      },
    },
  },
}));
