/**
 * The ticker tape across the top.
 *
 * Scrolls the most active contracts across every venue that ranks by volume.
 * The animation is CSS-driven over a duplicated track so it loops seamlessly,
 * and it pauses on hover so a tape entry can actually be clicked. Content is
 * only re-rendered when the ranked set changes, so a refresh does not visibly
 * restart the scroll.
 */

import { venue } from '../lib/api.js';
import { el } from '../lib/dom.js';
import { cents, direction, signedCents, truncate } from '../lib/format.js';
import type { Market } from '../../shared/types.js';
import { formatRef, venueInfo, VENUE_IDS } from '../../shared/venue.js';

const REFRESH_MS = 45_000;
const COUNT = 18;
/** Pulled per venue before merging, so no single book can fill the tape. */
const PER_VENUE = 12;

export class TickerTape {
  readonly root: HTMLElement;
  readonly #track: HTMLElement;
  readonly #onSelect: (ticker: string) => void;
  #timer: number | undefined;
  #signature = '';

  constructor(onSelect: (ticker: string) => void) {
    this.#onSelect = onSelect;
    this.#track = el('div', { class: 'tape-track' });
    this.root = el('div', { class: 'tape', role: 'marquee' }, [this.#track]);
  }

  start(): void {
    void this.#refresh();
    this.#timer = window.setInterval(() => void this.#refresh(), REFRESH_MS);
  }

  stop(): void {
    if (this.#timer !== undefined) window.clearInterval(this.#timer);
    this.#timer = undefined;
  }

  async #refresh(): Promise<void> {
    try {
      // One venue being down, or simply not publishing volume, must not empty
      // the tape — whatever answers gets scrolled.
      const settled = await Promise.allSettled(
        VENUE_IDS.map((v) => venue.top(v, 'volume', PER_VENUE)),
      );
      // Interleaved, not merged and sorted: Kalshi's contract volumes dwarf the
      // other two books, so ranking the pool would give one venue the whole
      // tape. Round-robin keeps every broker visible as it scrolls past.
      const ranked = settled
        .filter((r) => r.status === 'fulfilled')
        .map((r) => r.value.markets);

      const markets: Market[] = [];
      for (let i = 0; markets.length < COUNT; i++) {
        const before = markets.length;
        for (const list of ranked) if (list[i]) markets.push(list[i]!);
        if (markets.length === before) break;
      }
      markets.length = Math.min(markets.length, COUNT);
      if (markets.length === 0) throw new Error('no ranked markets');
      this.#render(markets);
    } catch {
      // The tape is decoration. A failure here must not raise anything at the
      // user — the panels will report a real outage far more usefully.
      if (!this.#signature) {
        this.#track.replaceChildren(
          el('span', { class: 'tape-item dim', text: 'tape unavailable' }),
        );
      }
    }
  }

  #render(markets: Market[]): void {
    const signature = markets
      .map((m) => `${m.venue}:${m.ticker}:${m.lastPrice ?? ''}:${m.yesBid ?? ''}`)
      .join('|');
    if (signature === this.#signature) return;
    this.#signature = signature;

    const items = markets.map((market) => this.#item(market));
    // Two copies back-to-back: the -50% keyframe lands exactly on the seam.
    const clones = markets.map((market) => this.#item(market, true));
    this.#track.replaceChildren(...items, ...clones);
  }

  #item(market: Market, isClone = false): HTMLElement {
    const price = market.lastPrice ?? market.mid;

    const node = el('button', { class: 'tape-item', type: 'button', ...(isClone ? { 'aria-hidden': 'true', tabindex: '-1' } : {}) }, [
      el('span', { class: `venue-badge venue-${market.venue}`, text: venueInfo(market.venue).code }),
      el('span', { class: 'tape-ticker', text: truncate(market.ticker, 26) }),
      el('span', { class: 'tape-price', text: cents(price) }),
      el('span', { class: `tape-change ${direction(market.change)}`, text: signedCents(market.change) }),
    ]);

    node.title = market.title;
    node.addEventListener('click', () =>
      this.#onSelect(formatRef({ venue: market.venue, id: market.ticker })),
    );
    return node;
  }
}
