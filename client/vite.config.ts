import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

/**
 * In development the client and the API are two processes: Vite serves the app
 * on :5173 and proxies `/api` to the Rust server on :8787. In production the
 * server serves the built client itself, so this proxy has no production
 * counterpart — the paths are simply same-origin there.
 */
export default defineConfig({
  plugins: [sveltekit()],
  server: {
    port: 5173,
    strictPort: false,
    proxy: {
      '/api': {
        target: process.env.API_TARGET ?? 'http://127.0.0.1:8787',
        changeOrigin: true,
      },
    },
  },
});
