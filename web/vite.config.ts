import { defineConfig } from "vite";

export default defineConfig({
  build: {
    rollupOptions: {
      input: {
        main: new URL("./index.html", import.meta.url).pathname,
        compare: new URL("./compare/index.html", import.meta.url).pathname,
      },
    },
  },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:3101",
    },
  },
});
