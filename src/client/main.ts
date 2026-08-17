/**
 * PREDICTION TERMINAL — application shell.
 *
 * Wires the four pieces together: a status header, the ticker tape, the tiled
 * workspace, and the command bar. Every user action — typed, clicked, or from a
 * key binding — funnels through `run()`, so there is exactly one dispatch path
 * to reason about.
 */

import './styles.css';

import { ApiRequestError, health } from './lib/api.js';
import { el } from './lib/dom.js';
import { clockEt, clockUtc } from './lib/format.js';
import { PanelManager } from './panels/manager.js';
import type { PanelContext } from './panels/panel.js';
import { Workspace } from './state.js';
import { CommandLine } from './terminal/commandline.js';
import { KeyRouter, Keymap, type KeyMode } from './terminal/keys.js';
import { parse } from './terminal/parser.js';
import { COMMAND_INDEX, UsageError, type CommandContext } from './terminal/registry.js';
import { TickerTape } from './terminal/tape.js';

const workspace = new Workspace();
workspace.applyTheme();

const panels = new PanelManager();
panels.setColumns(workspace.columns);

const keys = new Keymap(workspace);

/* ------------------------------------------------------------------ header */

const clockEl = el('span', { class: 'status-clock' });
const linkEl = el('span', { class: 'status-link', text: '● CONNECTING' });
const countEl = el('span', { class: 'status-item' });
const modeEl = el('span', { class: 'status-mode', text: 'CMD' });

const header = el('header', { class: 'topbar' }, [
  el('div', { class: 'brand' }, [
    el('span', { class: 'brand-mark', text: 'PT' }),
    el('span', { class: 'brand-name', text: 'PREDICTION TERMINAL' }),
  ]),
  el('div', { class: 'status' }, [
    modeEl,
    countEl,
    el('span', { class: 'status-item', text: 'KALSHI · POLYMARKET · POLYMARKET US · FRED' }),
    linkEl,
    clockEl,
  ]),
]);

function tickClock(): void {
  const now = new Date();
  clockEl.textContent = `${clockUtc(now)} UTC  ${clockEt(now)} ET`;
}
tickClock();
window.setInterval(tickClock, 1000);

function updateCounts(): void {
  const count = panels.panels.length;
  const layout = panels.zoomed ? 'zoom' : `${panels.columns} col`;
  countEl.textContent = `${count} panel${count === 1 ? '' : 's'} · ${layout}`;
}
panels.onChange(updateCounts);
updateCounts();

/* -------------------------------------------------------------- dispatch */

let commandLine: CommandLine;
let router: KeyRouter;

function log(message: string, level: 'info' | 'warn' | 'error' = 'info'): void {
  commandLine.log(message, level);
}

const panelContext: PanelContext = {
  run: (command) => run(command),
  log,
};

const commandContext: CommandContext = {
  panels,
  workspace,
  panelContext,
  keys,
  log,
  clearLog: () => commandLine.clearLog(),
  setMode: (mode) => router.setMode(mode),
  run: (command) => run(command),
};

/**
 * Execute one command line.
 *
 * Errors are reported on the message log rather than thrown: at a prompt, a
 * typo is normal input, not an exception. A `UsageError` prints the offending
 * command's usage line so the fix is immediate.
 */
function run(input: string): void {
  // A leading `>` means "type this, do not run it" — how a key binding opens a
  // command that still needs an argument, without guessing at the argument.
  if (input.startsWith('>')) {
    const line = input.slice(1).trim();
    if (line) commandLine.setValue(`${line} `);
    else commandLine.focus();
    return;
  }

  const parsed = parse(input);
  if (!parsed.verb) return;

  const command = COMMAND_INDEX.get(parsed.verb);
  if (!command) {
    log(`Unknown command "${parsed.verb}". Type HELP for the list.`, 'error');
    return;
  }

  try {
    const result = command.handler(parsed, commandContext);
    if (result instanceof Promise) {
      result.catch((err: unknown) => reportError(err, command.usage));
    }
  } catch (err) {
    reportError(err, command.usage);
  }
}

function reportError(err: unknown, usage: string): void {
  if (err instanceof UsageError) {
    log(`${err.message} · usage: ${usage}`, 'error');
    return;
  }
  if (err instanceof ApiRequestError) {
    log(err.hint ? `${err.message} — ${err.hint}` : err.message, 'error');
    return;
  }
  log(err instanceof Error ? err.message : String(err), 'error');
}

commandLine = new CommandLine(workspace, run);

/* ----------------------------------------------------------------- tape */

const tape = new TickerTape((ticker) => run(`GP ${ticker}`));

/* ----------------------------------------------------------------- mount */

const app = document.querySelector<HTMLElement>('#app');
if (!app) throw new Error('#app not found');

app.append(header, tape.root, panels.root, commandLine.root);

/* ------------------------------------------------------------- shortcuts */

/**
 * Every key press goes through the router, which turns it into a command line
 * and hands it back to `run()`. There is no second dispatch path: a shortcut
 * can only do something you could also have typed, and `KEYS` can only rebind
 * what the router already fires.
 */
router = new KeyRouter({
  keymap: keys,
  run: (command) => run(command),
  log,
  subject: () => panels.subject(),
  focusPrompt: () => commandLine.focus(),
  blurPrompt: () => commandLine.blur(),
  focusWorkspace: () => panels.focusElement(),
  onMode: (mode: KeyMode) => {
    modeEl.textContent = mode === 'nav' ? 'NAV' : 'CMD';
    modeEl.classList.toggle('is-nav', mode === 'nav');
    panels.root.classList.toggle('is-nav', mode === 'nav');
  },
});
router.attach(window);

// Esc with nothing left to clear hands the keyboard to the panels; putting it
// back in the prompt is what takes it away again.
commandLine.onEscape(() => router.setMode('nav'));
commandLine.root.addEventListener('focusin', () => router.setMode('cmd'));

/* -------------------------------------------------------------- liveness */

async function pollHealth(): Promise<void> {
  try {
    const status = await health();
    linkEl.textContent = `● LIVE${status.fredApiKey ? ' · FRED KEY' : ''}`;
    linkEl.className = 'status-link ok';
  } catch {
    linkEl.textContent = '● API DOWN';
    linkEl.className = 'status-link error';
  }
}
void pollHealth();
window.setInterval(() => void pollHealth(), 30_000);

/* --------------------------------------------------------------- opening */

commandLine.log(
  'PREDICTION TERMINAL — Kalshi · Polymarket · Polymarket US · FRED · Billboard',
  'info',
);
commandLine.log('Type HELP for commands, KEYS for the keyboard, or click an example below.', 'info');

tape.start();
commandLine.focus();

// A first-run workspace that shows what the thing does, rather than a void.
run('HELP');
if (workspace.watchlist.length > 0) run('W');
else run('TOP volume');
