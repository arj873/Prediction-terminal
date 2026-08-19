/**
 * The chart's palette, read from the live stylesheet.
 *
 * Nothing here is hard-coded, which is what makes `THEME amber|green|ice`
 * restyle every open chart with no per-panel bookkeeping: the CSS custom
 * properties change, the charts re-read them.
 */

import { ColorType, CrosshairMode, LineStyle } from 'lightweight-charts';
import type { DeepPartial, TimeChartOptions } from 'lightweight-charts';

function cssVar(name: string, fallback: string): string {
  if (typeof document === 'undefined') return fallback;
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

export interface TerminalChartTheme {
  background: string;
  text: string;
  dim: string;
  grid: string;
  border: string;
  up: string;
  down: string;
  accent: string;
  accentSoft: string;
}

export function chartTheme(): TerminalChartTheme {
  return {
    background: cssVar('--chart-bg', '#07070a'),
    text: cssVar('--fg', '#e8b23a'),
    dim: cssVar('--fg-dim', '#8a7440'),
    grid: cssVar('--chart-grid', '#1a1a22'),
    border: cssVar('--border', '#2a2a35'),
    up: cssVar('--up', '#33d17a'),
    down: cssVar('--down', '#f05a5a'),
    accent: cssVar('--accent', '#4aa3ff'),
    accentSoft: cssVar('--accent-soft', 'rgba(74,163,255,0.18)'),
  };
}

/**
 * Colours for implied-price overlays, in assignment order.
 *
 * Deliberately distinct from `--up`/`--down`: an overlay is a separate *series*,
 * not a direction, and colouring it green would read as "rising".
 */
export function overlayPalette(): string[] {
  return [
    cssVar('--overlay-1', '#4aa3ff'),
    cssVar('--overlay-2', '#c678dd'),
    cssVar('--overlay-3', '#e5c07b'),
    cssVar('--overlay-4', '#56b6c2'),
    cssVar('--overlay-5', '#e06c75'),
  ];
}

/**
 * Decimal places appropriate to a price's magnitude.
 *
 * The same axis has to render BTC at 63,000 and EUR/USD at 1.0842. Two decimals
 * on the second throws away the entire day's range; six on the first is noise.
 * Picking from the magnitude gets both right without a per-instrument table.
 */
export function precisionFor(magnitude: number): number {
  const abs = Math.abs(magnitude);
  if (!Number.isFinite(abs) || abs === 0) return 2;
  if (abs >= 1000) return 2;
  if (abs >= 10) return 2;
  if (abs >= 1) return 4;
  return 6;
}

export function priceFormat(precision: number) {
  return { type: 'price' as const, precision, minMove: 10 ** -precision };
}

/** Cents — how a prediction market is actually read. */
export function centsFormat() {
  return {
    type: 'custom' as const,
    minMove: 0.0001,
    formatter: (price: number): string => {
      const c = price * 100;
      return Number.isInteger(Math.round(c * 100) / 100) ? `${Math.round(c)}¢` : `${c.toFixed(1)}¢`;
    },
  };
}

export function baseOptions(theme: TerminalChartTheme): DeepPartial<TimeChartOptions> {
  return {
    layout: {
      background: { type: ColorType.Solid, color: theme.background },
      textColor: theme.dim,
      fontSize: 11,
      fontFamily: `'JetBrains Mono', 'IBM Plex Mono', 'SF Mono', Menlo, Consolas, monospace`,
      attributionLogo: false,
      panes: {
        separatorColor: theme.border,
        separatorHoverColor: theme.accent,
        enableResize: true,
      },
    },
    grid: {
      vertLines: { color: theme.grid, style: LineStyle.Solid },
      horzLines: { color: theme.grid, style: LineStyle.Solid },
    },
    rightPriceScale: {
      borderColor: theme.border,
      // Small, symmetric margins. Volume lives in its own pane rather than
      // squatting on the bottom of this one, so nothing has to be reserved — a
      // large bottom margin here is what makes a 0..1 contract's axis extend
      // into negative cents.
      scaleMargins: { top: 0.1, bottom: 0.08 },
    },
    timeScale: {
      borderColor: theme.border,
      rightOffset: 2,
      barSpacing: 6,
      minBarSpacing: 0.5,
      fixLeftEdge: true,
      lockVisibleTimeRangeOnResize: true,
    },
    crosshair: {
      mode: CrosshairMode.Normal,
      vertLine: {
        color: theme.accent,
        width: 1,
        style: LineStyle.Dashed,
        labelBackgroundColor: theme.accent,
      },
      horzLine: {
        color: theme.accent,
        width: 1,
        style: LineStyle.Dashed,
        labelBackgroundColor: theme.accent,
      },
    },
    handleScale: { axisPressedMouseMove: { time: true, price: false } },
    localization: { locale: 'en-US' },
    // Deliberately off: lightweight-charts' own ResizeObserver fires during
    // grid re-layout and fights the workspace's. The chart is resized
    // explicitly instead, which is deterministic.
    autoSize: false,
  };
}
