/**
 * Pack the snapshot build into one self-contained HTML file.
 *
 * A static host that will not run a process — an object store, a gist viewer, a
 * published artifact — can still serve the terminal if the terminal is one
 * file. This takes what `vite.snapshot.config.ts` emits and returns a single
 * document with the script, the stylesheet and a recorded API session inlined,
 * making no request to anywhere.
 *
 * The snapshot config already bundles to one chunk and one stylesheet, so there
 * is no module graph to chase here: this is inlining, and the flattening that
 * makes inlining possible happens in the build. The two halves are meant to be
 * read together.
 *
 * Usage:
 *   node tools/pack-single-file.mjs [--build DIR] [--snapshot FILE] [--out FILE]
 */
import { readFileSync, writeFileSync, readdirSync, mkdirSync } from 'node:fs';
import { join, resolve, dirname } from 'node:path';

/* ----------------------------------------------------------------- config */

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i === -1 ? fallback : args[i + 1];
};

const BUILD = resolve(flag('--build', 'client/snapshot-build'));
const SNAPSHOT = resolve(flag('--snapshot', 'client/snapshot.json'));
const OUT = resolve(flag('--out', 'dist/prediction-terminal.html'));

/**
 * Emit body content only, for a host that supplies its own document.
 *
 * Some publishing targets wrap what they are given in their own `<!doctype>`,
 * `<head>` and `<body>` and reject a page that brings its own. The contents are
 * otherwise identical — same script, same styles, same snapshot — but the theme
 * attribute has to be set from script rather than written on `<html>`, since
 * there is no `<html>` here to write it on.
 */
const FRAGMENT = args.includes('--fragment');

/* ------------------------------------------------------------------ helpers */

/** The single emitted file with the given extension. */
function asset(ext) {
  const dir = join(BUILD, 'assets');
  const hits = readdirSync(dir).filter((f) => f.endsWith(ext));
  if (hits.length !== 1) {
    throw new Error(
      `expected exactly one ${ext} in ${dir}, found ${hits.length}` +
        ` — the snapshot build is meant to emit a single chunk (${hits.join(', ')})`,
    );
  }
  return readFileSync(join(dir, hits[0]), 'utf8');
}

/**
 * Make a string safe to sit inside a `<script>` element.
 *
 * JSON is not automatically safe there. A `</script>` inside any recorded
 * string — a market title, a headline — ends the element early and the rest of
 * the payload lands in the document as markup; `<!--` opens a comment with the
 * same effect. Escaping every `<` covers both, and leaves the value
 * byte-identical once parsed.
 */
function forScript(json) {
  // U+2028 and U+2029 are line terminators to a JavaScript parser but legal
  // unescaped inside a JSON string, so they break the payload without being
  // visible in it.
  return json.replace(
    /[<\u2028\u2029]/g,
    (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, '0')}`,
  );
}

/* -------------------------------------------------------------------- main */

const js = asset('.js');
const css = asset('.css');

let payload = '';
let meta = { recordedAt: 'never', commands: [] };
try {
  const raw = readFileSync(SNAPSHOT, 'utf8');
  meta = JSON.parse(raw).meta ?? meta;
  payload = `<script>window.__TERMINAL_SNAPSHOT__=${forScript(raw)}</script>`;
} catch {
  console.warn(`! no snapshot at ${SNAPSHOT} — the page builds, but every panel will be empty`);
}

// The theme lives on the root element, and the app writes it there on mount.
// Setting it before the app script runs is what stops a flash of the default
// palette — in a fragment build it is the only way to set it at all, since the
// root element belongs to the host document.
const primeTheme = `<script>document.documentElement.dataset.theme="amber"</script>`;

const body = `    <div id="app"></div>
${payload}
    ${primeTheme}
    <script type="module">${js}</script>`;

const doc = FRAGMENT
  ? `<title>Prediction Terminal</title>
<style>${css}</style>
${body}
`
  : `<!doctype html>
<html lang="en" data-theme="amber">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <meta name="color-scheme" content="dark" />
    <meta name="description" content="A Bloomberg-style terminal for prediction markets, economic data and the charts." />
    <title>Prediction Terminal</title>
    <style>${css}</style>
  </head>
  <body>
${body}
  </body>
</html>
`;

mkdirSync(dirname(OUT), { recursive: true });
writeFileSync(OUT, doc);

const mb = (n) => `${(n / 1_048_576).toFixed(2)} MB`;
console.log(`  script     ${mb(js.length)}`);
console.log(`  styles     ${mb(css.length)}`);
console.log(`  snapshot   ${mb(payload.length)}  (${meta.commands.length} commands, recorded ${meta.recordedAt})`);
console.log(`  total      ${mb(doc.length)}${FRAGMENT ? '  (fragment — no document scaffolding)' : ''}`);
console.log(`  wrote      ${OUT}`);
