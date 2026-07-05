import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';
import { version } from './package.json';

// Vite + Vitest config. The WebUI's only seam to the backend is `src/services/` — the
// dev server proxies `/api` to Yagra-core so the typed client uses relative URLs.
// (Vite/Vitest transpile this file with esbuild; it is not part of the `tsc` typecheck.)
export default defineConfig({
  plugins: [react()],
  // Compile-time WebUI build version, from package.json (mirrors the workspace version). Shown on
  // Settings ▸ About next to the running core version so a core/web skew is visible.
  define: {
    __APP_VERSION__: JSON.stringify(version),
  },
  server: {
    proxy: {
      '/api': { target: 'http://localhost:8080', changeOrigin: true },
    },
  },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
});
