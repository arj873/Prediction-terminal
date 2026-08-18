/**
 * The terminal is a client-side app, not a rendered document.
 *
 * The workspace — open panels, watchlist, theme, key bindings — is read from
 * `localStorage` as the app boots, and the chart library reads its palette off
 * live CSS custom properties with `getComputedStyle`. Neither exists on a
 * server, so rendering there would produce a shell that is thrown away on
 * hydration at best, and throws at worst.
 */
export const ssr = false;
export const prerender = false;
