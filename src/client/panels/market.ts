/**
 * Market panels: quote (DES), order book (OB), and time & sales (TAS).
 *
 * Venue-agnostic. Each takes a {@link VenueRef}, so `DES KXFEDDECISION-26OCT`
 * and `DES pm:fed-decision-in-october` are the same panel reading two brokers.
 */

import type { Market, OrderBook, TradesResponse } from '../../shared/types.js';
import { formatRef, venueInfo, type VenueRef } from '../../shared/venue.js';
import { venue } from '../lib/api.js';
import { cell, el, field, row, table } from '../lib/dom.js';
import {
  EM_DASH,
  cents,
  compact,
  countdown,
  direction,
  group,
  percent,
  signedCents,
  stamp,
  timeOfDay,
  truncate,
} from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';

/* ------------------------------------------------------------------- DES */

export class QuotePanel extends Panel<{ market: Market; book: OrderBook | null }> {
  override readonly kind = 'DES';

  readonly #ref: VenueRef;
  #market: Market | undefined;

  constructor(id: string, context: PanelContext, ref: VenueRef) {
    super(id, context);
    this.#ref = ref;
    this.refreshMs = 5_000;
  }

  static idFor(ref: VenueRef): string {
    return `des:${ref.venue}:${ref.id}`;
  }

  protected override title(): string {
    return formatRef(this.#ref);
  }

  protected override subtitle(): string {
    const label = venueInfo(this.#ref.venue).label;
    return this.#market ? `${label} · ${truncate(this.#market.title, 52)}` : label;
  }

  protected override async load(signal: AbortSignal): Promise<{ market: Market; book: OrderBook | null }> {
    const [market, book] = await Promise.all([
      venue.market(this.#ref, signal),
      venue.orderBook(this.#ref, 5, signal).catch(() => null),
    ]);
    return { market, book };
  }

  protected override render({ market, book }: { market: Market; book: OrderBook | null }): void {
    this.#market = market;

    const mid = book?.mid ?? market.mid;

    this.body.append(
      el('div', { class: 'quote-head' }, [
        el('div', { class: 'quote-question' }, [
          el('span', {
            class: `venue-badge venue-${market.venue}`,
            text: venueInfo(market.venue).code,
          }),
          el('span', { text: market.title }),
        ]),
        el('div', { class: 'quote-strike' }, [
          el('span', { class: 'tag tag-yes', text: 'YES' }),
          el('span', { text: market.yesSubTitle || '—' }),
        ]),
      ]),
      el('div', { class: 'quote-grid' }, [
        this.#bigNumber('BID', cents(market.yesBid), 'bid'),
        this.#bigNumber('ASK', cents(market.yesAsk), 'ask'),
        this.#bigNumber('MID', cents(mid), 'mid'),
        this.#bigNumber('LAST', cents(market.lastPrice), direction(market.change)),
        this.#bigNumber('CHG', signedCents(market.change), direction(market.change)),
        this.#bigNumber('IMPL', percent(mid, 1), 'mid'),
      ]),
      el('div', { class: 'meta-strip' }, [
        field('STATUS', market.status.toUpperCase()),
        field('SPREAD', book?.spread !== null && book?.spread !== undefined ? cents(book.spread) : '—'),
        field('VOL', group(market.volume)),
        field('VOL 24H', group(market.volume24h)),
        field('OPEN INT', group(market.openInterest)),
        // A venue that does not publish depth prints `--`, not `$0.00`.
        field('LIQUIDITY', market.liquidity === null ? EM_DASH : `$${group(market.liquidity, 2)}`),
        field('CLOSES', stamp(market.closeTime)),
        field('IN', countdown(market.closeTime)),
        field('EVENT', market.eventTicker || EM_DASH),
        field('SERIES', market.seriesTicker || EM_DASH),
        market.result ? field('RESULT', market.result.toUpperCase()) : el('span'),
      ]),
    );

    if (market.rulesPrimary) {
      this.body.append(
        el('details', { class: 'notes' }, [
          el('summary', { text: 'SETTLEMENT RULES' }),
          el('p', { text: market.rulesPrimary }),
        ]),
      );
    }

    const ref = formatRef({ venue: market.venue, id: market.ticker });
    const event = formatRef({ venue: market.venue, id: market.eventTicker });

    this.body.append(
      el('div', { class: 'panel-actions' }, [
        this.#action('GP', `GP ${ref}`),
        this.#action('OB', `OB ${ref}`),
        this.#action('TAS', `TAS ${ref}`),
        market.eventTicker ? this.#action('EVT', `EVT ${event}`) : null,
        market.eventTicker ? this.#action('XV', `XV ${event}`) : null,
        this.#action('+WATCH', `W ADD ${ref}`),
      ]),
    );
  }

  #bigNumber(label: string, value: string, tone: string): HTMLElement {
    return el('div', { class: `quote-cell ${tone}` }, [
      el('div', { class: 'quote-label', text: label }),
      el('div', { class: 'quote-value', text: value }),
    ]);
  }

  #action(label: string, command: string): HTMLElement {
    const button = el('button', { class: 'action', type: 'button', text: label });
    button.addEventListener('click', () => this.context.run(command));
    return button;
  }
}

/* -------------------------------------------------------------------- OB */

export class DepthPanel extends Panel<OrderBook> {
  override readonly kind = 'OB';

  readonly #ref: VenueRef;

  constructor(id: string, context: PanelContext, ref: VenueRef) {
    super(id, context);
    this.#ref = ref;
    this.refreshMs = 3_000;
  }

  static idFor(ref: VenueRef): string {
    return `ob:${ref.venue}:${ref.id}`;
  }

  protected override title(): string {
    return formatRef(this.#ref);
  }

  protected override subtitle(): string {
    return venueInfo(this.#ref.venue).label;
  }

  protected override load(signal: AbortSignal): Promise<OrderBook> {
    return venue.orderBook(this.#ref, 15, signal);
  }

  /**
   * Rendered as a conventional ladder: YES asks descending above the spread,
   * YES bids descending below. The NO book is what actually rests on the ask
   * side — the server already inverted it into YES terms.
   */
  protected override render(book: OrderBook): void {
    const maxSize = Math.max(
      1,
      ...book.yes.map((l) => l.size),
      ...book.yesAsks.map((l) => l.size),
    );

    const askRows = [...book.yesAsks]
      .reverse()
      .map((level) => this.#ladderRow(level.price, level.size, maxSize, 'ask'));

    const bidRows = book.yes.map((level) => this.#ladderRow(level.price, level.size, maxSize, 'bid'));

    const spreadRow = row(
      [
        cell(book.spread === null ? '—' : cents(book.spread), 'num'),
        cell('SPREAD', 'spread-label'),
        cell(book.mid === null ? '—' : cents(book.mid), 'num'),
      ],
      'spread-row',
    );

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `BID ${cents(book.bestYesBid)}` }),
        el('span', { class: 'dim', text: ' × ' }),
        el('span', { text: `ASK ${cents(book.bestYesAsk)}` }),
        el('span', { class: 'dim', text: `   ${book.yes.length + book.yesAsks.length} levels` }),
      ]),
      table(['SIZE', 'PRICE (YES)', 'DEPTH'], [...askRows, spreadRow, ...bidRows], 'ladder'),
    );
  }

  #ladderRow(price: number, size: number, maxSize: number, side: 'bid' | 'ask'): HTMLTableRowElement {
    const depthCell = cell('');
    depthCell.className = 'depth-cell';
    depthCell.append(
      el('span', {
        class: `depth-bar depth-${side}`,
        style: `width:${Math.max(2, (size / maxSize) * 100)}%`,
      }),
    );

    return row(
      [cell(compact(size), 'num'), cell(cents(price), `num price-${side}`), depthCell],
      `ladder-${side}`,
    );
  }
}

/* ------------------------------------------------------------------- TAS */

export class TradesPanel extends Panel<TradesResponse> {
  override readonly kind = 'TAS';

