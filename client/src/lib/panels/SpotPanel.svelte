<!--
  STK / CRY / IMP — the underlying price chart, with implied-price overlays.

  The true price is drawn as candles or a line; on top of it go dashed lines,
  one per selected Kalshi ladder, showing what that ladder implied the price
  would be — computed at every historical bucket, not just now. Reading the two
  together is the point of the panel: the gap between them is the market's
  forward basis, and watching it close as expiry approaches is the whole story a
  prediction market tells about a price.

  Which ladders are drawn is the reader's choice. The picker under the chart
  lists every Kalshi expiry that prices this symbol with its live implied value,
  and clicking one toggles its overlay — through the workspace's own record of
  this panel, which is the same list the `IMP <symbol> <event>…` command adds to,
  so mouse and keyboard agree and neither can go stale against the other.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import type {
    ImpliedCandidate,
    ImpliedSeriesResponse,
    SpotCandle,
    SpotCandlesResponse,
    SpotQuote,
  } from '$gen';
  import type { MouseEventParams, Time } from 'lightweight-charts';

  import EmptyState from './EmptyState.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { navigable } from '../actions/navigable';
  import { ApiRequestError, implied, spot, type CandleInterval } from '../api/client';
  import Chart, { type SeriesSpec } from '../chart/Chart.svelte';
  import { toCandleData, toImpliedData, toLineData, toVolumeData } from '../chart/data';
  import { overlayPalette, precisionFor } from '../chart/theme';
  import { getRowCursor, getTerminalContext } from '../context';
  import {
    compact,
    direction,
    price as priceText,
    signedPercent,
    signedPrice,
    stamp,
    truncate,
  } from '../format';
  import type { SpotProps } from '../terminal/registry';

  const {
    id,
    symbol,
    assetClass,
    interval,
    lookbackSeconds,
    style,
    overlays,
    method,
    showPicker,
  }: { id: string } & SpotProps = $props();

  interface SpotData {
    quote: SpotQuote | null;
    candles: SpotCandlesResponse;
    candidates: ImpliedCandidate[];
    /** Why no ladders are offered, when that is the interesting answer. */
    candidatesNote: string | null;
    overlays: ImpliedSeriesResponse[];
    /** Overlays that were requested but failed, so the picker can say so. */
    failed: { eventTicker: string; message: string }[];
  }

  const INTERVAL_LABEL: Record<CandleInterval, string> = { 1: '1m', 60: '1h', 1440: '1d' };

  const { panels, workspace } = getTerminalContext();

  const data = createPanelData<SpotData>({
    load: async (signal) => {
      const end = Math.floor(Date.now() / 1000);
      const start = end - lookbackSeconds;

      // The chart is the panel; a failed quote or an unmapped symbol must not
      // take it down, so only the candles are allowed to reject.
      const [candles, quote, candidates] = await Promise.all([
        spot.candles(assetClass, symbol, interval, start, end, signal),
        spot.quote(assetClass, symbol, signal).catch(() => null),
        implied
          .candidates(symbol, signal)
          .then((response) => ({
            candidates: response.candidates,
            note: response.note ?? null,
          }))
          .catch((err: unknown) => {
            if ((err as Error)?.name === 'AbortError') throw err;
            return {
              candidates: [] as ImpliedCandidate[],
              note: err instanceof ApiRequestError ? (err.hint ?? err.message) : String(err),
            };
          }),
      ]);

      // One request per overlay, settled independently: a ladder that fails is
      // one missing line and a note in the picker, not a blank chart.
      const settled = await Promise.all(
        overlays.map((eventTicker) =>
          implied
            .series(eventTicker, interval, start, end, method, signal)
            .then((series) => ({ ok: true as const, series }))
            .catch((err: unknown) => {
              if ((err as Error)?.name === 'AbortError') throw err;
              return {
                ok: false as const,
                eventTicker,
                message: err instanceof Error ? err.message : String(err),
              };
            }),
        ),
      );

      return {
        quote,
        candles,
        candidates: candidates.candidates,
        candidatesNote: candidates.note,
        overlays: settled.filter((s) => s.ok).map((s) => s.series),
        failed: settled
          .filter((s) => !s.ok)
          .map((s) => ({ eventTicker: s.eventTicker, message: s.message })),
      };
    },
    refreshMs: () => (interval === 1 ? 15_000 : 60_000),
  });

  const loaded = $derived(data.data);
  const bars = $derived<readonly SpotCandle[]>(loaded?.candles.candles ?? []);
  /** Overlays that came back, in the order they were asked for. */
  const drawnOverlays = $derived(loaded?.overlays ?? []);

  /** Derived, not stored: `CRY BTC` then `STK BTC` re-labels the same panel. */
  const kind = $derived(assetClass === 'crypto' ? 'CRY' : 'STK');

  const subtitle = $derived.by(() => {
    const parts = [`${INTERVAL_LABEL[interval]} · ${style}`];
    const name = loaded?.quote?.name;
    if (name && name !== symbol) parts.push(truncate(name, 44));
    if (overlays.length > 0) parts.push(`${overlays.length} implied · ${method}`);
    return parts.join(' · ');
  });

  /* -------------------------------------------------------------- overlays */

  /**
   * An overlay's colour, keyed off its place in the *requested* list.
   *
   * The chip, the line and the legend entry all read it from here, so the three
   * agree even when a ladder in between them failed to load — the chip's colour
   * is the only thing tying a dashed line on the chart to a contract ticker,
   * and it has to survive its neighbours.
   */
  function colourFor(eventTicker: string): string {
    const palette = overlayPalette();
    const index = overlays.indexOf(eventTicker);
    return palette[(index >= 0 ? index : 0) % palette.length]!;
  }

  /**
   * Toggle a ladder on or off.
   *
   * The workspace's description of this panel owns the list, not the component:
   * `IMP` merges into the same array, so a chip clicked here and a ticker typed
   * there cannot disagree, and re-opening the panel keeps what was ticked.
   */
  function toggleOverlay(eventTicker: string): void {
    const desc = panels.find(id);
    if (!desc) return;

    const current = (desc.props['overlays'] as string[] | undefined) ?? [];
    desc.props = {
      ...desc.props,
      overlays: current.includes(eventTicker)
        ? current.filter((ticker) => ticker !== eventTicker)
        : [...current, eventTicker],
    };
    data.refresh();
  }

  /* ---------------------------------------------------------------- series */

  const precision = $derived(precisionFor(bars.at(-1)?.close ?? 1));

  const series = $derived.by((): SeriesSpec[] => {
    // Volume bars carry their up/down tint in the data, so a `THEME` change has
    // to re-shape them; the chart's own restyle can only reach series options.
    void workspace.theme;

    return [
      // Stable ids: the price series survives a poll, and with it the reader's
      // zoom. A style switch changes the series *kind* under the same id, which
      // the chart reads as a different series wearing the same name; an overlay
      // is keyed by its ticker, so unticking one leaves the others untouched.
      {
        id: 'price',
        kind: style === 'candle' ? 'spot-candles' : 'spot-line',
        data: style === 'candle' ? toCandleData(bars) : toLineData(bars),
        precision,
      },
      { id: 'volume', kind: 'volume', data: toVolumeData(bars) },
      ...drawnOverlays.map((overlay) => ({
        id: `implied:${overlay.eventTicker}`,
        kind: 'implied-line' as const,
        data: toImpliedData(overlay.points),
        color: colourFor(overlay.eventTicker),
        precision,
      })),
    ];
  });

  /**
   * What is being charted, as opposed to what the data currently says.
   *
   * A ladder that was not on the chart before counts: it can bring in bars well
   * outside the current view — a contract that opened yesterday against a year
   * of price history, or the reverse — so the chart is refitted to show what was
   * just asked for. Removing one only takes data away, and there the reader's
   * place is worth keeping, so `drawn` is never pruned.
   *
   * It is applied only once a load has landed. Bumping it the moment the props
   * change would refit the chart to the series still on screen and leave the new
   * ones off to one side.
   */
  const drawn = new Set<string>();
  let added = 0;

  function identity(): string {
    // The first call seeds `drawn`, so the overlays the panel opened with are
    // counted once and the fit that draws them is the initial one.
    for (const ticker of overlays) {
      if (!drawn.has(ticker)) {
        drawn.add(ticker);
        added += 1;
      }
    }
    return `${symbol}|${assetClass}|${interval}|${lookbackSeconds}|${style}|${added}`;
  }

  let fitKey = $state(identity());

  $effect(() => {
    void data.data;
    fitKey = untrack(identity);
  });

  /* ---------------------------------------------------------------- legend */

  /**
   * The bar under the crosshair, or nothing when the cursor is off the pane.
   *
   * Only moved to a time the price series actually has a bar for — over a gap,
   * or out past the end of the price history where only a ladder is drawn, the
   * previous reading stands rather than blanking.
   */
  let cursorTime = $state<number | undefined>(undefined);

  function onCrosshair(param: MouseEventParams<Time>): void {
    if (!param.time || param.point === undefined) {
      cursorTime = undefined;
      return;
    }
    const time = Number(param.time);
    if (bars.some((bar) => bar.time === time)) cursorTime = time;
  }

  const bar = $derived(
    (cursorTime === undefined ? undefined : bars.find((b) => b.time === cursorTime)) ?? bars.at(-1),
  );

  /**
   * Each overlay's value at the cursor, for the legend.
   *
   * Empty while the cursor is off the pane: the legend then falls back to the
   * last bar, and an implied value from some other instant next to it would be
   * a comparison nobody asked for.
   */
  const impliedAtCursor = $derived.by(
    (): { eventTicker: string; value: number; colour: string }[] => {
      if (cursorTime === undefined) return [];
      return drawnOverlays.flatMap((overlay) => {
        const value = overlay.points.find((p) => p.time === cursorTime)?.value;
        if (value === undefined || value === null) return [];
        return [
          { eventTicker: overlay.eventTicker, value, colour: colourFor(overlay.eventTicker) },
        ];
      });
    },
  );

  /** `2026-08-16 14:25` on intraday bars, `2026-08-16` on daily ones. */
  function legendTime(time: number): string {
    const at = new Date(time * 1000).toISOString();
    return interval === 1440 ? at.slice(0, 10) : at.slice(0, 16).replace('T', ' ');
  }

  /* ----------------------------------------------------------------- stats */

  const stats = $derived.by(() => {
    const quote = loaded?.quote;
    const first = bars[0];
    const last = bars.at(-1);

    const lastPrice = quote?.price ?? last?.close ?? null;
    const windowChange = first && last ? last.close - first.open : null;
    const windowPercent =
      first && last && first.open !== 0 ? ((last.close - first.open) / first.open) * 100 : null;

    // Every price-shaped figure is formatted against `lastPrice`, so a change,
    // a basis and the price itself all carry the same number of decimals.
    const scale = lastPrice ?? undefined;

    const items = [
      { label: 'LAST', value: priceText(lastPrice), tone: '' },
      {
        label: 'CHG',
        value: signedPrice(quote?.change ?? null, scale),
        tone: direction(quote?.change ?? null),
      },
      {
        label: '%',
        value: signedPercent(quote?.changePercent ?? null),
        tone: direction(quote?.changePercent ?? null),
      },
      { label: 'OPEN', value: priceText(quote?.dayOpen ?? null, scale), tone: '' },
      { label: 'HIGH', value: priceText(quote?.dayHigh ?? null, scale), tone: '' },
      { label: 'LOW', value: priceText(quote?.dayLow ?? null, scale), tone: '' },
      { label: 'VOL', value: compact(quote?.volume ?? null), tone: '' },
      { label: 'WIN', value: signedPrice(windowChange, scale), tone: direction(windowChange) },
      { label: 'WIN%', value: signedPercent(windowPercent), tone: direction(windowPercent) },
      { label: 'BARS', value: String(bars.length), tone: '' },
    ];

    // One entry per overlay: what it implies now, and the basis against spot.
    // The basis is the number the whole panel exists to show.
    for (const overlay of drawnOverlays) {
      const value = lastValueOf(overlay);
      const basis = value !== null && lastPrice !== null ? value - lastPrice : null;
      items.push({
        label: `IMP ${shortEventLabel(overlay.eventTicker)}`,
        value: value === null ? '--' : `${priceText(value, scale)} (${signedPrice(basis, scale)})`,
        tone: direction(basis),
      });
    }

    // Which provider answered. The bars and the quote come off separate
    // fallback chains — Yahoo can serve the history while Nasdaq served the
    // print — so the quote's source is named when it differs rather than
    // folded into one label that would speak for both.
    const source = loaded?.candles.source;
    if (source) items.push({ label: 'SRC', value: source, tone: 'dim' });
    const quoteSource = quote?.source;
    if (quoteSource && quoteSource !== source) {
      items.push({ label: 'QUOTE', value: quoteSource, tone: 'dim' });
    }

    return items;
  });

  /* ---------------------------------------------------------------- picker */

  /**
   * The picker earns its row when there is something to pick, or when the
   * reader asked for implied prices by name — `IMP AAPL` deserves an answer
   * about why there is no overlay, while a plain `STK AAPL` does not need a line
   * of chrome telling it so. It also stays while anything is selected: a
   * transient failure to list candidates must not strand a drawn overlay with no
   * chip left to untick it.
   */
  const wantsPicker = $derived(
    (loaded?.candidates.length ?? 0) > 0 || overlays.length > 0 || showPicker,
  );

  /**
   * The chips to offer: every candidate, plus any selected ladder the candidate
   * list has stopped mentioning.
   *
   * A ladder can drop off the list while its overlay is drawn — it expires, or a
   * discovery call fails transiently. Rendering only the current candidates
   * would leave that line on the chart with no chip left to turn it off, so a
   * stub is synthesised from what the loaded series already told us.
   */
  const offered = $derived.by((): ImpliedCandidate[] => {
    const rows = [...(loaded?.candidates ?? [])];
    const known = new Set(rows.map((c) => c.eventTicker));

    for (const eventTicker of overlays) {
      if (known.has(eventTicker)) continue;
      const series = drawnOverlays.find((o) => o.eventTicker === eventTicker);
      rows.push({
        eventTicker,
        seriesTicker: '',
        title: series?.title ?? eventTicker,
        subTitle: '',
        strikeDate: series?.strikeDate ?? '',
        strikes: series?.contributors.length ?? 0,
        quoted: series?.contributors.length ?? 0,
        volume24h: 0,
        strikeLow: null,
        strikeHigh: null,
        implied: series ? lastValueOf(series) : null,
        tailMass: null,
      });
    }

    return rows;
  });

  /**
   * A ladder that loaded cleanly but has no history yet is the confusing case:
   * nothing is drawn and nothing is wrong. It happens constantly with same-day
   * expiries, which open hours before they settle and so have no completed
   * hourly bucket at all. Say so, and name the interval that would have data.
   */
  const emptyOverlayNote = $derived.by(() => {
    const empty = drawnOverlays.filter((overlay) => lastValueOf(overlay) === null);
    if (empty.length === 0) return null;
    const finer = interval === 1440 ? '1h' : '1m';
    return (
      `${empty.map((o) => shortEventLabel(o.eventTicker)).join(', ')}: ` +
      `no ${INTERVAL_LABEL[interval]} history yet — try ${finer}`
    );
  });

  /* --------------------------------------------------------------- helpers */

  /** The last bucket where the ladder actually produced a price. */
  function lastValueOf(series: ImpliedSeriesResponse): number | null {
    for (let i = series.points.length - 1; i >= 0; i--) {
      const value = series.points[i]!.value;
      if (value !== null) return value;
    }
    return null;
  }

  /**
   * `KXBTCD-26AUG1617` → `BTCD·26AUG1617`.
   *
   * The expiry alone is not enough. Kalshi lists two ladders over the same
   * underlying and the same instant — an above/below ladder (`KXBTCD`) and a
   * mutually-exclusive range ladder (`KXBTC`) — and they imply slightly
   * different prices. Labelling both `26AUG1617` would put two different lines
   * on the chart under one name. The `KX` prefix carries no information and is
   * dropped.
   */
  function shortEventLabel(eventTicker: string): string {
    const dash = eventTicker.indexOf('-');
    if (dash === -1) return eventTicker;
    const series = eventTicker.slice(0, dash).replace(/^KX/, '');
    return `${series}·${eventTicker.slice(dash + 1)}`;
  }

  function failureFor(eventTicker: string): { eventTicker: string; message: string } | undefined {
    return loaded?.failed.find((f) => f.eventTicker === eventTicker);
  }

  /**
   * The chip's tooltip: what this expiry is, and what it currently says.
   *
   * Every candidate is one Kalshi expiry with its live implied price, so the
   * choice between four expiries is made on what they actually say rather than
   * on their tickers.
   */
  function chipTooltip(candidate: ImpliedCandidate, failure?: string): string {
    const lines = [
      candidate.title,
      `Event    ${candidate.eventTicker}`,
      `Settles  ${candidate.strikeDate ? stamp(candidate.strikeDate) : 'unknown'}`,
      `Strikes  ${candidate.quoted} quoted of ${candidate.strikes}` +
        (candidate.strikeLow !== null && candidate.strikeHigh !== null
          ? ` (${priceText(candidate.strikeLow)} – ${priceText(candidate.strikeHigh)})`
          : ''),
      `Implied  ${
        candidate.implied === null
          ? 'not derivable from the current book'
          : priceText(candidate.implied)
      }`,
      `Tail     ${
        candidate.tailMass === null
          ? '--'
          : `${(candidate.tailMass * 100).toFixed(1)}% of probability outside the strikes`
      }`,
      `24h vol  ${compact(candidate.volume24h)}`,
    ];
    if (failure) lines.push('', `Overlay failed: ${failure}`);
    return lines.join('\n');
  }
