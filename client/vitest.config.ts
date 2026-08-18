import { defineConfig } from 'vitest/config';

/**
 * Separate from `vite.config.ts` so the app build does not carry test config.
 *
 * Most of what is tested here is pure — the command parser, the chord/keymap
 * logic, the formatters — so the default environment is Node. A file that needs
 * a document opts in by naming itself `*.dom.test.ts`.
 */
export default defineConfig({
  test: {
    include: ['src/**/*.{test,spec}.{js,ts}'],
    environment: 'node',
    environmentMatchGlobs: [['src/**/*.dom.{test,spec}.ts', 'jsdom']],
  },
});
