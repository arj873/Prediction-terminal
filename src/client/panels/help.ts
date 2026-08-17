/**
 * HELP — generated from the command registry, so it cannot drift from what the
 * terminal actually does.
 */

import { cell, el, row, table } from '../lib/dom.js';
import { Panel, type PanelContext } from './panel.js';
import { bindRow } from './table.js';
import { COMMANDS, COMMAND_INDEX, type Command } from '../terminal/registry.js';

const GROUP_TITLES: Record<Command['group'], string> = {
  markets: 'PREDICTION MARKETS',
  data: 'DATA SOURCES',
  workspace: 'WORKSPACE',
};

export class HelpPanel extends Panel<Command[]> {
  override readonly kind = 'HELP';

  readonly #topic: string | undefined;

  constructor(id: string, context: PanelContext, topic?: string) {
    super(id, context);
    this.#topic = topic;
  }

  static idFor(topic?: string): string {
    return topic ? `help:${topic.toLowerCase()}` : 'help';
  }

  protected override title(): string {
    return this.#topic ?? 'COMMANDS';
  }

  protected override load(): Promise<Command[]> {
    return Promise.resolve(COMMANDS);
  }

  protected override render(commands: Command[]): void {
    if (this.#topic) {
      const command = COMMAND_INDEX.get(this.#topic);
      if (!command) {
        this.body.append(
          el('div', { class: 'panel-empty' }, [
            el('div', { text: `No command called ${this.#topic}.` }),
            el('div', { class: 'panel-empty-hint', text: 'Run HELP for the full list.' }),
          ]),
        );
        return;
      }
      this.#renderDetail(command);
      return;
    }

    this.body.append(
      el('div', { class: 'help-intro' }, [
        el('p', {
          text:
            'Type a command and press Enter. ↑/↓ walks history, Tab completes, ' +
            'Esc clears the line. Click any table row to drill in.',
        }),
      ]),
    );

    for (const group of ['markets', 'data', 'workspace'] as const) {
      const rows = commands
        .filter((c) => c.group === group)
        .map((command) => {
          const tr = row([
            cell(command.verb, 'mono strong'),
            cell(command.usage, 'mono dim'),
            cell(command.summary),
            cell((command.aliases ?? []).join(' '), 'mono dim'),
          ]);
          bindRow(tr, `HELP ${command.verb}`, (line) => this.context.run(line));
          return tr;
        });

      this.body.append(
        el('div', { class: 'help-group' }, [
          el('h3', { class: 'help-group-title', text: GROUP_TITLES[group] }),
          table(['CMD', 'USAGE', 'DESCRIPTION', 'ALIASES'], rows),
        ]),
      );
    }

    // The key map is generated from the bindings themselves, in its own panel —
    // duplicating it here is how the two would come to disagree. What is left
    // is the handful worth knowing before you have read it.
    const keyRow = (key: string, action: string): HTMLTableRowElement =>
      row([cell(key, 'mono strong'), cell(action)]);

    this.body.append(
      el('div', { class: 'help-group' }, [
        el('h3', { class: 'help-group-title', text: 'KEYS — run KEYS for the whole map' }),
        table(
          ['KEY', 'ACTION'],
          [
            keyRow('Enter', 'Run the command'),
            keyRow('↑ / ↓', 'Previous / next command in history'),
            keyRow('Tab', 'Complete the verb'),
            keyRow('Esc', 'Clear the line — again on an empty line hands the keyboard to the panels'),
            keyRow('Alt+1…9', 'Focus that panel'),
            keyRow('Alt+↑/↓', 'Move the row cursor · Alt+Enter opens the row'),
            keyRow('Ctrl+←/→', 'Move focus between panels'),
            keyRow('Alt+F', 'Maximise the focused panel'),
            keyRow('Alt+Q', 'Close the focused panel · Ctrl+L clears the log'),
            keyRow('Alt+K', 'The key map, where all of this can be rebound'),
          ],
        ),
      ]),
      el('div', { class: 'help-group' }, [
        el('h3', { class: 'help-group-title', text: 'DATA SOURCES' }),
        el('ul', { class: 'help-list' }, [
          el('li', {
            text: 'Kalshi — public trade-api v2. Prices in cents; a contract settles at $1.',
          }),
          el('li', {
            text: 'Polymarket International — gamma-api for the catalogue, the CLOB for books and price history. Prefix a slug with pm:.',
          }),
          el('li', {
            text: 'Polymarket US — the public gateway. Quote and book only: its trade tape and candles need an API key, and the terminal holds none. Prefix a slug with pmus:.',
          }),
          el('li', {
            text: 'FRED — scraped from fred.stlouisfed.org. Set FRED_API_KEY for an API fallback if the scrape is blocked.',
          }),
          el('li', { text: 'Billboard — scraped from billboard.com/charts.' }),
          el('li', {
            text: 'Rotten Tomatoes — scraped from rottentomatoes.com. Settles Kalshi KXRT.',
          }),
          el('li', {
            text: 'Netflix — the official Top 10 dataset published at netflix.com/tudum/top10.',
          }),
          el('li', {
            text: 'Spotify / YouTube — mirrored by kworb.net; both platforms’ own charts need a login or render client-side.',
          }),
          el('li', { text: 'Box office — scraped from boxofficemojo.com daily charts.' }),
          el('li', { text: 'Steam — Valve’s public API for live players, steamcharts.com for the leaderboard.' }),
          el('li', { text: 'TV schedules — the TVmaze public API.' }),
          el('li', {
            text: 'News — Alpaca’s wire (Benzinga). The one keyed feed: set ALPACA_API_KEY_ID and ALPACA_API_SECRET_KEY.',
          }),
          el('li', {
            class: 'dim',
            text: 'Read-only market data. Nothing here places an order; the Alpaca key is used to read news and nothing else.',
          }),
        ]),
      ]),
    );
  }

  #renderDetail(command: Command): void {
    this.body.append(
      el('div', { class: 'help-detail' }, [
        el('h3', { class: 'help-group-title', text: command.verb }),
        el('p', { class: 'help-summary', text: command.summary }),
        el('div', { class: 'help-usage' }, [
          el('span', { class: 'help-label', text: 'USAGE' }),
          el('code', { text: command.usage }),
        ]),
        (command.aliases ?? []).length > 0
          ? el('div', { class: 'help-usage' }, [
              el('span', { class: 'help-label', text: 'ALIASES' }),
              el('code', { text: (command.aliases ?? []).join('  ') }),
            ])
          : null,
      ]),
    );

    if (command.examples?.length) {
      const list = el('div', { class: 'help-examples' });
      for (const example of command.examples) {
        const button = el('button', { class: 'example', type: 'button', text: example });
        button.addEventListener('click', () => this.context.run(example));
        list.append(button);
      }
      this.body.append(
        el('div', { class: 'help-detail' }, [
          el('span', { class: 'help-label', text: 'EXAMPLES (click to run)' }),
          list,
        ]),
      );
    }
  }
}
