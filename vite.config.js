import { defineConfig } from "vite";

// Tauri expects a fixed dev port and must not have the screen cleared under it.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**", "**/target/**", "**/data/**", "**/logs/**", "**/crates/**"] } },
  build: { target: "es2022" },
  // MapLibre 6 loads its worker from a sibling file (new URL(..., import.meta.url)); the
  // pre-bundler moves the entry into .vite/deps without that file, so leave it unbundled.
  optimizeDeps: { exclude: ["maplibre-gl"] },
});
