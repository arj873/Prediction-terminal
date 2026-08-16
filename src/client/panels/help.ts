/**
 * HELP — generated from the command registry, so it cannot drift from what the
 * terminal actually does.
 */

import { cell, el, row, table } from '../lib/dom.js';
import { Panel, type PanelContext } from './panel.js';
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
          tr.classList.add('clickable');
          tr.title = `HELP ${command.verb}`;
          tr.addEventListener('click', () => this.context.run(`HELP ${command.verb}`));
          return tr;
        });

      this.body.append(
        el('div', { class: 'help-group' }, [
          el('h3', { class: 'help-group-title', text: GROUP_TITLES[group] }),
          table(['CMD', 'USAGE', 'DESCRIPTION', 'ALIASES'], rows),
        ]),
      );
    }

    this.body.append(
      el('div', { class: 'help-group' }, [
        el('h3', { class: 'help-group-title', text: 'KEYS' }),
        table(
          ['KEY', 'ACTION'],
          [
            row([cell('Enter', 'mono strong'), cell('Run the command')]),
            row([cell('↑ / ↓', 'mono strong'), cell('Previous / next command in history')]),
            row([cell('Tab', 'mono strong'), cell('Complete the verb')]),
            row([cell('Esc', 'mono strong'), cell('Clear the command line')]),
            row([cell('Ctrl+←/→', 'mono strong'), cell('Move focus between panels')]),
            row([cell('Ctrl+W', 'mono strong'), cell('Close the focused panel')]),
            row([cell('Ctrl+L', 'mono strong'), cell('Clear the message log')]),
            row([cell('/', 'mono strong'), cell('Focus the command line from anywhere')]),
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
