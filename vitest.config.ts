import preact from "@preact/preset-vite";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [preact()],
  test: {
    environment: "jsdom",
    globals: true,
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    // src/test/tauri.ts keeps its implementation across tests; restoring mocks
    // would strip it. State is reset explicitly in src/test/setup.ts instead.
    restoreMocks: false,
    setupFiles: ["src/test/setup.ts"],
  },
});
