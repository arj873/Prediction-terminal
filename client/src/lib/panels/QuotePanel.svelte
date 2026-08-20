<!--
  DES — the quote, the contract description, and the rules it settles by.

  Venue-agnostic, so `DES KXFEDDECISION-26OCT` and `DES pm:fed-decision-in-october`
  are the same panel reading two brokers. Only one of the three quotes an ask
  natively; the server has already converted every book into a conventional YES
  bid/ask, so this renders what it is given rather than inverting anything itself.
-->
<script lang="ts">
  import type { Market, OrderBook } from '$gen';

  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { navigable } from '../actions/navigable';
  import { venue } from '../api/client';
  import { getRowCursor, getTerminalContext } from '../context';
  import {
    EM_DASH,
    cents,
    countdown,
    direction,
    group,
    percent,
    signedCents,
    stamp,
    truncate,
  } from '../format';
  import { formatRef, venueInfo, type VenueRef } from '../terminal/venue';

  const { id, ref }: { id: string; ref: VenueRef } = $props();

  const { run } = getTerminalContext();

  interface Quote {
    market: Market;
    book: OrderBook | null;
  }

  const data = createPanelData<Quote>({
    load: async (signal) => {
      const [market, book] = await Promise.all([
        venue.market(ref, signal),
        // A venue that will not serve a book still has a quote worth printing.
        venue.orderBook(ref, 5, signal).catch(() => null),
      ]);
      return { market, book };
    },
    refreshMs: 5_000,
  });

  const subject = $derived(formatRef(ref));

  const subtitle = $derived.by(() => {
    const label = venueInfo(ref.venue).label;
    const market = data.data?.market;
    return market ? `${label} · ${truncate(market.title, 52)}` : label;
  });

  /**
   * The obvious next commands, one click away.
   *
   * Only what this venue actually serves is offered. Polymarket US publishes no
   * public price history or tape and ForecastEx publishes no book at all, so an
   * enabled GP/TAS/OB button there was a click that could only ever land on a
   * 501 — the capability is declared on the venue registry precisely so the
   * button can know before the reader does. A disabled one is still shown,
   * wearing the venue's own explanation as its tooltip: the reader learns what
   * the exchange does publish rather than that a button did nothing.
   */
  const actions = $derived.by(() => {
    const market = data.data?.market;
    if (!market) return [];

    const marketRef = formatRef({ venue: market.venue, id: market.ticker });
    const eventRef = formatRef({ venue: market.venue, id: market.eventTicker });
    const caps = venueInfo(market.venue).capabilities;

    return [
      { label: 'GP', command: `GP ${marketRef}`, disabled: caps.candles ? undefined : caps.note },
      { label: 'OB', command: `OB ${marketRef}`, disabled: caps.book ? undefined : caps.note },
      { label: 'TAS', command: `TAS ${marketRef}`, disabled: caps.trades ? undefined : caps.note },
      ...(market.eventTicker
        ? [
            { label: 'EVT', command: `EVT ${eventRef}`, disabled: undefined },
            { label: 'XV', command: `XV ${eventRef}`, disabled: undefined },
          ]
        : []),
      { label: '+WATCH', command: `W ADD ${marketRef}`, disabled: undefined },
    ];
  });
</script>

{#snippet field(label: string, value: string)}
  <div class="field">
    <span class="field-label">{label}</span>
    <span class="field-value">{value}</span>
  </div>
{/snippet}

{#snippet bigNumber(label: string, value: string, tone: string)}
  <div class="quote-cell {tone}">
    <div class="quote-label">{label}</div>
    <div class="quote-value">{value}</div>
  </div>
{/snippet}

<PanelFrame {id} kind="DES" title={subject} {subtitle} {subject} {data}>
  {@const cursor = getRowCursor()}
  {@const quote = data.data as Quote}
  {@const market = quote.market}
  {@const book = quote.book}
  <!-- The book's mid is the better one when there is a book; the catalogue's is the fallback. -->
  {@const mid = book?.mid ?? market.mid}

  <div class="quote-head">
    <div class="quote-question">
      <span class="venue-badge venue-{market.venue}">{venueInfo(market.venue).code}</span>
      <span>{market.title}</span>
    </div>
    <div class="quote-strike">
      <span class="tag tag-yes">YES</span>
      <span>{market.yesSubTitle || '—'}</span>
    </div>
  </div>

  <div class="quote-grid">
    {@render bigNumber('BID', cents(market.yesBid), 'bid')}
    {@render bigNumber('ASK', cents(market.yesAsk), 'ask')}
    {@render bigNumber('MID', cents(mid), 'mid')}
    {@render bigNumber('LAST', cents(market.lastPrice), direction(market.change))}
    {@render bigNumber('CHG', signedCents(market.change), direction(market.change))}
    {@render bigNumber('IMPL', percent(mid, 1), 'mid')}
  </div>

  <div class="meta-strip">
    {@render field('STATUS', market.status.toUpperCase())}
    {@render field('SPREAD', cents(book?.spread ?? null))}
    {@render field('VOL', group(market.volume))}
    {@render field('VOL 24H', group(market.volume24h))}
    {@render field('OPEN INT', group(market.openInterest))}
    <!-- A venue that does not publish depth prints `--`, not `$0.00`. -->
    {@render field(
      'LIQUIDITY',
      market.liquidity === null ? EM_DASH : `$${group(market.liquidity, 2)}`,
    )}
    {@render field('CLOSES', stamp(market.closeTime))}
    {@render field('IN', countdown(market.closeTime))}
    {@render field('EVENT', market.eventTicker || EM_DASH)}
    {@render field('SERIES', market.seriesTicker || EM_DASH)}
    {#if market.result}
      {@render field('RESULT', market.result.toUpperCase())}
    {/if}
  </div>

  {#if market.rulesPrimary}
    <details class="notes">
      <!-- Navigable so the keyboard can open it: the mouse can, so the caret must. -->
      <summary use:navigable={{ cursor }}>SETTLEMENT RULES</summary>
      <p>{market.rulesPrimary}</p>
    </details>
  {/if}

  <div class="panel-actions">
    {#each actions as action (action.label)}
      {#if action.disabled !== undefined}
        <!--
          Reachable by the caret but inert, as it was before: the reader can walk
          onto it and read why the venue cannot serve it, and Enter does nothing.
        -->
        <button
          class="action action-disabled"
          type="button"
          disabled
          title={action.disabled}
          use:navigable={{ cursor }}>{action.label}</button
        >
      {:else}
        <!--
          No `command` on the registration: `$` means the market this panel is
          about, and `W ADD KXFOO` would otherwise offer `ADD` as the subject.
        -->
        <button
          class="action"
          type="button"
          onclick={() => run(action.command)}
          use:navigable={{ cursor }}>{action.label}</button
        >
      {/if}
    {/each}
  </div>
</PanelFrame>
