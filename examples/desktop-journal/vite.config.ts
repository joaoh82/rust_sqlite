import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

// Tauri 2 + Vite. The dev server runs on port 1421 so it doesn't
// collide with the existing SQL playground (`desktop/`, port 1420) —
// both can be open at once during dev. Bumps to Tailwind v4 use the
// official Vite plugin instead of PostCSS, per the v4 docs.
//   See https://v2.tauri.app/start/frontend/vite/
export default defineConfig(async () => ({
  plugins: [svelte(), tailwindcss()],

  clearScreen: false,

  server: {
    port: 1421,
    strictPort: true,
    host: false,
  },

  envPrefix: ["VITE_", "TAURI_"],

  build: {
    target: process.env.TAURI_ENV_PLATFORM === "windows"
      ? "chrome105"
      : "safari13",
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
}));
