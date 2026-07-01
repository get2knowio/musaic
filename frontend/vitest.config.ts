import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import path from "node:path";

// Separate from vite.config.ts on purpose: the dev server's /api proxy +
// SSE header munging are irrelevant under test, and keeping the test
// config lean means a vite-plugin change there can't silently alter how
// tests resolve. Mirrors only the `@` alias + the React plugin (needed to
// transform JSX/TSX in component + hook tests).
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    css: false,
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
  },
});
