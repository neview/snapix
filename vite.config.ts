/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import { resolve } from "node:path";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],

  resolve: {
    alias: {
      "@": resolve(__dirname, "src"),
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1430,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1431,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },

  build: {
    // Multi-window: each Tauri window loads its own HTML entry.
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        overlay: resolve(__dirname, "overlay.html"),
        editor: resolve(__dirname, "editor.html"),
        pin: resolve(__dirname, "pin.html"),
        scroll: resolve(__dirname, "scroll.html"),
        gif: resolve(__dirname, "gif.html"),
        line: resolve(__dirname, "line.html"),
      },
    },
  },

  // Vitest：只测纯逻辑（store / beautify），不依赖 DOM 与 Tauri 运行时
  test: {
    include: ["src/**/*.test.ts"],
    environment: "node",
  },
}));
