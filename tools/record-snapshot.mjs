/**
 * Record an API session for a snapshot build.
 *
 * Drives the real client in a real browser against a running server and keeps
 * every `/api` response it makes. The point of driving the app rather than
 * hitting a hand-written list of URLs is that the app decides the URLs: panel
 * poll intervals, default depths and limits, the second request a picker makes
 * when it resolves an overlay. A list written by hand drifts from those the
 * first time a default changes, and the miss only shows up as an empty panel in
 * the built artifact.
 *
 * Usage:
 *   node tools/record-snapshot.mjs [--base URL] [--out FILE]
 *
 * Requires `playwright` and a Chromium. Neither is a dependency of the client —
 * this is a release tool, not part of the app — so both are resolved from the
 * environment: set PLAYWRIGHT_CHROMIUM to point at a browser if the default
 * lookup does not find one.
 */
import { writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);

/* ----------------------------------------------------------------- config */

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i === -1 ? fallback : args[i + 1];
};

const BASE = flag('--base', 'http://127.0.0.1:8787');
const OUT = flag('--out', 'client/snapshot.json');

/**
 * Query parameters dropped from the key.
 *
 * A chart asks for a window computed from the clock, so a request made while
 * viewing the built page never equals the one made while recording. Dropping
 * the pair lets the captured series answer for any range — the reason a
 * snapshot chart shows the window it was recorded with. `client/src/lib/api/
 * snapshot.ts` strips exactly the same pair, and the two must not drift.
 */
const VOLATILE = new Set(['start', 'end']);

/**
 * What to record.
 *
 * Deliberately not every command: `NEWS` needs Alpaca credentials, and `SEC`
 * and the FRED-backed half of `ECO` are refused outright from datacentre
 * addresses, which is where a recording is usually made. Recording those
 * captures their error responses and bakes a broken panel into the build, so
 * they are left out and the snapshot answers `snapshot_miss` for them instead.
 */
const COMMANDS = [
  // Prediction markets, across all six venues.
  'SRCH fed',
  'XV fed',
  'TOP volume',
  'TOP gainers',
  'TOP volume pm',
  'DES KXFEDDECISION-26SEP-C25',
  'OB KXFEDDECISION-26SEP-C25',
  'TAS KXFEDDECISION-26SEP-C25',
  'GP KXFEDDECISION-26SEP-C25 1h 7d',
  'EVT KXFEDDECISION-26SEP',
  // Spot and implied.
  'STK NVDA 1d 1y',
  'STK AAPL 1d 1y',
  'CRY BTC 1h 30d',
  'CRY ETH 1d 1y',
  'IMP BTC',
  // Options.
  'OPT BTC',
  'VOL BTC',
  'OPD BTC',
  'OI BTC',
  // Entertainment and the charts these markets settle against.
  'ENT film',
  'BB hot-100',
  'NFLX',
  'BO',
  'STEAM',
  'TV',
  'AWRD best picture',
  'TRND',
  'POD',
  // Reference.
  'SRC',
  'HELP',
];

/** How long to let a command's panels settle before typing the next one. */
const SETTLE_MS = 2600;
/** How long to wait at the end for late pollers to land. */
const DRAIN_MS = 5000;

/* ------------------------------------------------------------------ helpers */

function keyFor(rawUrl) {
  const u = new URL(rawUrl);
  const params = [...u.searchParams.entries()]
    .filter(([k]) => !VOLATILE.has(k))
    .sort(([a], [b]) => a.localeCompare(b));
  const qs = new URLSearchParams(params).toString();
  return u.pathname.replace(/^\/api/, '') + (qs ? `?${qs}` : '');
}

/**
 * How much data a response carries, for choosing between two that share a key.
 *
 * Stripping the window bounds means several requests collapse onto one entry —
 * a chart's first cut and the narrower one a poll makes seconds later land in
 * the same place. Last-wins picks whichever happened to be last, which is how
 * an empty candle array ends up shadowing a full year of bars. Richest-wins
 * keeps the one worth shipping.
 */
