import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// CL Recoder 前端（Tauri v2 WebView；tauri.conf.json devUrl 锚定 5173 端口）
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  esbuild: {
    jsx: "automatic",
    jsxImportSource: "react",
  },
});
