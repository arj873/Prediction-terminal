/**
 * Discovery panels: SRCH (event search), EVT (strike ladder), TOP (movers) and
 * W (watchlist). Every row is clickable and routes back through the command
 * bus, so the mouse and the keyboard drive the same code path.
 *
 * `SRCH` and `TOP` read every venue at once and label each row with the broker
 * quoting it, because the first question about a market is usually "who else
 * lists this, and at what?".
 */

import type { Market, Venue, VenueEvent } from '../../shared/types.js';
import {
  formatRef,
  parseRef,
  supportsSort,
  venueInfo,
  VENUE_IDS,
  type MoverSort,
  type VenueRef,
} from '../../shared/venue.js';
import { venue, type SearchResponse } from '../lib/api.js';
import { cell, el, row, table } from '../lib/dom.js';
import { cents, compact, countdown, direction, signedCents, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';
import type { Workspace } from '../state.js';

/** Shared column layout for any list of markets. */
const MARKET_HEADERS = ['VEN', 'TICKER', 'CONTRACT', 'BID', 'ASK', 'LAST', 'CHG', 'VOL 24H', 'OI', 'CLOSES'];

/** The venue's short code as a coloured chip, for a table cell. */
function venueCell(v: Venue): HTMLTableCellElement {
  const td = cell('');
  td.className = 'venue-cell';
  td.title = venueInfo(v).label;
  td.append(el('span', { class: `venue-badge venue-${v}`, text: venueInfo(v).code }));
  return td;
}

function marketRow(market: Market, onOpen: (ref: string) => void): HTMLTableRowElement {
  const ref = formatRef({ venue: market.venue, id: market.ticker });
  const tr = row([
    venueCell(market.venue),
    cell(truncate(market.ticker, 28), 'mono strong', 'td', market.ticker),
    cell(truncate(market.yesSubTitle || market.title, 42), undefined, 'td', market.title),
    cell(cents(market.yesBid), 'num price-bid'),
    cell(cents(market.yesAsk), 'num price-ask'),
    cell(cents(market.lastPrice), 'num'),
    cell(signedCents(market.change), `num ${direction(market.change)}`),
    cell(compact(market.volume24h), 'num dim'),
    cell(compact(market.openInterest), 'num dim'),
    cell(countdown(market.closeTime), 'num dim'),
  ]);
  tr.classList.add('clickable');
  tr.title = `${market.title}\nClick to chart ${ref}`;
  tr.addEventListener('click', () => onOpen(ref));
  return tr;
}

/* ------------------------------------------------------------------ SRCH */

interface MultiSearch {
  query: string;
  results: { venue: Venue; response: SearchResponse | null; error: string | null }[];
}

export class SearchPanel extends Panel<MultiSearch> {
  override readonly kind = 'SRCH';

  readonly #query: string;
  readonly #venues: readonly Venue[];

  constructor(id: string, context: PanelContext, query: string, venues: readonly Venue[] = VENUE_IDS) {
    super(id, context);
    this.#query = query;
    this.#venues = venues;
    this.refreshMs = 60_000;
  }

  static idFor(query: string, venues: readonly Venue[] = VENUE_IDS): string {
    return `srch:${venues.join('+')}:${query.toLowerCase()}`;
  }

  protected override title(): string {
    return `"${this.#query}"`;
  }

  protected override subtitle(): string {
    return this.#venues.length === VENUE_IDS.length
      ? 'all venues'
      : this.#venues.map((v) => venueInfo(v).label).join(' · ');
  }

  protected override async load(signal: AbortSignal): Promise<MultiSearch> {
    // One venue being down should cost that venue's rows, not the search.
    const settled = await Promise.allSettled(
      this.#venues.map((v) => venue.search(v, this.#query, 12, signal)),
    );

    return {
      query: this.#query,
      results: this.#venues.map((v, i) => {
        const result = settled[i];
        return result?.status === 'fulfilled'
          ? { venue: v, response: result.value, error: null }
          : {
              venue: v,
              response: null,
              error: result?.reason instanceof Error ? result.reason.message : 'unavailable',
            };
      }),
    };
  }

  protected override render(data: MultiSearch): void {
    const hits = data.results.flatMap((r) =>
      (r.response?.hits ?? []).map((hit) => ({ venue: r.venue, hit })),
    );

    const scanned = data.results.reduce((sum, r) => sum + (r.response?.scanned ?? 0), 0);
    const failures = data.results.filter((r) => r.error !== null);

    if (hits.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: `Nothing open matches "${data.query}".` }),
          el('div', {
            class: 'panel-empty-hint',
            text: `Searched ${scanned.toLocaleString()} open events across ${data.results.length} venues. Try fewer or broader words.`,
          }),
        ]),
      );
      return;
    }

    // Interleaved by score, so the best answer is at the top whoever lists it.
    hits.sort((a, b) => b.hit.score - a.hit.score || (b.hit.volume24h ?? 0) - (a.hit.volume24h ?? 0));

    const counts = data.results
      .filter((r) => r.response)
      .map((r) => `${venueInfo(r.venue).code} ${r.response!.hits.length}`)
      .join(' · ');

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `${hits.length} events · ${counts}` }),
        el('span', {
          class: 'dim',
          text: ` · ${scanned.toLocaleString()} scanned`,
        }),
        ...failures.map((f) =>
          el('span', { class: 'down', text: ` · ${venueInfo(f.venue).code} unavailable` }),
        ),
      ]),
    );

    for (const { venue: v, hit } of hits.slice(0, 24)) {
      const eventRef = formatRef({ venue: v, id: hit.event.eventTicker });

      const header = el('div', { class: 'group-header' }, [
        el('span', { class: `venue-badge venue-${v}`, text: venueInfo(v).code }),
        el('span', { class: 'group-ticker', text: truncate(hit.event.eventTicker, 34) }),
        el('span', { class: 'group-title', text: truncate(hit.event.title, 62) }),
        el('span', { class: 'group-meta', text: `${hit.markets.length} contracts` }),
        el('span', { class: 'group-meta', text: `24h ${compact(hit.volume24h)}` }),
      ]);
      header.addEventListener('click', () => this.context.run(`EVT ${eventRef}`));
      header.title = `Open the full ladder for ${eventRef}`;

      // Show the liquid few inline; the ladder is one click away.
      const rows = hit.markets
        .slice(0, 4)
        .map((market) => marketRow(market, (ref) => this.context.run(`GP ${ref}`)));

      this.body.append(el('div', { class: 'group' }, [header, table(MARKET_HEADERS, rows)]));
    }
  }
}