  readonly #ref: VenueRef;

  constructor(id: string, context: PanelContext, ref: VenueRef) {
    super(id, context);
    this.#ref = ref;
    this.refreshMs = 5_000;
  }

  static idFor(ref: VenueRef): string {
    return `tas:${ref.venue}:${ref.id}`;
  }

  protected override title(): string {
    return formatRef(this.#ref);
  }

  protected override subtitle(): string {
    return venueInfo(this.#ref.venue).label;
  }

  protected override load(signal: AbortSignal): Promise<TradesResponse> {
    return venue.trades(this.#ref, 100, signal);
  }

  protected override render(data: TradesResponse): void {
    if (data.trades.length === 0) {
      this.body.append(el('div', { class: 'panel-empty', text: 'No prints yet.' }));
      return;
    }

    const volume = data.trades.reduce((sum, t) => sum + t.count, 0);
    const notional = data.trades.reduce((sum, t) => sum + t.count * t.yesPrice, 0);

    const rows = data.trades.map((trade) => {
      // A YES taker lifted the offer — an aggressive buy of YES, which prints
      // green by the usual convention. A NO taker hit the bid: that is a sale
      // of YES, and prints red.
      const aggression = trade.takerSide === 'yes' ? 'buy' : 'sell';
      return row(
        [
          cell(timeOfDay(trade.ts), 'mono dim'),
          cell(cents(trade.yesPrice), `num trade-${aggression}`),
          cell(compact(trade.count), 'num'),
          cell(trade.takerSide.toUpperCase(), `tag-cell trade-${aggression}`),
          cell(trade.isBlockTrade ? 'BLOCK' : '', 'dim'),
        ],
        `trade-row-${aggression}`,
      );
    });

    this.body.append(
      el('div', {
        class: 'result-note',
        text: `${data.trades.length} prints · ${group(volume)} contracts · $${group(notional, 2)} notional`,
      }),
      table(['TIME (UTC)', 'PRICE', 'SIZE', 'TAKER', ''], rows, 'tas-table'),
    );
  }
}
