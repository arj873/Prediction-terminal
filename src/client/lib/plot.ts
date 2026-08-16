/**
 * A small XY plot, in SVG.
 *
 * `lib/chart.ts` wraps lightweight-charts, which is excellent and is what every
 * price panel uses — but its x-axis is a *time* scale, and it cannot be
 * anything else. A volatility smile is implied vol against **strike**, and a
 * term structure is implied vol against **days to expiry**; forcing either onto
 * a time axis means lying to the library about what the numbers are and getting
 * date labels under a strike ladder.
 *
 * So: a deliberately small plotter for the handful of views whose x-axis is a
 * number rather than an instant. It reads its colours from the same CSS custom
 * properties as the charts, so `THEME` restyles it too, and it re-renders on
 * resize rather than scaling a viewBox, which keeps the labels crisp and the
 * line weights honest at every tile size.
 */

const NS = 'http://www.w3.org/2000/svg';

export interface PlotPoint {
  x: number;
  y: number;
  /** Shown in the readout instead of the formatted value, when present. */
  label?: string;
}

export interface PlotSeries {
  label: string;
  color: string;
  points: PlotPoint[];
  dashed?: boolean;
  /** Draw a dot at every point. Useful when the rungs are the data. */
  markers?: boolean;
}

export interface PlotMarker {
  x: number;
  label: string;
  color?: string;
}

export interface PlotOptions {
  formatX?: (value: number) => string;
  formatY?: (value: number) => string;
  /** Vertical reference lines — spot and the forward, on a smile. */
  markers?: PlotMarker[];
  /** Called with the nearest points when the pointer moves; `null` on exit. */
  onHover?: (x: number | null, values: { series: PlotSeries; point: PlotPoint }[]) => void;
}

function cssVar(name: string, fallback: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

function svgEl<K extends keyof SVGElementTagNameMap>(
  tag: K,
  attrs: Record<string, string | number> = {},
): SVGElementTagNameMap[K] {
  const node = document.createElementNS(NS, tag);
  for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, String(value));
  return node;
}

/** Round to a readable step — 1, 2, 2.5 or 5 times a power of ten. */
function niceStep(range: number, targetTicks: number): number {
  if (!Number.isFinite(range) || range <= 0) return 1;
  const rough = range / Math.max(1, targetTicks);
  const magnitude = 10 ** Math.floor(Math.log10(rough));
  const normalised = rough / magnitude;
  const step = normalised <= 1 ? 1 : normalised <= 2 ? 2 : normalised <= 2.5 ? 2.5 : normalised <= 5 ? 5 : 10;
  return step * magnitude;
}

export class Plot {
  readonly #host: HTMLElement;
  #series: PlotSeries[] = [];
  #options: PlotOptions = {};
  #svg: SVGSVGElement | undefined;
  #detach: (() => void) | undefined;

  constructor(host: HTMLElement) {
    this.#host = host;
  }

  setData(series: PlotSeries[], options: PlotOptions = {}): void {
    this.#series = series;
    this.#options = options;
    this.render();
  }

