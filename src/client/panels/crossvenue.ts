/**
 * XV — the same question, at every broker that lists it.
 *
 * Two panels behind one verb, because "who else lists this?" and "at what
 * price?" are the two halves of the same question:
 *
 *   XV                 the board of series more than one venue carries
 *   XV fed             …narrowed to a query
 *   XV <event-ticker>  one event, quoted side by side across venues
 *
 * Both surface the confidence and the reason behind every pairing. A quote from
 * two brokers is only worth reading if you can see why the terminal believes
 * they are quoting the same thing.
 */

import type {
  CompareResponse,
  LinkedSeries,
  LinkedSeriesResponse,
  MatchConfidence,
} from '../../shared/types.js';
import { formatRef, venueInfo, type VenueRef } from '../../shared/venue.js';
import { xv } from '../lib/api.js';
import { cell, el, row, table } from '../lib/dom.js';
import { cents, compact, countdown, signedCents, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';

/** Confidence as a chip. `linked` is stated; the rest are the matcher reading. */
function confidenceChip(confidence: MatchConfidence, reason: string): HTMLElement {
  const chip = el('span', {
    class: `match-chip match-${confidence}`,
    text: confidence.toUpperCase(),
  });
  chip.title = reason;
  return chip;
}

/* ------------------------------------------------------------- the board */

export class LinkedSeriesPanel extends Panel<LinkedSeriesResponse> {
  override readonly kind = 'XV';

  readonly #query: string;

  constructor(id: string, context: PanelContext, query: string) {
    super(id, context);
    this.#query = query;
    this.refreshMs = 120_000;
  }

  static idFor(query: string): string {
    return `xv:${query.toLowerCase()}`;
  }

  protected override title(): string {
    return this.#query ? `"${this.#query}"` : 'LINKED SERIES';
  }

  protected override subtitle(): string {
    return 'series listed by more than one broker';
  }

  protected override load(signal: AbortSignal): Promise<LinkedSeriesResponse> {
    return xv.series(this.#query, 40, signal);
  }

  protected override render(data: LinkedSeriesResponse): void {
    const scanned = Object.entries(data.scanned)
      .map(([v, n]) => `${venueInfo(v as never).code} ${n.toLocaleString()}`)
      .join(' · ');

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `${data.series.length} linked series` }),
        el('span', { class: 'dim', text: ` · scanned ${scanned}` }),
        el('span', { class: 'dim', text: ` · snapshot ${data.snapshotAgeSeconds}s old` }),
        ...data.unavailable.map((u) =>
          el('span', { class: 'down', text: ` · ${venueInfo(u.venue).label} unavailable` }),
        ),
      ]),
    );

    if (data.series.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: this.#query ? `No linked series match "${this.#query}".` : 'No linked series.' }),
          el('div', {
            class: 'panel-empty-hint',
            text: 'A series appears here only when two or more brokers list it.',
          }),
        ]),
      );
      return;
    }

    for (const series of data.series) this.body.append(this.#card(series));
  }

  #card(series: LinkedSeries): HTMLElement {
    const header = el('div', { class: 'group-header' }, [
      confidenceChip(series.confidence, series.reason),
      el('span', { class: 'group-title', text: truncate(series.title, 62) }),
      el('span', { class: 'group-meta', text: `${series.legs.length} venues` }),
    ]);
    header.title = series.reason;

    const rows = series.legs.map((leg) => {
      const ref: VenueRef = { venue: leg.venue, id: leg.sampleEvent };
      const tr = row([
        (() => {
          const td = cell('');
          td.className = 'venue-cell';
          td.title = venueInfo(leg.venue).label;
          td.append(
            el('span', {
              class: `venue-badge venue-${leg.venue}`,
              text: venueInfo(leg.venue).code,
            }),
          );
          return td;
        })(),
        cell(truncate(leg.seriesTicker, 30), 'mono strong', 'td', leg.seriesTicker),
        cell(truncate(leg.title, 40), undefined, 'td', leg.title),
        cell(String(leg.events), 'num dim'),
        cell(String(leg.markets), 'num dim'),
        cell(compact(leg.volume24h), 'num'),
        cell(countdown(leg.closeTime), 'num dim'),
      ]);
      tr.classList.add('clickable');
      tr.title = `Compare ${formatRef(ref)} across venues`;
      tr.addEventListener('click', () => this.context.run(`XV ${formatRef(ref)}`));
      return tr;
    });

    return el('div', { class: 'group' }, [
      header,
      table(['VEN', 'SERIES', 'TITLE', 'EVENTS', 'MKTS', 'VOL 24H', 'NEXT CLOSE'], rows),
    ]);
  }
}

