import { svelte, vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';

/**
 * The snapshot build: the same app, bundled flat for a host that runs nothing.
 *
 * Plain Vite rather than SvelteKit, for the reason `snapshot/main.ts` gives —
 * SvelteKit's route loader builds its chunk URLs at runtime, which no bundler
 * can follow into a single file. Everything else is shared: the same
 * components, the same stores, the same `src/app.css`. Only the entry differs.
 *
 * The two aliases restate what SvelteKit provides for the normal build: `$lib`
 * is its convention, `$gen` is declared in `svelte.config.js`. They must keep
 * naming the same directories as that file, or the two builds compile different
 * source.
 */
const dir = (p: string) => fileURLToPath(new URL(p, import.meta.url));

export default defineConfig({
  root: dir('./snapshot'),
  // Stated rather than defaulted: `svelte.config.js` preprocesses the normal
  // build, and this root has no such file for the plugin to find.
  plugins: [svelte({ preprocess: vitePreprocess() })],
  resolve: {
    alias: {
      $lib: dir('./src/lib'),
      $gen: dir('./src/lib/api/gen'),
    },
  },
  build: {
    outDir: dir('./snapshot-build'),
    emptyOutDir: true,
    // One file at the end means one chunk now: no split points for the packer
    // to have to chase, and no stylesheet arriving as a second request.
    cssCodeSplit: false,
    // The snapshot payload is the bulk of the page and is inlined by the packer
    // afterwards; warning about chunk size here would only ever be noise.
    chunkSizeWarningLimit: 4096,
    rollupOptions: {
      output: { inlineDynamicImports: true, manualChunks: undefined },
    },
  },
});
