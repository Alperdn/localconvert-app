import { vi } from "vitest";

// useStore.ts calls invoke()/listen() at module load time (e.g.
// get_default_output_dir) and reads localStorage/matchMedia to resolve the
// theme - none of that exists outside the real Tauri webview, so it must be
// stubbed before any test imports the store.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve(undefined)),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

// jsdom doesn't implement matchMedia; useStore.ts uses it to resolve the
// "system" theme preference at module load and on OS-scheme change.
window.matchMedia =
  window.matchMedia ||
  ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  }));
