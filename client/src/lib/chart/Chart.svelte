<!--
  The one component that knows about lightweight-charts.

  Panels hand it a list of series and it reconciles them: new ids are created,
  departed ids removed, and everything else gets `setData`. That reconciliation
  is the point. The class this replaces was destroyed and rebuilt on every poll,
  because the old panel lifecycle cleared the whole body before re-rendering —
  so a chart someone had zoomed into snapped back to the full window every few
  seconds. Here the instance outlives the data, and the reader's view survives a
  refresh.

  The chart is refitted only when `fitKey` changes, which a panel sets to
  whatever identifies "a different thing is being charted now" — a new ticker, a
  new interval. A poll is not that.
-->
<script lang="ts">
  import { createChart } from 'lightweight-charts';
  import type {
    IChartApi,
    ISeriesApi,
    MouseEventParams,
    SeriesType,
    Time,
  } from 'lightweight-charts';
  import {
    AreaSeries,
    CandlestickSeries,
    HistogramSeries,
    LineSeries,
    LineStyle,
  } from 'lightweight-charts';
  import { onDestroy, onMount } from 'svelte';

  import { getTerminalContext } from '../context';
  import {
    baseOptions,
    centsFormat,
    chartTheme,
    priceFormat,
    type TerminalChartTheme,
  } from './theme';

  export type SeriesKind =
    | 'candles'
    | 'probability-area'
    | 'volume'
    | 'line'
    | 'spot-candles'
    | 'spot-line'
    | 'implied-line'
    | 'fred-area';

  export interface SeriesSpec {
    /** Stable across refreshes — this is what keeps a series alive. */
    id: string;
    kind: SeriesKind;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    data: any[];
    /** Overlay colour, for `implied-line`. */
    color?: string;
    /** Decimal places, for the spot and implied series. */
    precision?: number;
  }

  interface Props {
    series: readonly SeriesSpec[];
    /** Refit when this changes. Identifies *what* is charted, not its data. */
    fitKey?: string | number;
    /** Height of the volume pane, when there is one. */
    volumePaneHeight?: number;
    onCrosshair?: (params: MouseEventParams<Time>) => void;
  }

  const { series, fitKey = '', volumePaneHeight = 70, onCrosshair }: Props = $props();

  const { workspace } = getTerminalContext();

  let container = $state<HTMLDivElement | undefined>(undefined);
  let chart: IChartApi | undefined;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const live = new Map<string, { spec: SeriesSpec; api: ISeriesApi<any> }>();
  let lastFitKey: string | number | undefined;
  let observer: ResizeObserver | undefined;

  /** Colour and format options for a series kind, so creation and restyle agree. */
  function optionsFor(spec: SeriesSpec, theme: TerminalChartTheme): Record<string, unknown> {
    switch (spec.kind) {
      case 'candles':
        return { ...candleColours(theme), priceFormat: centsFormat() };
      case 'spot-candles':
        return { ...candleColours(theme), priceFormat: priceFormat(spec.precision ?? 2) };
      case 'probability-area':
      case 'fred-area':
        return {
          lineColor: theme.accent,
          topColor: theme.accentSoft,
          bottomColor: 'rgba(0,0,0,0)',
          lineWidth: 2,
          ...(spec.kind === 'probability-area'
            ? { priceFormat: centsFormat() }
            : { priceLineVisible: false }),
        };
      case 'volume':
        return { priceFormat: { type: 'volume' }, color: theme.dim, priceLineVisible: false };
      case 'line':
        return { color: theme.accent, lineWidth: 2 };
      case 'spot-line':
        return { color: theme.text, lineWidth: 2, priceFormat: priceFormat(spec.precision ?? 2) };
      case 'implied-line':
        // Dashed and thinner than the price on purpose: both series share an
        // axis and the reader has to tell the forecast from what happened.
        return {
          color: spec.color ?? theme.accent,
          lineWidth: 2,
          lineStyle: LineStyle.Dashed,
          priceFormat: priceFormat(spec.precision ?? 2),
          priceLineVisible: false,
          lastValueVisible: true,
          crosshairMarkerVisible: true,
        };
    }
  }

  function candleColours(theme: TerminalChartTheme) {
    return {
      upColor: theme.up,
      downColor: theme.down,
      wickUpColor: theme.up,
      wickDownColor: theme.down,
      borderUpColor: theme.up,
      borderDownColor: theme.down,
    };
  }

  function definitionFor(kind: SeriesKind): SeriesType {
    switch (kind) {
      case 'candles':
      case 'spot-candles':
        return CandlestickSeries as unknown as SeriesType;
      case 'probability-area':
      case 'fred-area':
        return AreaSeries as unknown as SeriesType;
      case 'volume':
        return HistogramSeries as unknown as SeriesType;
      default:
        return LineSeries as unknown as SeriesType;
    }
  }

  /**
   * Volume gets its own pane rather than an overlay price scale.
   *
   * The obvious alternative — a large bottom margin on the main scale — makes
   * the price axis extend past its data, which on a 0..1 contract prints
   * negative cents.
   */
  function paneFor(kind: SeriesKind): number {
    return kind === 'volume' ? 1 : 0;
  }

  function sync() {
    if (!chart) return;
    const theme = chartTheme();
    const wanted = new Set(series.map((s) => s.id));

    for (const [id, entry] of [...live]) {
      // A kind change is a different series wearing the same name.
      if (!wanted.has(id) || entry.spec.kind !== series.find((s) => s.id === id)?.kind) {
        chart.removeSeries(entry.api);
        live.delete(id);
      }
    }

    for (const spec of series) {
      let entry = live.get(spec.id);
      if (!entry) {
        const api = chart.addSeries(
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          definitionFor(spec.kind) as any,
          optionsFor(spec, theme),
          paneFor(spec.kind),
        );
        entry = { spec, api };
        live.set(spec.id, entry);
        sizeVolumePane(spec.kind);
      } else {
        entry.spec = spec;
        entry.api.applyOptions(optionsFor(spec, theme));
      }
      entry.api.setData(spec.data);
    }

    if (fitKey !== lastFitKey) {
      lastFitKey = fitKey;
      chart.timeScale().fitContent();
    }
  }

  function sizeVolumePane(kind: SeriesKind) {
    if (kind !== 'volume' || !chart || !container) return;
    const panes = chart.panes();
    // Only size it when there is room; forcing 70px of volume onto a short tile
    // would leave nothing for the price.
    if (panes.length > 1 && container.clientHeight > volumePaneHeight * 3) {
      panes[1]?.setHeight(volumePaneHeight);
    }
  }

  function resize() {
    if (!chart || !container) return;
    const { clientWidth: width, clientHeight: height } = container;
    if (width > 0 && height > 0) chart.resize(width, height);
  }

  onMount(() => {
    if (!container) return;
    chart = createChart(container, {
      ...baseOptions(chartTheme()),
      width: Math.max(container.clientWidth, 120),
      height: Math.max(container.clientHeight, 80),
    });
    if (onCrosshair) chart.subscribeCrosshairMove(onCrosshair);

    observer = new ResizeObserver(() => resize());
    observer.observe(container);
    sync();
  });

  onDestroy(() => {
    observer?.disconnect();
    live.clear();
    chart?.remove();
    chart = undefined;
  });

  // Data, and the series list, changing.
  $effect(() => {
    void series;
    void fitKey;
    sync();
  });

  // `THEME` changing. The palette lives in CSS custom properties, so the values
  // are only readable after the class swap has landed — hence re-reading rather
  // than mapping the theme name to colours here.
  $effect(() => {
    void workspace.theme;
    if (!chart) return;
    const theme = chartTheme();
    chart.applyOptions(baseOptions(theme));
    for (const entry of live.values()) entry.api.applyOptions(optionsFor(entry.spec, theme));
  });
</script>

<div class="chart-host" bind:this={container}></div>
