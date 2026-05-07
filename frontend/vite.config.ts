import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

// Vite dev server runs on :5173. Proxies /api/* to the FastAPI server on
// :8765 so REST + SSE both work seamlessly during development.
//
// Production: `npm run build` emits dist/ which FastAPI mounts at /.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8765",
        changeOrigin: true,
        // SSE needs streaming responses — disable any buffering.
        configure: (proxy) => {
          proxy.on("proxyRes", (proxyRes) => {
            // Strip any compression / buffering headers that would break SSE.
            delete proxyRes.headers["content-length"];
          });
        },
      },
    },
  },
  build: {
    outDir: "dist",
    sourcemap: true,
    // Hash assets so FastAPI can serve them with aggressive caching while
    // index.html stays revalidated.
    assetsDir: "assets",
  },
});
