import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

/**
 * Separate from `vite.config.ts` so the app build does not carry test config.
 *
 * Most of what is tested here is pure — the command parser, the chord/keymap
 * logic, the formatters — so the default environment is Node. A file that needs
 * a document opts in by naming itself `*.dom.test.ts`.
 *
 * The Svelte plugin is here because a `.svelte.ts` module (`state/workspace`)
 * is runes source, not plain TypeScript: without it `$state` is an undefined
 * identifier at run time. Being separate from `vite.config.ts` means this file
 * does not inherit the app's plugins, so it has to name the one it needs.
 */
export default defineConfig({
  plugins: [svelte()],
  test: {
    include: ['src/**/*.{test,spec}.{js,ts}'],
    environment: 'node',
    environmentMatchGlobs: [['src/**/*.dom.{test,spec}.ts', 'jsdom']],
  },
});
