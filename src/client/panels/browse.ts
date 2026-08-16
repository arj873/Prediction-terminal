/**
 * Discovery panels: SRCH (event search), EVT (strike ladder), TOP (movers) and
 * W (watchlist). Every row is clickable and routes back through the command
 * bus, so the mouse and the keyboard drive the same code path.
 */

import type { KalshiEvent, Market } from '../../shared/types.js';
import { kalshi, type SearchResponse } from '../lib/api.js';
import { cell, el, row, table } from '../lib/dom.js';
import { cents, compact, countdown, direction, signedCents, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';
import type { Workspace } from '../state.js';

/** Shared column layout for any list of markets. */
const MARKET_HEADERS = ['TICKER', 'CONTRACT', 'BID', 'ASK', 'LAST', 'CHG', 'VOL 24H', 'OI', 'CLOSES'];

function marketRow(market: Market, onOpen: (ticker: string) => void): HTMLTableRowElement {
  const tr = row([
    cell(market.ticker, 'mono strong'),
    cell(truncate(market.yesSubTitle || market.title, 46), undefined, 'td', market.title),
    cell(cents(market.yesBid), 'num price-bid'),
    cell(cents(market.yesAsk), 'num price-ask'),
    cell(cents(market.lastPrice), 'num'),
    cell(signedCents(market.change), `num ${direction(market.change)}`),
    cell(compact(market.volume24h), 'num dim'),
    cell(compact(market.openInterest), 'num dim'),
    cell(countdown(market.closeTime), 'num dim'),
  ]);
  tr.classList.add('clickable');
  tr.title = `${market.title}\nClick to chart ${market.ticker}`;
  tr.addEventListener('click', () => onOpen(market.ticker));
  return tr;
}

/* ------------------------------------------------------------------ SRCH */

export class SearchPanel extends Panel<SearchResponse> {
  override readonly kind = 'SRCH';

  readonly #query: string;

  constructor(id: string, context: PanelContext, query: string) {
    super(id, context);
    this.#query = query;
    this.refreshMs = 60_000;
  }

  static idFor(query: string): string {
    return `srch:${query.toLowerCase()}`;
  }

  protected override title(): string {
    return `"${this.#query}"`;
  }

  protected override load(signal: AbortSignal): Promise<SearchResponse> {
    return kalshi.search(this.#query, 25, signal);
  }

  protected override render(data: SearchResponse): void {
    if (data.hits.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: `Nothing open matches "${data.query}".` }),
          el('div', {
            class: 'panel-empty-hint',
            text: `Searched ${data.scanned.toLocaleString()} open events. Try fewer or broader words.`,
          }),
        ]),
      );
      return;
    }

    this.body.append(
      el('div', {
        class: 'result-note',
        text: `${data.hits.length} events of ${data.scanned.toLocaleString()} · snapshot ${data.snapshotAgeSeconds}s old`,
      }),
    );

    for (const hit of data.hits) {
      const header = el('div', { class: 'group-header' }, [
        el('span', { class: 'group-ticker', text: hit.event.eventTicker }),
        el('span', { class: 'group-title', text: truncate(hit.event.title, 72) }),
        el('span', { class: 'group-meta', text: `${hit.markets.length} contracts` }),
        el('span', { class: 'group-meta', text: `24h ${compact(hit.volume24h)}` }),
      ]);
      header.addEventListener('click', () => this.context.run(`EVT ${hit.event.eventTicker}`));
      header.title = `Open the full ladder for ${hit.event.eventTicker}`;

      // Show the liquid few inline; the ladder is one click away.
      const rows = hit.markets
        .slice(0, 4)
        .map((market) => marketRow(market, (ticker) => this.context.run(`GP ${ticker}`)));

      this.body.append(
        el('div', { class: 'group' }, [header, table(MARKET_HEADERS, rows)]),
      );
    }
  }
}

/* ------------------------------------------------------------------- EVT */

export class EventPanel extends Panel<KalshiEvent> {
  override readonly kind = 'EVT';

  readonly #eventTicker: string;
  #event: KalshiEvent | undefined;

  constructor(id: string, context: PanelContext, eventTicker: string) {
    super(id, context);
    this.#eventTicker = eventTicker;
    this.refreshMs = 10_000;
  }

  static idFor(eventTicker: string): string {
    return `evt:${eventTicker.toUpperCase()}`;
  }

  protected override title(): string {
    return this.#eventTicker;
  }

  protected override subtitle(): string {
    return this.#event ? truncate(this.#event.title, 64) : '';
  }

