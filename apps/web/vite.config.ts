import { defineConfig, loadEnv } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

// The Rust bot serves the built assets. `base` must match the route the
// embedded axum server mounts them under.
const BASE = "/app/";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const apiTarget = env.ZDX_API_TARGET || "http://127.0.0.1:4141";

  return {
    base: BASE,
    plugins: [svelte(), tailwindcss()],
    resolve: {
      alias: { $lib: path.resolve(__dirname, "src/lib") },
    },
    build: {
      target: "es2022",
      outDir: "dist",
      emptyOutDir: true,
    },
    server: {
      port: 5173,
      proxy: {
        // Dev: forward API calls to the running `zdx bot` embedded server.
        "/api": { target: apiTarget, changeOrigin: true },
      },
    },
  };
});
