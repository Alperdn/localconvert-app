// Which backend the UI talks to.
//
// - Desktop (default): the Tauri webview - invoke()/events/dialog/fs.
// - Web: a browser talking to `meb-server` over HTTP (`npm run dev:web` /
//   `npm run build:web`, i.e. Vite `--mode web`).
//
// Decided at build time (Vite mode), not by sniffing `window.__TAURI__`, so
// the desktop build and its tests behave exactly as before and a web build
// can never silently fall back to Tauri APIs.
export const IS_WEB_RUNTIME: boolean = import.meta.env.MODE === "web";
