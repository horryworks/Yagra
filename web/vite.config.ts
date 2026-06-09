import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

// Vite + Vitest config. The WebUI's only seam to the backend is `src/services/` — the
// dev server proxies `/api` to Yagra-core so the typed client uses relative URLs.
// (Vite/Vitest transpile this file with esbuild; it is not part of the `tsc` typecheck.)
export default defineConfig({
  plugins: [react()],
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