  render(): void {
    const width = this.#host.clientWidth;
    const height = this.#host.clientHeight;
    if (width <= 0 || height <= 0) return;

    this.#detach?.();
    this.#detach = undefined;

    const theme = {
      text: cssVar('--fg-dim', '#8a7440'),
      muted: cssVar('--fg-muted', '#6b5a33'),
      grid: cssVar('--chart-grid', '#1a1a22'),
      border: cssVar('--border', '#2a2a35'),
      accent: cssVar('--accent', '#4aa3ff'),
    };

    const points = this.#series.flatMap((s) => s.points);
    const svg = svgEl('svg', { width, height, viewBox: `0 0 ${width} ${height}`, class: 'plot' });

    if (points.length === 0) {
      const empty = svgEl('text', {
        x: width / 2,
        y: height / 2,
        fill: theme.muted,
        'font-size': 11,
        'text-anchor': 'middle',
      });
      empty.textContent = 'No data to plot.';
      svg.append(empty);
      this.#host.replaceChildren(svg);
      this.#svg = svg;
      return;
    }

    // Axis gutters. Wide enough for a five-digit strike and a percentage.
    const pad = { top: 10, right: 12, bottom: 20, left: 46 };
    const plotWidth = Math.max(1, width - pad.left - pad.right);
    const plotHeight = Math.max(1, height - pad.top - pad.bottom);

    const markerXs = (this.#options.markers ?? []).map((m) => m.x).filter(Number.isFinite);
    const xs = [...points.map((p) => p.x), ...markerXs];
    const ys = points.map((p) => p.y);

    let xMin = Math.min(...xs);
    let xMax = Math.max(...xs);
    let yMin = Math.min(...ys);
    let yMax = Math.max(...ys);

    // A flat series still deserves an axis it sits in the middle of.
    if (xMax - xMin < 1e-9) [xMin, xMax] = [xMin - 1, xMax + 1];
    if (yMax - yMin < 1e-9) [yMin, yMax] = [yMin - Math.abs(yMin || 1) * 0.1, yMax + Math.abs(yMax || 1) * 0.1];

    const yPad = (yMax - yMin) * 0.08;
    yMin -= yPad;
    yMax += yPad;

    const toX = (value: number): number => pad.left + ((value - xMin) / (xMax - xMin)) * plotWidth;
    const toY = (value: number): number =>
      pad.top + plotHeight - ((value - yMin) / (yMax - yMin)) * plotHeight;

    const formatX = this.#options.formatX ?? ((v: number) => String(Math.round(v)));
    const formatY = this.#options.formatY ?? ((v: number) => v.toFixed(1));

    // ---- grid + axes ------------------------------------------------------
    const yStep = niceStep(yMax - yMin, 4);
    for (let value = Math.ceil(yMin / yStep) * yStep; value <= yMax; value += yStep) {
      const y = toY(value);
      svg.append(
        svgEl('line', { x1: pad.left, y1: y, x2: width - pad.right, y2: y, stroke: theme.grid }),
      );
      const label = svgEl('text', {
        x: pad.left - 6,
        y: y + 3,
        fill: theme.muted,
        'font-size': 10,
        'text-anchor': 'end',
      });
      label.textContent = formatY(value);
      svg.append(label);
    }

    const xStep = niceStep(xMax - xMin, 5);
    for (let value = Math.ceil(xMin / xStep) * xStep; value <= xMax; value += xStep) {
      const x = toX(value);
      svg.append(
        svgEl('line', { x1: x, y1: pad.top, x2: x, y2: pad.top + plotHeight, stroke: theme.grid }),
      );
      const label = svgEl('text', {
        x,
        y: height - 6,
        fill: theme.muted,
        'font-size': 10,
        'text-anchor': 'middle',
      });
      label.textContent = formatX(value);
      svg.append(label);
    }

    svg.append(
      svgEl('line', {
        x1: pad.left,
        y1: pad.top + plotHeight,
        x2: width - pad.right,
        y2: pad.top + plotHeight,
        stroke: theme.border,
      }),
    );

    // ---- reference lines --------------------------------------------------
    for (const marker of this.#options.markers ?? []) {
      if (!Number.isFinite(marker.x)) continue;
      const x = toX(marker.x);
      svg.append(
        svgEl('line', {
          x1: x,
          y1: pad.top,
          x2: x,
          y2: pad.top + plotHeight,
          stroke: marker.color ?? theme.accent,
          'stroke-dasharray': '3 3',
          'stroke-width': 1,
        }),
      );
      const label = svgEl('text', {
        x: x + 3,
        y: pad.top + 9,
        fill: marker.color ?? theme.accent,
        'font-size': 9,
      });
      label.textContent = marker.label;
      svg.append(label);
    }

    // ---- series -----------------------------------------------------------
    for (const series of this.#series) {
      const ordered = [...series.points].sort((a, b) => a.x - b.x);
      if (ordered.length === 0) continue;

      if (ordered.length > 1) {
        svg.append(
          svgEl('polyline', {
            points: ordered.map((p) => `${toX(p.x)},${toY(p.y)}`).join(' '),
            fill: 'none',
            stroke: series.color,
            'stroke-width': 1.5,
            ...(series.dashed ? { 'stroke-dasharray': '4 3' } : {}),
          }),
        );
      }

      if (series.markers || ordered.length === 1) {
        for (const point of ordered) {
          svg.append(
            svgEl('circle', { cx: toX(point.x), cy: toY(point.y), r: 1.8, fill: series.color }),
          );
        }
      }
    }

    // ---- crosshair --------------------------------------------------------
    const cursor = svgEl('line', {
      x1: 0,
      y1: pad.top,
      x2: 0,
      y2: pad.top + plotHeight,
      stroke: theme.accent,
      'stroke-dasharray': '2 3',
      opacity: 0,
    });
    svg.append(cursor);

    const onHover = this.#options.onHover;
    if (onHover) {
      const move = (event: MouseEvent): void => {
        const rect = svg.getBoundingClientRect();
        const px = event.clientX - rect.left;
        if (px < pad.left || px > width - pad.right) {
          cursor.setAttribute('opacity', '0');
          onHover(null, []);
          return;
        }
        const value = xMin + ((px - pad.left) / plotWidth) * (xMax - xMin);

        // Snap to the nearest rung of the first series that has one, so the
        // readout names a real strike rather than an interpolated position.
        const anchor = this.#series.find((s) => s.points.length > 0);
        const nearest = anchor?.points.reduce((best, point) =>
          Math.abs(point.x - value) < Math.abs(best.x - value) ? point : best,
        );
        const snapped = nearest?.x ?? value;

        cursor.setAttribute('x1', String(toX(snapped)));
        cursor.setAttribute('x2', String(toX(snapped)));
        cursor.setAttribute('opacity', '1');

        const values = this.#series
          .map((series) => {
            const point = series.points.find((p) => p.x === snapped);
            return point ? { series, point } : null;
          })
          .filter((v): v is { series: PlotSeries; point: PlotPoint } => v !== null);

        onHover(snapped, values);
      };

      const leave = (): void => {
        cursor.setAttribute('opacity', '0');
        onHover(null, []);
      };

      svg.addEventListener('mousemove', move);
      svg.addEventListener('mouseleave', leave);
      this.#detach = () => {
        svg.removeEventListener('mousemove', move);
        svg.removeEventListener('mouseleave', leave);
      };
    }

    this.#host.replaceChildren(svg);
    this.#svg = svg;
  }

  resize(): void {
    this.render();
  }

  destroy(): void {
    this.#detach?.();
    this.#detach = undefined;
    this.#svg?.remove();
    this.#svg = undefined;
  }
}
