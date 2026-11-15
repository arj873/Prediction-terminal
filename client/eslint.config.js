import js from '@eslint/js';
import prettier from 'eslint-config-prettier';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import ts from 'typescript-eslint';

export default ts.config(
  js.configs.recommended,
  ...ts.configs.recommended,
  ...svelte.configs['flat/recommended'],
  prettier,
  ...svelte.configs['flat/prettier'],
  {
    languageOptions: {
      globals: { ...globals.browser, ...globals.node },
    },
    rules: {
      // TypeScript resolves identifiers itself, and does it correctly: the base
      // rule cannot see type-only names and reports every generic parameter as
      // undefined. Leaving it on would mean writing `R` twice for eslint's
      // benefit in every generic component.
      'no-undef': 'off',
      // The terminal is a single screen with no URL routing: every `<a href>` is
      // an outbound link opened in a new tab (a news story, a filing, a source),
      // never an internal SvelteKit route. `resolve()` is only for internal
      // paths, so this rule has nothing valid to catch here.
      'svelte/no-navigation-without-resolve': 'off',
    },
  },
  {
    // The Svelte parser needs the TypeScript parser for `<script lang="ts">` in
    // `.svelte` files, and — since eslint-plugin-svelte 3 lints them too — for
    // the runes modules in `.svelte.ts` / `.svelte.js`.
    files: ['**/*.svelte', '**/*.svelte.ts', '**/*.svelte.js'],
    languageOptions: {
      parserOptions: { parser: ts.parser },
    },
  },
  {
    ignores: [
      'build/',
      '.svelte-kit/',
      'node_modules/',
      // Generated from the Rust wire structs; `make gen-types` owns it.
      'src/lib/api/gen/',
    ],
  },
);