function weigh(body) {
  if (Array.isArray(body)) return body.reduce((n, v) => n + weigh(v), 0);
  if (body && typeof body === 'object') {
    return Object.values(body).reduce((n, v) => n + weigh(v), 0);
  }
  return 1;
}

function resolveChromium() {
  if (process.env.PLAYWRIGHT_CHROMIUM) return process.env.PLAYWRIGHT_CHROMIUM;
  return undefined; // let Playwright find its own
}

/* -------------------------------------------------------------------- main */

const { chromium } = require('playwright');

const responses = {};
const assets = {};
const weights = {};
const failures = new Map();

const browser = await chromium.launch({ executablePath: resolveChromium() });
const page = await browser.newPage({ viewport: { width: 1680, height: 1000 } });

page.on('response', async (res) => {
  const url = res.url();
  if (!url.includes('/api/')) return;

  const key = keyFor(url);

  // Not every API response is JSON. Billboard artwork is proxied through the
  // server so it is same-origin, which means it arrives as an <img> src rather
  // than through the fetch path, and a snapshot that skipped it would leave a
  // hundred empty squares in `BB`. Anything the API answers as an image is kept
  // as a data URI instead.
  const type = res.headers()['content-type'] ?? '';
  if (type.startsWith('image/')) {
    if (res.ok() && !(key in assets)) {
      const buf = await res.body();
      assets[key] = `data:${type.split(';')[0]};base64,${buf.toString('base64')}`;
    }
    return;
  }

  let body;
  try {
    body = await res.json();
  } catch {
    return; // static assets share the origin
  }

  if (!res.ok()) {
    failures.set(key, `${res.status()} ${body?.code ?? ''}`.trim());
    return;
  }

  const w = weigh(body);
  if (!(key in responses) || w > weights[key]) {
    responses[key] = body;
    weights[key] = w;
  }
});

console.log(`recording ${COMMANDS.length} commands against ${BASE}`);
await page.goto(BASE, { waitUntil: 'networkidle' });

/**
 * Scroll every scrollable region to the bottom and back.
 *
 * Artwork is `loading="lazy"`, so a hundred-row chart only ever requests the
 * dozen images that happen to be in view — a recording made without this
 * captures a fifth of them and the built page shows placeholders for the rest.
 * Written against any overflowing element rather than a known panel class, so
 * it keeps working as panels are added.
 */
async function revealLazyContent() {
  const boxes = await page.evaluate(() => {
    const scrollable = [...document.querySelectorAll('*')].filter(
      (el) => el.scrollHeight > el.clientHeight + 40,
    );
    scrollable.forEach((el) => (el.scrollTop = el.scrollHeight));
    return scrollable.length;
  });
  if (boxes) await page.waitForTimeout(1800);
  await page.evaluate(() => {
    [...document.querySelectorAll('*')]
      .filter((el) => el.scrollTop > 0)
      .forEach((el) => (el.scrollTop = 0));
  });
}

for (const cmd of COMMANDS) {
  const input = page.locator('input[aria-label="Command"]');
  await input.click();
  await input.fill(cmd);
  await input.press('Enter');
  await page.waitForTimeout(SETTLE_MS);
  await revealLazyContent();
  process.stdout.write(`· ${cmd}\n`);
}

await page.waitForTimeout(DRAIN_MS);
await browser.close();

const bundle = {
  meta: { recordedAt: new Date().toISOString(), commands: COMMANDS },
  responses,
  assets,
};

writeFileSync(OUT, JSON.stringify(bundle));

const empty = Object.entries(responses)
  .filter(([, v]) => Array.isArray(v?.candles) && v.candles.length === 0)
  .map(([k]) => k);

console.log(`\n  captured  ${Object.keys(responses).length} responses`);
console.log(`  assets    ${Object.keys(assets).length} images`);
console.log(`  bytes     ${JSON.stringify(bundle).length.toLocaleString()}`);
console.log(`  wrote     ${OUT}`);
if (empty.length) console.log(`  EMPTY     ${empty.join(', ')}`);
if (failures.size) {
  console.log(`  failed    ${failures.size}`);
  for (const [k, v] of failures) console.log(`            ${v}  ${k}`);
}