/* ---------------------------------------------------------- the compare */

export class ComparePanel extends Panel<CompareResponse> {
  override readonly kind = 'XV';

  readonly #ref: VenueRef;
  #title = '';

  constructor(id: string, context: PanelContext, ref: VenueRef) {
    super(id, context);
    this.#ref = ref;
    this.refreshMs = 10_000;
  }

  static idFor(ref: VenueRef): string {
    return `xvcmp:${ref.venue}:${ref.id}`;
  }

  protected override title(): string {
    return formatRef(this.#ref);
  }

  protected override subtitle(): string {
    return this.#title ? truncate(this.#title, 64) : 'cross-venue quote';
  }

  protected override load(signal: AbortSignal): Promise<CompareResponse> {
    return xv.compare(this.#ref.id, this.#ref.venue, signal);
  }

  protected override render(data: CompareResponse): void {
    this.#title = data.title;

    // ---- which events are being compared, and how sure the terminal is -----
    const legs = el('div', { class: 'compare-legs' });
    for (const event of data.events) {
      const node = el('div', { class: 'compare-leg' }, [
        el('span', {
          class: `venue-badge venue-${event.venue}`,
          text: venueInfo(event.venue).code,
        }),
        el('span', { class: 'compare-leg-ticker', text: truncate(event.eventTicker, 40) }),
        confidenceChip(event.confidence, event.reason),
        el('span', { class: 'dim', text: countdown(event.closeTime) }),
      ]);
      node.title = `${event.title}\n${event.reason}`;
      node.addEventListener('click', () =>
        this.context.run(`EVT ${formatRef({ venue: event.venue, id: event.eventTicker })}`),
      );
      node.classList.add('clickable');
      legs.append(node);
    }
    this.body.append(legs);

    if (data.events.length < 2) {
      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: 'No other broker appears to list this question.' }),
          el('div', {
            class: 'panel-empty-hint',
            text: 'Run `XV` on its own for the board of series more than one venue carries.',
          }),
        ]),
      );
      return;
    }

    if (data.rows.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: 'The events match, but none of their contracts could be paired.' }),
          el('div', {
            class: 'panel-empty-hint',
            text: 'The brokers word this ladder too differently to line up rung by rung.',
          }),
        ]),
      );
    } else {
      const venues = data.events.map((e) => e.venue);
      const headers = [
        'CONTRACT',
        ...venues.flatMap((v) => [`${venueInfo(v).code} BID`, `${venueInfo(v).code} ASK`]),
        'DIVERGE',
        'EDGE',
      ];

      const rows = data.rows.map((r) => {
        const cells = [cell(truncate(r.label, 30), 'strong', 'td', r.label)];

        for (const v of venues) {
          const leg = r.legs.find((l) => l.venue === v);
          cells.push(cell(cents(leg?.yesBid ?? null), 'num price-bid'));
          cells.push(cell(cents(leg?.yesAsk ?? null), 'num price-ask'));
        }

        cells.push(cell(r.divergence === null ? '--' : cents(r.divergence), 'num'));

        // A positive edge is a crossed book between brokers; anything else is
        // the spread, and colouring it would invite reading a cost as a profit.
        const edgeCell = cell(
          r.edge === null ? '--' : signedCents(r.edge),
          `num ${r.edge !== null && r.edge > 0 ? 'up strong' : 'dim'}`,
        );
        if (r.edge !== null && r.edge > 0 && r.edgeVenues.length === 2) {
          edgeCell.title =
            `Buy at ${venueInfo(r.edgeVenues[0]!).label}, sell at ${venueInfo(r.edgeVenues[1]!).label}. ` +
            `Before fees, and both legs have to fill.`;
        }
        cells.push(edgeCell);

        return row(cells);
      });

      this.body.append(table(headers, rows));
    }

    if (data.unmatched.length > 0) {
      this.body.append(
        el('details', { class: 'notes' }, [
          el('summary', { text: `${data.unmatched.length} CONTRACTS WITHOUT A COUNTERPART` }),
          el('p', {
            text: data.unmatched
              .map((u) => `${venueInfo(u.venue).code} ${u.label}`)
              .join(' · '),
          }),
        ]),
      );
    }
  }
}