/* ------------------------------------------------------------------- EVT */

export class EventPanel extends Panel<VenueEvent> {
  override readonly kind = 'EVT';

  readonly #ref: VenueRef;

  constructor(id: string, context: PanelContext, ref: VenueRef) {
    super(id, context);
    this.#ref = ref;
    this.refreshMs = 10_000;
  }

  static idFor(ref: VenueRef): string {
    return `evt:${ref.venue}:${ref.id}`;
  }

  protected override title(): string {
    return formatRef(this.#ref);
  }

  protected override subtitle(): string {
    const label = venueInfo(this.#ref.venue).label;
    return this.latest ? `${label} · ${truncate(this.latest.title, 52)}` : label;
  }

  protected override load(signal: AbortSignal): Promise<VenueEvent> {
    return venue.event(this.#ref, signal);
  }

  protected override render(event: VenueEvent): void {
    const total = event.markets.reduce((sum, m) => sum + (m.volume24h ?? 0), 0);
    // On a mutually exclusive event the YES mids should sum to ~1; when they do
    // not, the gap is the arbitrage (or the width of the spreads).
    const midSum = event.markets.reduce((sum, m) => sum + (m.mid ?? 0), 0);

    const compareButton = el('button', { class: 'action', type: 'button', text: 'XV COMPARE' });
    compareButton.addEventListener('click', () => this.context.run(`XV ${formatRef(this.#ref)}`));

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
    const rows = sorted.map((market) => marketRow(market, (ref) => this.context.run(`GP ${ref}`)));

    this.body.append(
      table(MARKET_HEADERS, rows),
      el('div', { class: 'panel-actions' }, [compareButton]),
    );
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

interface TopData {
  sort: string;
  markets: Market[];
  /** Venues whose API does not publish this figure at all. */
  cannotRank: Venue[];
  /** Venues that do publish it but did not answer. */
  unavailable: Venue[];
}

export class TopPanel extends Panel<TopData> {
  override readonly kind = 'TOP';

  readonly #sort: string;
  readonly #venues: readonly Venue[];

  constructor(id: string, context: PanelContext, sort: string, venues: readonly Venue[] = VENUE_IDS) {
    super(id, context);
    this.#sort = sort;
    this.#venues = venues;
    this.refreshMs = 30_000;
  }

  static idFor(sort: string, venues: readonly Venue[] = VENUE_IDS): string {
    return `top:${venues.join('+')}:${sort}`;
  }

  protected override title(): string {
    return this.#sort.toUpperCase();
  }

  protected override subtitle(): string {
    const scope =
      this.#venues.length === VENUE_IDS.length
        ? 'all venues'
        : this.#venues.map((v) => venueInfo(v).label).join(' · ');
    return `by ${SORT_LABEL[this.#sort] ?? this.#sort} · ${scope}`;
  }

  protected override async load(signal: AbortSignal): Promise<TopData> {
    // A venue that does not publish this figure is not asked for it. Inferring
    // that from an empty result made an outage indistinguishable from a fact
    // about the venue's API — and the panel said the wrong one out loud.
    const ranked = this.#venues.filter((v) => supportsSort(v, this.#sort as MoverSort));
    const cannotRank = this.#venues.filter((v) => !ranked.includes(v));

    const settled = await Promise.allSettled(
      ranked.map((v) => venue.top(v, this.#sort, 30, signal)),
    );

    const markets: Market[] = [];
    const unavailable: Venue[] = [];

    settled.forEach((result, i) => {
      const v = ranked[i]!;
      if (result.status !== 'fulfilled') unavailable.push(v);
      else markets.push(...result.value.markets);
    });

    const field = (m: Market): number =>
      this.#sort === 'volume'
        ? (m.volume24h ?? 0)
        : this.#sort === 'open_interest'
          ? (m.openInterest ?? 0)
          : this.#sort === 'liquidity'
            ? (m.liquidity ?? 0)
            : (m.change ?? 0);

    markets.sort((a, b) => (this.#sort === 'losers' ? field(a) - field(b) : field(b) - field(a)));

    return { sort: this.#sort, markets: markets.slice(0, 30), cannotRank, unavailable };
  }

  protected override render(data: TopData): void {
    if (data.markets.length === 0) {
      const why =
        data.cannotRank.length === this.#venues.length
          ? `No venue in this scope publishes ${SORT_LABEL[data.sort] ?? data.sort}.`
          : 'No markets to rank yet.';
      this.body.append(el('div', { class: 'panel-empty', text: why }));
      return;
    }

    // Two different absences, and a trader needs to tell them apart: a venue
    // that publishes no volume cannot appear on a volume board however healthy
    // it is, whereas a venue that does publish it and did not answer is an
    // outage. Reporting the second as the first was actively misleading.
    const label = SORT_LABEL[data.sort] ?? data.sort;
    if (data.cannotRank.length > 0) {
      this.body.append(
        el('div', {
          class: 'result-note dim',
          text: `${data.cannotRank.map((v) => venueInfo(v).label).join(', ')} publishes no ${label} and is not ranked here.`,
        }),
      );
    }
    if (data.unavailable.length > 0) {
      this.body.append(
        el('div', {
          class: 'result-note down',
          text: `${data.unavailable.map((v) => venueInfo(v).label).join(', ')} did not answer — these rankings are incomplete.`,
        }),
      );
    }

    const rows = data.markets.map((market) => {
      const ref = formatRef({ venue: market.venue, id: market.ticker });
      const tr = row([
        venueCell(market.venue),
        cell(truncate(market.ticker, 26), 'mono strong', 'td', market.ticker),
        cell(truncate(market.title, 46), undefined, 'td', market.title),
        cell(cents(market.yesBid), 'num price-bid'),
        cell(cents(market.yesAsk), 'num price-ask'),
        cell(signedCents(market.change), `num ${direction(market.change)}`),
        cell(compact(market.volume24h), 'num'),
        cell(compact(market.openInterest), 'num dim'),
      ]);
      tr.classList.add('clickable');
      tr.title = `Click to chart ${ref}`;
      tr.addEventListener('click', () => this.context.run(`GP ${ref}`));
      return tr;
    });

    this.body.append(
      table(['VEN', 'TICKER', 'MARKET', 'BID', 'ASK', 'CHG', 'VOL 24H', 'OI'], rows),
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
    const entries = this.#workspace.watchlist;
    if (entries.length === 0) return [];

    // One failing entry (settled, delisted, mistyped) must not blank the list.
    const settled = await Promise.allSettled(
      entries.map((entry) => venue.market(parseRef(entry), signal)),
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
          el('div', {
            class: 'panel-empty-hint',
            text: 'Add with `W ADD <ticker>` — prefix `pm:` or `pmus:` for a Polymarket slug.',
          }),
        ]),
      );
      return;
    }

    const rows = markets.map((market) => {
      const ref = formatRef({ venue: market.venue, id: market.ticker });
      const remove = el('button', { class: 'row-action', type: 'button', text: '×', title: 'Remove' });
      remove.addEventListener('click', (event) => {
        event.stopPropagation();
        this.context.run(`W DEL ${ref}`);
      });

      const actionCell = cell('');
      actionCell.className = 'action-cell';
      actionCell.append(remove);

      const tr = row([
        venueCell(market.venue),
        cell(truncate(market.ticker, 26), 'mono strong', 'td', market.ticker),
        cell(truncate(market.yesSubTitle || market.title, 36), undefined, 'td', market.title),
        cell(cents(market.yesBid), 'num price-bid'),
        cell(cents(market.yesAsk), 'num price-ask'),
        cell(signedCents(market.change), `num ${direction(market.change)}`),
        cell(compact(market.volume24h), 'num dim'),
        cell(countdown(market.closeTime), 'num dim'),
        actionCell,
      ]);
      tr.classList.add('clickable');
      tr.addEventListener('click', () => this.context.run(`GP ${ref}`));
      return tr;
    });

    this.body.append(
      table(['VEN', 'TICKER', 'CONTRACT', 'BID', 'ASK', 'CHG', 'VOL 24H', 'CLOSES', ''], rows),
    );
  }
}
