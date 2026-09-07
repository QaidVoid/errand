import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";

/**
 * The daemon serves the built assets itself, so there is no dev server in
 * production and every asset path is relative to wherever it is mounted.
 */
export default defineConfig({
  plugins: [svelte()],
  base: "./",
  build: {
    outDir: "../dist/web",
    emptyOutDir: true,
    target: "es2022",
  },
  server: {
    // Development proxies the API to the running daemon, so the interface can
    // be edited against real sessions rather than against fixtures.
    proxy: { "/api": "http://127.0.0.1:8787" },
  },
});
