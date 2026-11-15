import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

/**
 * Separate from `vite.config.ts` so the app build does not carry test config.
 *
 * Most of what is tested here is pure — the command parser, the chord/keymap
 * logic, the formatters — so the default environment is Node. A file that needs
 * a document opts in by naming itself `*.dom.test.ts`, which the jsdom project
 * below picks up; everything else runs under Node.
 *
 * The split is expressed with `test.projects` because Vitest removed the older
 * `environmentMatchGlobs` option: one project per environment, each extending
 * this file so both inherit the Svelte plugin.
 *
 * The Svelte plugin is here because a `.svelte.ts` module (`state/workspace`)
 * is runes source, not plain TypeScript: without it `$state` is an undefined
 * identifier at run time. Being separate from `vite.config.ts` means this file
 * does not inherit the app's plugins, so it has to name the one it needs.
 */
export default defineConfig({
  plugins: [svelte()],
  test: {
    projects: [
      {
        extends: true,
        test: {
          name: 'node',
          include: ['src/**/*.{test,spec}.{js,ts}'],
          exclude: ['src/**/*.dom.{test,spec}.ts'],
          environment: 'node',
        },
      },
      {
        extends: true,
        test: {
          name: 'jsdom',
          include: ['src/**/*.dom.{test,spec}.ts'],
          environment: 'jsdom',
        },
      },
    ],
  },
});
