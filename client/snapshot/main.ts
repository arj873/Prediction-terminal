/**
 * Boot the terminal without SvelteKit.
 *
 * A snapshot build is one HTML file with no server behind it, and SvelteKit's
 * boot is the one thing in the app that cannot survive that: it resolves route
 * nodes by building chunk URLs at runtime, so the module graph cannot be
 * bundled flat, and its router then tries to match the document's own path as a
 * route — which, for a file opened from disk or served from an object store, is
 * not a route and 404s the page before anything mounts.
 *
 * None of that is load-bearing here. The terminal has one route, no
 * server-rendered pass (`ssr = false` in `+layout.ts`), no data loaders, and no
 * import from `$app` anywhere in the source: the layout renders its children
 * and nothing else. So the snapshot build mounts the page component itself and
 * leaves the router out, which is what makes the graph statically bundleable.
 *
 * The page component is the same file the production build ships. This entry
 * changes how the app starts, never what it is.
 */
import { mount } from 'svelte';

import '../src/app.css';
import Page from '../src/routes/+page.svelte';

const target = document.getElementById('app');
if (!target) throw new Error('no #app element to mount into');

mount(Page, { target });
