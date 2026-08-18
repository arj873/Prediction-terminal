/**
 * Turning the terminal's wire types into what lightweight-charts wants.
 *
 * The interesting cases are all about `null`. A missing volume and a missing
 * value mean different things and are drawn differently, and neither is a zero.
 */

import type {
  CandlestickData,
  HistogramData,
  LineData,
  Time,
  UTCTimestamp,
  WhitespaceData,
} from 'lightweight-charts';

import { chartTheme, type TerminalChartTheme } from './theme';

/**
 * lightweight-charts requires strictly ascending, unique timestamps and throws
 * on anything else. Upstream data is *usually* clean; this makes it always
 * clean, keeping last-wins on a duplicate bucket.
 */
export function dedupeAscending<T extends { time: Time }>(points: T[]): T[] {
  const byTime = new Map<unknown, T>();
  for (const point of points) byTime.set(point.time, point);
  return [...byTime.values()].sort((a, b) => Number(a.time) - Number(b.time));
}

export function toCandleData(
  candles: readonly { time: number; open: number; high: number; low: number; close: number }[],
): CandlestickData<Time>[] {
  return dedupeAscending(
    candles.map((c) => ({
      time: c.time as UTCTimestamp,
      open: c.open,
      high: c.high,
      low: c.low,
      close: c.close,
    })),
  );
}

/**
 * Volume bars, skipping periods with no size behind them.
 *
 * A `null` volume means the venue publishes prices without sizes, not that
 * nothing traded — so those buckets are left out rather than drawn as bars of
 * height zero. When no candle carries a volume the series is simply empty and
 * the pane stays blank, which is the truthful picture.
 */
export function toVolumeData(
  candles: readonly { time: number; volume: number | null; close: number; open: number }[],
  theme: TerminalChartTheme = chartTheme(),
): HistogramData<Time>[] {
  return dedupeAscending(
    candles
      .filter((c): c is typeof c & { volume: number } => c.volume !== null)
      .map((c) => ({
        time: c.time as UTCTimestamp,
        value: c.volume,
        color: c.close >= c.open ? `${theme.up}55` : `${theme.down}55`,
      })),
  );
}

export function toLineData(candles: readonly { time: number; close: number }[]): LineData<Time>[] {
  return dedupeAscending(candles.map((c) => ({ time: c.time as UTCTimestamp, value: c.close })));
}

/**
 * Implied-price points → chart points.
 *
 * A `null` value becomes a whitespace point rather than being dropped. That is
 * the whole reason this is not `toLineData`: a bucket where the ladder went
 * unquoted is a genuine hole in the forecast, and joining across it would draw
 * a straight line the market never implied.
 */
export function toImpliedData(
  points: readonly { time: number; value: number | null }[],
): (LineData<Time> | WhitespaceData<Time>)[] {
  return dedupeAscending(
    points.map((p) =>
      p.value === null
        ? { time: p.time as UTCTimestamp }
        : { time: p.time as UTCTimestamp, value: p.value },
    ),
  );
}

/**
 * FRED observations → chart points.
 *
 * A `null` value becomes a whitespace point, which is how lightweight-charts
 * draws a genuine gap. Dropping the row instead would silently connect across a
 * data outage and misrepresent the series.
 */
export function toFredData(
  observations: readonly { date: string; value: number | null }[],
): (LineData<Time> | WhitespaceData<Time>)[] {
  return dedupeAscending(
    observations.map((o) =>
      o.value === null ? { time: o.date as Time } : { time: o.date as Time, value: o.value },
    ),
  );
}
