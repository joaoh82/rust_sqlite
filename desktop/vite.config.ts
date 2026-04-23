import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Tauri 2 expects the dev server on a fixed port (configured in
// tauri.conf.json) and a predictable build output directory.
// See https://v2.tauri.app/start/frontend/vite/
export default defineConfig(async () => ({
  plugins: [svelte()],

  // Prevent Vite from obscuring Rust errors.
  clearScreen: false,

  server: {
    port: 1420,
    strictPort: true,
    host: false,
  },

  // Env variables starting with `TAURI_` get forwarded to the app.
  envPrefix: ["VITE_", "TAURI_"],

  build: {
    // Chromium on Windows (Edge WebView2) needs ES2021; other Tauri
    // webviews are at least as new.
    target: process.env.TAURI_ENV_PLATFORM === "windows"
      ? "chrome105"
      : "safari13",
    // Don't minify in dev builds — easier to diagnose.
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
}));
