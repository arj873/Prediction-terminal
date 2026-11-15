import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

/**
 * SvelteKit 3 no longer reads `svelte.config.js`; its configuration is passed
 * to the `sveltekit(...)` plugin here instead.
 *
 * The terminal is a single screen with no URL routing and no server rendering:
 * the workspace lives in localStorage, panels are opened by typed commands, and
 * the chart library reads CSS custom properties off a live document. So the
 * client builds to static files with an SPA fallback, and the Rust server
 * serves them — one process, one port, same as the Node server it replaces.
 *
 * In development the client and the API are two processes: Vite serves the app
 * on :5173 and proxies `/api` to the Rust server on :8787. In production the
 * server serves the built client itself, so this proxy has no production
 * counterpart — the paths are simply same-origin there.
 */
export default defineConfig({
  plugins: [
    sveltekit({
      preprocess: vitePreprocess(),
      adapter: adapter({
        pages: 'build',
        assets: 'build',
        fallback: 'index.html',
        precompress: false,
        strict: true,
      }),
      alias: {
        // SvelteKit 3 removed the built-in `$lib` alias in favour of `#lib`
        // subpath imports; re-declaring it here keeps the existing imports
        // working, alongside the project's own `$gen` alias.
        $lib: 'src/lib',
        $gen: 'src/lib/api/gen',
      },
    }),
  ],
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
