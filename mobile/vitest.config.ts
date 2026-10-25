import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

const root = fileURLToPath(new URL(".", import.meta.url));

export default defineConfig({
  root,
  resolve: {
    alias: {
      "@": `${root}src`,
      react: `${root}node_modules/react`,
      "react-dom": `${root}node_modules/react-dom`,
    },
  },
  esbuild: { jsx: "automatic" },
  // The protocol round-trip suite lives just outside the mobile project.
  server: { fs: { allow: [fileURLToPath(new URL("..", import.meta.url))] } },
  test: {
    environment: "happy-dom",
    // Protocol round trips need both the desktop and mobile dependency trees.
    include: [
      "tests/**/*.test.ts",
      "tests/**/*.test.tsx",
      "../tests/mobile/**/*.test.ts",
    ],
    maxWorkers: 1,
    minWorkers: 1,
  },
});
