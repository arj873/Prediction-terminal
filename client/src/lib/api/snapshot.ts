/**
 * Offline replay of a recorded API session.
 *
 * The terminal is normally two halves — this client and the Rust server that
 * reaches the venues for it. A snapshot build drops the second half: every
 * response the app made during a recording is frozen into the page, and
 * {@link resolve} answers from that table instead of the network. It is what
 * makes a single self-contained HTML file of the terminal possible, for a
 * static host that cannot run the server.
 *
 * Nothing here is reachable in a normal build. `client.ts` consults the table
 * only when {@link snapshotAvailable} finds one on the page, and the recorder
 * is the only thing that puts one there, so an ordinary `npm run build` keeps
 * the plain `fetch` path with no branch taken and no snapshot shipped.
 *
 * The keys are request paths with two accommodations, both matching
 * `tools/record-snapshot.mjs`:
 *
 *   *Window bounds are dropped.* A chart asks for `start`/`end` computed from
 *   the clock, so a request made while viewing never equals one made while
 *   recording. Stripping the pair lets the captured series answer for the
 *   range, which is why a snapshot chart shows the window it was recorded
 *   with rather than the one that was asked for.
 *
 *   *Remaining parameters are sorted.* Two calls that differ only in argument
 *   order are one entry rather than two.
 */

/** Query parameters excluded from the key. See the module note. */
const VOLATILE = new Set(['start', 'end']);

export interface SnapshotMeta {
  /** When the recording was taken, ISO 8601. */
  recordedAt: string;
  /** The commands that were run to produce it. */
  commands: readonly string[];
}

interface SnapshotBundle {
  meta: SnapshotMeta;
  responses: Record<string, unknown>;
  /** Same keys, for the API responses that are images. Values are data URIs. */
  assets?: Record<string, string>;
}

function bundle(): SnapshotBundle | undefined {
  return (globalThis as { __TERMINAL_SNAPSHOT__?: SnapshotBundle }).__TERMINAL_SNAPSHOT__;
}

/** Whether this page was built as a snapshot. Decides the whole branch. */
export function snapshotAvailable(): boolean {
  return bundle() !== undefined;
}

/** What the recording was, for the panels that say so on screen. */
export function snapshotMeta(): SnapshotMeta | undefined {
  return bundle()?.meta;
}

/**
 * Normalise an API path to its table key.
 *
 * Kept identical to the recorder's `keyFor`, which is the only reason a
 * request made now can find a response captured then.
 */
export function snapshotKey(path: string): string {
  const [pathname, rawQuery] = path.split('?', 2);
  if (!rawQuery) return pathname;

  const params = [...new URLSearchParams(rawQuery).entries()]
    .filter(([k]) => !VOLATILE.has(k))
    .sort(([a], [b]) => a.localeCompare(b));

  const qs = new URLSearchParams(params).toString();
  return qs ? `${pathname}?${qs}` : pathname;
}

/**
 * The recorded data URI for an API path that answers with an image.
 *
 * Artwork reaches the page as an `<img src>` rather than through `fetch`, so it
 * never passes the request path and has to be swapped at the point the URL is
 * built. A miss returns undefined, which the caller renders as the same empty
 * placeholder a failed load already produces.
 */
export function asset(path: string): string | undefined {
  return bundle()?.assets?.[snapshotKey(path)];
}

export type SnapshotHit =
  | { readonly found: true; readonly body: unknown }
  | { readonly found: false; readonly key: string };

/**
 * Look one path up in the recorded table.
 *
 * A miss is returned rather than thrown so the caller can raise the same
 * {@link import('./client').ApiRequestError} shape the network path raises —
 * the panels already know how to show one, and a snapshot miss should read as
 * "this build didn't record that" rather than as a broken request.
 */
export function resolve(path: string): SnapshotHit {
  const table = bundle()?.responses;
  const key = snapshotKey(path);
  if (table && Object.prototype.hasOwnProperty.call(table, key)) {
    return { found: true, body: table[key] };
  }
  return { found: false, key };
}
