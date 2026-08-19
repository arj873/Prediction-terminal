import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/**
 * The terminal is a single screen with no URL routing and no server rendering:
 * the workspace lives in localStorage, panels are opened by typed commands, and
 * the chart library reads CSS custom properties off a live document. So the
 * client builds to static files with an SPA fallback, and the Rust server
 * serves them — one process, one port, same as the Node server it replaces.
 *
 * @type {import('@sveltejs/kit').Config}
 */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      pages: 'build',
      assets: 'build',
      fallback: 'index.html',
      precompress: false,
      strict: true,
    }),
    alias: {
      $gen: 'src/lib/api/gen',
    },
  },
};

export default config;