</script>

<PanelFrame {id} {kind} title={symbol} {subtitle} subject={symbol} {data}>
  <!--
    The row cursor belongs to the frame, which is a descendant of this
    component — so it is read here, inside the frame's own content, rather than
    from this component's script where the context does not yet exist.
  -->
  {@const cursor = getRowCursor()}

  <div class="chart-wrap">
    {#if bar}
      <!--
        Whitespace between these spans is not rendered: `.chart-legend` and
        `.legend-pair` are flex containers, so whitespace-only text between
        items produces no box.
      -->
      <div class="chart-legend">
        <span class="legend-time">{legendTime(bar.time)}Z</span>
        <span class="legend-pair">
          <span class="legend-key">O</span>
          <span>{priceText(bar.open, bar.close)}</span>
        </span>
        <span class="legend-pair">
          <span class="legend-key">H</span>
          <span>{priceText(bar.high, bar.close)}</span>
        </span>
        <span class="legend-pair">
          <span class="legend-key">L</span>
          <span>{priceText(bar.low, bar.close)}</span>
        </span>
        <span class="legend-pair">
          <span class="legend-key">C</span>
          <span class={direction(bar.close - bar.open)}>{priceText(bar.close, bar.close)}</span>
        </span>
        <span class="legend-pair">
          <span class="legend-key">V</span>
          <span>{compact(bar.volume)}</span>
        </span>
        <!--
          Each overlay's value at the cursor and its basis against the bar's
          close, in the same colour as its line so the pairing is unambiguous.
        -->
        {#each impliedAtCursor as entry (entry.eventTicker)}
          <span class="legend-pair implied" style="--chip-color: {entry.colour}">
            <span class="legend-key">{shortEventLabel(entry.eventTicker)}</span>
            <span>{priceText(entry.value, bar.close)}</span>
            <span class="legend-basis {direction(entry.value - bar.close)}">
              ({signedPrice(entry.value - bar.close, bar.close)})
            </span>
          </span>
        {/each}
      </div>
    {/if}

    {#if bars.length === 0}
      <div class="chart-host">
        <EmptyState
          message="No price history in this window."
          hint={interval === 1
            ? 'Intraday history is short-lived upstream. Try `1h` or `1d`.'
            : 'Try a wider range, e.g. `1d 1y`.'}
        />
      </div>
    {:else}
      <Chart {series} {fitKey} {onCrosshair} volumePaneHeight={70} />
    {/if}
  </div>

  {#if wantsPicker}
    <div class="implied-picker">
      <span class="picker-label">IMPLIED</span>

      {#if offered.length === 0}
        <span class="picker-empty">
          {loaded?.candidatesNote ?? `No Kalshi ladder prices ${symbol}.`}
        </span>
      {:else}
        <div class="picker-chips">
          {#each offered as candidate (candidate.eventTicker)}
            {@const selected = overlays.includes(candidate.eventTicker)}
            {@const failure = failureFor(candidate.eventTicker)}
            <button
              class="picker-chip"
              class:on={selected}
              class:failed={failure !== undefined}
              type="button"
              title={chipTooltip(candidate, failure?.message)}
              style={selected ? `--chip-color: ${colourFor(candidate.eventTicker)}` : undefined}
              onclick={() => toggleOverlay(candidate.eventTicker)}
              use:navigable={{ cursor }}
            >
              <span class="chip-swatch"></span>
              <span class="chip-ticker">{shortEventLabel(candidate.eventTicker)}</span>
              <span class="chip-implied">
                {candidate.implied === null ? '--' : priceText(candidate.implied)}
              </span>
            </button>
          {/each}
        </div>

        {#if (loaded?.failed.length ?? 0) > 0}
          {@const count = loaded?.failed.length ?? 0}
          <span class="picker-error">
            {count} overlay{count === 1 ? '' : 's'} failed to load
          </span>
        {/if}

        {#if emptyOverlayNote}
          <span class="picker-note">{emptyOverlayNote}</span>
        {/if}
      {/if}
    </div>
  {/if}

  <div class="chart-stats">
    {#each stats as stat, i (i)}
      <span class="stat">
        <span class="stat-label">{stat.label}</span>
        <span class="stat-value {stat.tone}">{stat.value}</span>
      </span>
    {/each}
  </div>
</PanelFrame>
