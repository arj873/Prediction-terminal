/**
 * NEWS — the market news wire.
 *
 * Reads like the trade tape rather than like a chart: newest at the top, one
 * line per story, the tagged symbols beside it. The headline links out to the
 * publisher, and the rest of the row drills into the price of what the story is
 * about, because "what is this doing to the stock" is the next question every
 * time.
 */

import type { NewsArticle, NewsFeed } from '../../shared/types.js';
import { news } from '../lib/api.js';
import { append, cell, el, row, table } from '../lib/dom.js';
import { EM_DASH, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';

export interface NewsPanelOptions {
  /** Symbols to filter to. Empty means the whole wire. */
  symbols: string[];
  limit: number;
  days: number;
}

const HHMM = new Intl.DateTimeFormat('en-GB', {
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
  timeZone: 'UTC',
});

const DAY_HHMM = new Intl.DateTimeFormat('en-GB', {
  day: '2-digit',
  month: 'short',
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
  timeZone: 'UTC',
});

/**
 * `14:25Z` for today, `14 Aug 09:02` for anything older.
 *
 * A wire panel is mostly today, where the clock is what matters and a date on
 * every row is noise — but the window reaches back a week, and a bare time on a
 * three-day-old story would read as three hours old.
 */
function when(unixSeconds: number): string {
  if (!Number.isFinite(unixSeconds) || unixSeconds <= 0) return EM_DASH;
  const date = new Date(unixSeconds * 1000);
  const today = new Date();
  const sameDay = date.toISOString().slice(0, 10) === today.toISOString().slice(0, 10);
  return sameDay ? `${HHMM.format(date)}Z` : DAY_HHMM.format(date).replace(',', '');
}

/**
 * The chart command for a tagged symbol.
 *
 * Crypto stories are tagged with a pair (`BTCUSD`), and `STK BTCUSD` prices
 * nothing — those go to `CRY` with the quote currency stripped. Matching the
 * pair shape rather than a list of known coins keeps an equity ticker that
 * happens to share a name with a token (`LINK`) on the equity feed.
 */
const CRYPTO_PAIR = /^([A-Z]{2,10})[-/]?(?:USD|USDT|USDC)$/;

export function chartCommand(symbol: string): string {
  const pair = CRYPTO_PAIR.exec(symbol);
  return pair ? `CRY ${pair[1]}` : `STK ${symbol}`;
}

export class NewsPanel extends Panel<NewsFeed> {
  override readonly kind = 'NEWS';

  #options: NewsPanelOptions;

  constructor(id: string, context: PanelContext, options: NewsPanelOptions) {
    super(id, context);
    this.#options = options;
    // A wire is the one feed here that is worth watching in real time.
    this.refreshMs = 60_000;
  }

  /**
   * Keyed on the symbols alone: re-issuing `NEWS NVDA 50` for an open
   * `NEWS NVDA` refreshes that panel rather than tiling a second copy of the
   * same wire beside it.
   */
  static idFor(symbols: string[]): string {
    return `news:${symbols.join(',').toLowerCase() || 'wire'}`;
  }

  /**
   * Re-issue with new arguments.
   *
   * The symbols are the panel's identity, so only the count and the window can
   * change here — which is exactly what `NEWS NVDA 30d` after `NEWS NVDA` is
   * asking for, and what the empty state tells you to type.
   */
  reconfigure(options: NewsPanelOptions): void {
    this.#options = { ...this.#options, limit: options.limit, days: options.days };
    void this.refresh();
  }

  protected override title(): string {
    const { symbols } = this.#options;
    return symbols.length > 0 ? truncate(symbols.join(' '), 30) : 'WIRE';
  }

  protected override subtitle(): string {
    const data = this.latest;
    if (!data) return '';
    return `${data.articles.length} headlines · ${data.days}d · ${data.source}`;
  }

  protected override load(signal: AbortSignal): Promise<NewsFeed> {
    return news.feed(this.#options.symbols, this.#options.limit, this.#options.days, signal);
  }

  protected override render(data: NewsFeed): void {
    if (data.articles.length === 0) {
      // An empty wire is a fact about a stated window, so the window is stated
      // and the command that widens it is spelled out.
      const scope = data.symbols.length > 0 ? data.symbols.join(' ') : 'the wire';
      const window = data.days === 1 ? 'day' : `${data.days} days`;
      const wider = `NEWS ${[...data.symbols, '30d'].join(' ')}`;

      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: `Nothing on ${scope} in the last ${window}.` }),
          el('div', {
            class: 'panel-empty-hint',
            text:
              data.symbols.length > 0
                ? `Try a wider window — \`${wider}\` — or drop the symbol filter with \`NEWS\`.`
                : `Try a wider window — \`${wider}\`.`,
          }),
        ]),
      );
      return;
    }

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', {
          text:
            data.symbols.length > 0
              ? `${data.articles.length} headlines on ${data.symbols.join(', ')}`
              : `${data.articles.length} headlines across the wire`,
        }),
        el('span', { class: 'dim', text: `  last ${data.days}d` }),
        // Say whose wire this is rather than passing it off as first-party.
        el('span', { class: 'dim', text: `  · via ${data.source}` }),
      ]),
    );

    this.body.append(
      table(
        ['TIME', 'HEADLINE', 'SYMBOLS', 'SRC'],
        data.articles.map((article) => this.#row(article)),
        'news-table',
      ),
    );
  }

  /** The headline itself — a link out when the item carries a usable one. */
  #headline(article: NewsArticle): HTMLElement | string {
    if (!article.url) return article.headline;

    const link = el('a', {
      class: 'news-link',
      href: article.url,
      target: '_blank',
      rel: 'noopener noreferrer',
      text: article.headline,
    });
    // The row drills into the price; the headline opens the story. One click
    // must not do both.
    link.addEventListener('click', (event) => event.stopPropagation());
    return link;
  }

  #row(article: NewsArticle): HTMLTableRowElement {
    const headlineCell = cell('');
    append(
      headlineCell,
      el('div', { class: 'news-headline' }, [this.#headline(article)]),
      article.summary
        ? el('div', { class: 'news-summary', text: truncate(article.summary, 150) })
        : null,
    );
    headlineCell.title = article.summary
      ? `${article.headline}\n\n${article.summary}`
      : article.headline;

    const symbols = article.symbols;
    const symbolCell = cell(
      symbols.length === 0 ? EM_DASH : truncate(symbols.slice(0, 4).join(' '), 24),
      symbols.length === 0 ? 'mono dim' : 'mono',
      'td',
      symbols.join(' '),
    );

    const tr = row([
      cell(when(article.time), 'num dim'),
      headlineCell,
      symbolCell,
      cell(article.publisher.toUpperCase() || EM_DASH, 'dim tiny'),
    ]);

    const primary = symbols[0];
    if (primary) {
      const command = chartCommand(primary);
      tr.classList.add('clickable');
      tr.title = `${article.headline}\nClick for ${command}`;
      tr.addEventListener('click', () => this.context.run(command));
    }

    return tr;
  }
}