  protected override load(signal: AbortSignal): Promise<KalshiEvent> {
    return kalshi.event(this.#eventTicker, signal);
  }

  protected override render(event: KalshiEvent): void {
    this.#event = event;

    const total = event.markets.reduce((sum, m) => sum + m.volume24h, 0);
    // On a mutually exclusive event the YES mids should sum to ~1; when they do
    // not, the gap is the arbitrage (or the width of the spreads).
    const midSum = event.markets.reduce((sum, m) => sum + (m.mid ?? 0), 0);

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `${event.markets.length} contracts` }),
        el('span', { class: 'dim', text: ` · 24h ${compact(total)}` }),
        el('span', { class: 'dim', text: ` · ${event.category || 'uncategorised'}` }),
        event.mutuallyExclusive
          ? el('span', {
              class: Math.abs(midSum - 1) > 0.05 ? 'down' : 'dim',
              text: ` · exclusive, Σmid ${(midSum * 100).toFixed(1)}%`,
            })
          : null,
      ]),
    );

    const sorted = [...event.markets].sort((a, b) => (b.mid ?? 0) - (a.mid ?? 0));
    const rows = sorted.map((market) =>
      marketRow(market, (ticker) => this.context.run(`GP ${ticker}`)),
    );

    this.body.append(table(MARKET_HEADERS, rows));
  }
}

/* ------------------------------------------------------------------- TOP */

const SORT_LABEL: Record<string, string> = {
  volume: '24h volume',
  gainers: 'gainers',
  losers: 'losers',
  open_interest: 'open interest',
  liquidity: 'liquidity',
};

export class TopPanel extends Panel<{ sort: string; markets: Market[] }> {
  override readonly kind = 'TOP';

  readonly #sort: string;

  constructor(id: string, context: PanelContext, sort: string) {
    super(id, context);
    this.#sort = sort;
    this.refreshMs = 30_000;
  }

  static idFor(sort: string): string {
    return `top:${sort}`;
  }

  protected override title(): string {
    return this.#sort.toUpperCase();
  }

  protected override subtitle(): string {
    return `by ${SORT_LABEL[this.#sort] ?? this.#sort}`;
  }

  protected override load(signal: AbortSignal): Promise<{ sort: string; markets: Market[] }> {
    return kalshi.top(this.#sort, 30, signal);
  }

  protected override render(data: { sort: string; markets: Market[] }): void {
    if (data.markets.length === 0) {
      this.body.append(el('div', { class: 'panel-empty', text: 'No markets to rank yet.' }));
      return;
    }

    const rows = data.markets.map((market) => {
      const tr = row([
        cell(market.ticker, 'mono strong'),
        cell(truncate(market.title, 52), undefined, 'td', market.title),
        cell(cents(market.yesBid), 'num price-bid'),
        cell(cents(market.yesAsk), 'num price-ask'),
        cell(signedCents(market.change), `num ${direction(market.change)}`),
        cell(compact(market.volume24h), 'num'),
        cell(compact(market.openInterest), 'num dim'),
      ]);
      tr.classList.add('clickable');
      tr.title = `Click to chart ${market.ticker}`;
      tr.addEventListener('click', () => this.context.run(`GP ${market.ticker}`));
      return tr;
    });

    this.body.append(
      table(['TICKER', 'MARKET', 'BID', 'ASK', 'CHG', 'VOL 24H', 'OI'], rows),
    );
  }
}

/* --------------------------------------------------------------- WATCH */

export class WatchlistPanel extends Panel<Market[]> {
  override readonly kind = 'W';

  static readonly ID = 'watchlist';

  readonly #workspace: Workspace;

  constructor(id: string, context: PanelContext, workspace: Workspace) {
    super(id, context);
    this.#workspace = workspace;
    this.refreshMs = 6_000;
  }

  protected override title(): string {
    return 'WATCHLIST';
  }

  protected override async load(signal: AbortSignal): Promise<Market[]> {
    const tickers = this.#workspace.watchlist;
    if (tickers.length === 0) return [];

    // One failing ticker (settled, delisted) must not blank the whole list.
    const settled = await Promise.allSettled(
      tickers.map((ticker) => kalshi.market(ticker, signal)),
    );
    return settled
      .filter((r): r is PromiseFulfilledResult<Market> => r.status === 'fulfilled')
      .map((r) => r.value);
  }

  protected override render(markets: Market[]): void {
    if (markets.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: 'Watchlist is empty.' }),
          el('div', { class: 'panel-empty-hint', text: 'Add with `W ADD <ticker>`.' }),
        ]),
      );
      return;
    }

    const rows = markets.map((market) => {
      const remove = el('button', { class: 'row-action', type: 'button', text: '×', title: 'Remove' });
      remove.addEventListener('click', (event) => {
        event.stopPropagation();
        this.context.run(`W DEL ${market.ticker}`);
      });

      const actionCell = cell('');
      actionCell.className = 'action-cell';
      actionCell.append(remove);

      const tr = row([
        cell(market.ticker, 'mono strong'),
        cell(truncate(market.yesSubTitle || market.title, 40), undefined, 'td', market.title),
        cell(cents(market.yesBid), 'num price-bid'),
        cell(cents(market.yesAsk), 'num price-ask'),
        cell(signedCents(market.change), `num ${direction(market.change)}`),
        cell(compact(market.volume24h), 'num dim'),
        cell(countdown(market.closeTime), 'num dim'),
        actionCell,
      ]);
      tr.classList.add('clickable');
      tr.addEventListener('click', () => this.context.run(`GP ${market.ticker}`));
      return tr;
    });

    this.body.append(
      table(['TICKER', 'CONTRACT', 'BID', 'ASK', 'CHG', 'VOL 24H', 'CLOSES', ''], rows),
    );
  }
}
