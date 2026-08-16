/**
 * OPT / OPD / VOL / OI — the option board.
 *
 * Four views of one chain, because an option is four questions at once and no
 * single table answers them all:
 *
 *   * `OPT` — the chain. What is quoted, at what, and what is its risk.
 *   * `OPD` — one contract, in full, with its price history where a venue
 *     publishes any.
 *   * `VOL` — the volatility smile and the term structure. What the board
 *     thinks about the *distribution* rather than about any one strike.
 *   * `OI`  — open interest and volume by strike, and where the pain sits.
 *
 * They share an expiry: every panel carries the same strip of chips, and
 * picking one re-loads that panel alone. That mirrors how `IMP` handles its
 * ladders — the choice is the reader's, made on numbers the chip already shows.
 *
 * Every Greek on screen is computed by this terminal, not taken from a vendor.
 * The forward is fitted from put-call parity where the venue does not publish
 * one, and the panels say which — a Greek is only as good as its carry, and the
 * difference between a quoted forward and an assumed one is exactly the sort of
 * thing that should never be invisible. See `shared/greeks.ts`.
 */

import type {
  OptionChain,
  OptionContract,
  OptionExpiry,
  OptionPositioning,
  OptionQuoteResponse,
  OptionSurface,
} from '../../shared/types.js';
import { options as optionsApi } from '../lib/api.js';
import {
  TerminalChart,
  chartTheme,
  overlayPalette,
  precisionFor,
  toCandleData,
  toVolumeData,
} from '../lib/chart.js';
import { Plot, type PlotSeries } from '../lib/plot.js';
import { append, cell, el, field, row, table } from '../lib/dom.js';
import {
  compact,
  countdown,
  day,
  direction,
  greek,
  group,
  price as priceText,
  signedPercent,
  signedPrice,
  truncate,
  volPercent,
} from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';

/* ------------------------------------------------------------------ shared */

/**
 * The expiry strip.
 *
 * Each chip carries its days-to-expiry and its open interest, so choosing
 * between eight Fridays is a decision made on liquidity rather than on dates.
 */
function renderExpiryPicker(
  host: HTMLElement,
  expiries: OptionExpiry[],
  selected: number,
  onPick: (expiry: OptionExpiry) => void,
): void {
  host.replaceChildren();
  host.append(el('span', { class: 'picker-label', text: 'EXPIRY' }));

  if (expiries.length === 0) {
    host.append(el('span', { class: 'picker-empty', text: 'No expiries listed.' }));
    return;
  }

  const chips = el('div', { class: 'picker-chips' });

  for (const expiry of expiries) {
    const on = expiry.expiry === selected;
    const chip = el('button', {
      class: `picker-chip${on ? ' on' : ''}`,
      type: 'button',
      title:
        `${expiry.date}\n` +
        `Expires  ${countdown(new Date(expiry.expiry * 1000).toISOString())}\n` +
        `Strikes  ${expiry.contracts}\n` +
        `OI       ${group(expiry.openInterest)}\n` +
        `Volume   ${group(expiry.volume)}`,
    });
    if (on) chip.style.setProperty('--chip-color', 'var(--accent)');

    append(
      chip,
      el('span', { class: 'chip-swatch' }),
      el('span', { class: 'chip-ticker', text: expiry.date.slice(5) }),
      el('span', {
        class: 'chip-implied',
        text: expiry.daysToExpiry <= 0 ? '0d' : `${expiry.daysToExpiry}d`,
      }),
    );

    chip.addEventListener('click', () => onPick(expiry));
    chips.append(chip);
  }

  host.append(chips);
}

/** The carry line, which is where a Greek's trustworthiness is decided. */
function forwardField(chain: {
  forward: number | null;
  forwardSource: string;
  spot: number | null;
}): HTMLElement {
  const label =
    chain.forwardSource === 'venue'
      ? 'venue'
      : chain.forwardSource === 'parity'
        ? 'parity'
        : 'assumed';
  return field(
    'FWD',
    `${priceText(chain.forward, chain.spot ?? undefined)} (${label})`,
    chain.forwardSource === 'assumed' ? 'warn' : '',
  );
}

/** Row tone for an in-the-money leg, so the board reads at a glance. */
function moneyClass(contract: OptionContract | undefined): string {
  return contract?.inTheMoney ? 'itm' : '';
}

function contractOf(contract: OptionContract | undefined): string {
  return contract?.contract ?? '';
}

/* ---------------------------------------------------------------- OPT chain */

export type ChainView = 'quotes' | 'greeks';
export type ChainSide = 'both' | 'calls' | 'puts';

export interface ChainPanelOptions {
  symbol: string;
  /** `YYYY-MM-DD`, a horizon like `30d`, or undefined for the front expiry. */
  expiry?: string;
  view: ChainView;
  side: ChainSide;
  /** Strikes either side of the money. Keeps a 400-rung crypto board readable. */
  strikes: number;
}

export class OptionChainPanel extends Panel<OptionChain> {
  override readonly kind = 'OPT';

  #options: ChainPanelOptions;
  #latest: OptionChain | undefined;

  constructor(id: string, context: PanelContext, options: ChainPanelOptions) {
    super(id, context);
    this.#options = options;
    this.refreshMs = 20_000;
  }

  static idFor(symbol: string): string {
    return `opt:${symbol.toUpperCase()}`;
  }

  protected override title(): string {
    return this.#options.symbol;
  }

  protected override subtitle(): string {
    const chain = this.#latest;
    if (!chain) return '';
    const parts = [chain.expiry.date, `${chain.expiry.daysToExpiry}d`];
    if (chain.atmIv !== null) parts.push(`ATM ${volPercent(chain.atmIv)}`);
    if (this.#options.view === 'greeks') parts.push('greeks');
    return parts.join(' · ');
  }

  protected override load(signal: AbortSignal): Promise<OptionChain> {
    return optionsApi.chain(this.#options.symbol, this.#options.expiry, signal);
  }

  protected override render(chain: OptionChain): void {
    this.#latest = chain;

    const headline = el('div', { class: 'headline' }, [
      el('span', { class: 'headline-value', text: priceText(chain.spot) }),
      el('span', { class: 'headline-unit', text: chain.currency }),
      el('span', { class: 'headline-date', text: truncate(chain.name, 40) }),
    ]);

    const meta = el('div', { class: 'meta-strip' }, [
      field('EXP', day(chain.expiry.date)),
      field('DTE', `${chain.expiry.daysToExpiry}d`),
      forwardField(chain),
      field('ATM IV', volPercent(chain.atmIv)),
      field('RATE', chain.rate === null ? '--' : `${(chain.rate * 100).toFixed(2)}%`),
      field('CARRY', chain.carry === null ? '--' : `${(chain.carry * 100).toFixed(2)}%`),
      field('MULT', `×${chain.contractSize}`),
      field('SRC', `${chain.venue} · ${chain.source}`),
    ]);

    const picker = el('div', { class: 'implied-picker' });
    renderExpiryPicker(picker, chain.expiries, chain.expiry.expiry, (expiry) => {
      this.#options = { ...this.#options, expiry: expiry.date };
      void this.refresh();
    });

    this.body.append(headline, meta, picker);

    if (chain.note) {
      this.body.append(el('div', { class: 'result-note', text: chain.note }));
    }

    const rows = this.#buildRows(chain);
    if (rows.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: 'No contracts quoted for this expiry.' }),
          el('div', {
            class: 'panel-empty-hint',
            text: 'Pick a different expiry from the strip above.',
          }),
        ]),
      );
      return;
    }

    this.body.append(table(this.#headers(), rows, 'option-chain'));
  }

  #headers(): string[] {
    const { view, side } = this.#options;
    const callSide =
      view === 'greeks'
        ? ['THETA', 'VEGA', 'GAMMA', 'DELTA', 'IV', 'MID']
        : ['DELTA', 'IV', 'OI', 'VOL', 'BID', 'ASK', 'LAST'];
    const putSide =
      view === 'greeks'
        ? ['MID', 'IV', 'DELTA', 'GAMMA', 'VEGA', 'THETA']
        : ['LAST', 'BID', 'ASK', 'VOL', 'OI', 'IV', 'DELTA'];

    if (side === 'calls') return [...callSide, 'STRIKE'];
    if (side === 'puts') return ['STRIKE', ...putSide];
    return [...callSide, 'STRIKE', ...putSide];
  }

  /**
   * One row per strike, both legs on it.
   *
   * A strike where only one leg is listed still gets a row — an incomplete rung
   * is a real feature of a board, and dropping it would silently renumber the
   * ladder the reader is scanning.
   */
  #buildRows(chain: OptionChain): HTMLTableRowElement[] {
    const byStrike = new Map<number, { call?: OptionContract; put?: OptionContract }>();
    for (const call of chain.calls) {
      byStrike.set(call.strike, { ...byStrike.get(call.strike), call });
    }
    for (const put of chain.puts) {
      byStrike.set(put.strike, { ...byStrike.get(put.strike), put });
    }

    const anchor = chain.forward ?? chain.spot;
    let strikes = [...byStrike.keys()].sort((a, b) => a - b);

    // A Deribit board runs to hundreds of rungs and an SPX board to thousands.
    // Windowing around the money keeps the panel readable; `OPT X … 999` opens
    // it back up for anyone who wants the wings.
    if (anchor !== null && strikes.length > this.#options.strikes * 2) {
      const centre = strikes.reduce((best, strike) =>
        Math.abs(strike - anchor) < Math.abs(best - anchor) ? strike : best,
      );
      const index = strikes.indexOf(centre);
      strikes = strikes.slice(
        Math.max(0, index - this.#options.strikes),
        index + this.#options.strikes + 1,
      );
    }

    const scale = anchor ?? undefined;
    const { view, side } = this.#options;

    return strikes.map((strike) => {
      const legs = byStrike.get(strike)!;
      const cells: HTMLTableCellElement[] = [];

      if (side !== 'puts') cells.push(...this.#legCells(legs.call, 'call', view, scale));

      const atm =
        anchor !== null &&
        strikes.every((other) => Math.abs(other - anchor) >= Math.abs(strike - anchor));
      cells.push(cell(priceText(strike, scale), `num strike${atm ? ' atm' : ''}`));

      if (side !== 'calls') cells.push(...this.#legCells(legs.put, 'put', view, scale));

      const tr = row(cells, 'clickable');
      tr.title = 'Open the contract on the side you click';
      tr.addEventListener('click', (event) => {
        const target = (event.target as HTMLElement).closest('td');
        const wanted = target?.classList.contains('opt-put') ? legs.put : legs.call;
        const fallback = wanted ?? legs.call ?? legs.put;
        if (fallback) this.context.run(`OPD ${fallback.contract}`);
      });
      return tr;
    });
  }

  #legCells(
    contract: OptionContract | undefined,
    leg: 'call' | 'put',
    view: ChainView,
    scale: number | undefined,
  ): HTMLTableCellElement[] {
    const side = leg === 'call' ? 'opt-call' : 'opt-put';
    const tone = moneyClass(contract);
    const make = (text: string, extra = ''): HTMLTableCellElement =>
      cell(text, `num ${side} ${tone} ${extra}`.replace(/\s+/g, ' ').trim(), 'td', contractOf(contract));

    if (!contract) {
      const width = view === 'greeks' ? 6 : 7;
      return Array.from({ length: width }, () => make('--'));
    }

    const g = contract.greeks;

    if (view === 'greeks') {
      const cells = [
        make(greek(g.theta, 3)),
        make(greek(g.vega, 3)),
        make(greek(g.gamma, 5)),
        make(greek(g.delta, 3), direction(g.delta)),
        make(volPercent(contract.iv)),
        make(priceText(contract.mid, scale)),
      ];
      return leg === 'call' ? cells : cells.reverse();
    }

    const delta = make(greek(g.delta, 3), direction(g.delta));
    const iv = make(volPercent(contract.iv));
    const oi = make(compact(contract.openInterest));
    const volume = make(compact(contract.volume));
    const bid = make(priceText(contract.bid, scale), 'price-bid');
    const ask = make(priceText(contract.ask, scale), 'price-ask');
    const last = make(priceText(contract.last, scale));

    if (leg === 'call') return [delta, iv, oi, volume, bid, ask, last];

    // The put side mirrors the call side — except for the bid/ask pair, which
    // stays in that order on both. Reversing the whole row is the obvious way
    // to mirror it and prints the ask under the BID header, which reads as a
    // crossed book on every quoted strike.
    return [last, bid, ask, volume, oi, iv, delta];
  }

  reconfigure(options: Partial<ChainPanelOptions>): void {
    this.#options = { ...this.#options, ...options };
    void this.refresh();
  }

  get options(): ChainPanelOptions {
    return { ...this.#options };
  }
}

/* ------------------------------------------------------------- OPD contract */

export class OptionDetailPanel extends Panel<OptionQuoteResponse> {
  override readonly kind = 'OPD';

  readonly #contract: string;
  #chart: TerminalChart | undefined;
  #latest: OptionQuoteResponse | undefined;

  constructor(id: string, context: PanelContext, contract: string) {
    super(id, context);
    this.#contract = contract;
    this.refreshMs = 15_000;
  }

  static idFor(contract: string): string {
    return `opd:${contract.toUpperCase()}`;
  }

  protected override title(): string {
    return this.#contract;
  }

  protected override subtitle(): string {
    const data = this.#latest;
    if (!data) return '';
    const c = data.contract;
    return `${data.symbol} ${c.type.toUpperCase()} ${priceText(c.strike, data.spot ?? undefined)} · ${day(new Date(c.expiry * 1000).toISOString().slice(0, 10))}`;
  }

  protected override load(signal: AbortSignal): Promise<OptionQuoteResponse> {
    return optionsApi.contract(this.#contract, 60, undefined, undefined, signal);
  }

  protected override render(data: OptionQuoteResponse): void {
    this.#latest = data;
    const c = data.contract;
    const scale = data.spot ?? undefined;
    const expiryIso = new Date(c.expiry * 1000).toISOString();

    this.body.append(
      el('div', { class: 'headline' }, [
        el('span', { class: 'headline-value', text: priceText(c.mid, scale) }),
        el('span', { class: 'headline-unit', text: data.currency }),
        el('span', {
          class: `headline-change ${direction(c.change)}`,
          text: c.change === null ? '' : signedPrice(c.change, scale),
        }),
        el('span', { class: 'headline-date', text: countdown(expiryIso) }),
      ]),
      el('div', { class: 'meta-strip' }, [
        field('UNDERLYING', `${data.symbol} @ ${priceText(data.spot)}`),
        field('TYPE', c.type.toUpperCase(), c.type === 'call' ? 'up' : 'down'),
        field('STRIKE', priceText(c.strike, scale)),
        field('EXPIRY', day(expiryIso.slice(0, 10))),
        field('MONEY', c.inTheMoney ? 'ITM' : 'OTM', c.inTheMoney ? 'up' : 'dim'),
        forwardField(data),
        field('MULT', `×${data.contractSize}`),
        field('SRC', `${data.venue} · ${data.source}`),
      ]),
    );

    // ---- Greeks ------------------------------------------------------------
    const g = c.greeks;
    this.body.append(
      el('div', { class: 'greek-grid' }, [
        greekTile('DELTA', greek(g.delta, 4), 'per 1 move in the underlying'),
        greekTile('GAMMA', greek(g.gamma, 6), 'delta per 1 move in the underlying'),
        greekTile('VEGA', greek(g.vega, 4), 'per 1 volatility point'),
        greekTile('THETA', greek(g.theta, 4), 'per calendar day'),
        greekTile('RHO', greek(g.rho, 4), 'per 1 percentage point of rate'),
        greekTile(
          'IV',
          volPercent(c.iv),
          c.ivSource === 'venue'
            ? "the venue's own mark volatility"
            : 'solved from the book mid against the fitted forward',
        ),
      ]),
    );

    // ---- pricing -----------------------------------------------------------
    const perContract = (value: number | null): string =>
      value === null ? '--' : priceText(value * data.contractSize, scale);

    this.body.append(
      el('div', { class: 'meta-strip' }, [
        field('BID', priceText(c.bid, scale), 'price-bid'),
        field('ASK', priceText(c.ask, scale), 'price-ask'),
        field('LAST', priceText(c.last, scale)),
        field('THEO', priceText(c.theo, scale)),
        field('INTRINSIC', priceText(c.intrinsic, scale)),
        field('EXTRINSIC', priceText(c.extrinsic, scale)),
        field('BREAKEVEN', priceText(c.breakeven, scale)),
        field('OI', group(c.openInterest)),
        field('VOL', group(c.volume)),
        field('PREMIUM', perContract(c.mid)),
      ]),
    );

    // ---- the other leg -----------------------------------------------------
    if (data.pair) {
      const p = data.pair;
      this.body.append(
        el('div', { class: 'result-note' }, [
          el('span', {
            text:
              `Pair leg ${p.type.toUpperCase()} ${priceText(p.strike, scale)}: ` +
              `${priceText(p.mid, scale)} · IV ${volPercent(p.iv)} · Δ ${greek(p.greeks.delta, 3)} · `,
          }),
          linkSpan(`OPD ${p.contract}`, this.context),
        ]),
      );
    }

    // ---- history -----------------------------------------------------------
    const host = el('div', { class: 'chart-host' });
    this.body.append(el('div', { class: 'chart-wrap option-history' }, [host]));

    if (data.history.length === 0) {
      host.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: 'No price history for this contract.' }),
          el('div', { class: 'panel-empty-hint', text: data.historyNote ?? '' }),
        ]),
      );
      return;
    }

    this.#chart?.destroy();
    const chart = new TerminalChart(host);
    this.#chart = chart;

    const precision = precisionFor(data.history.at(-1)?.close ?? 1);
    chart.addSpotCandles(precision).setData(toCandleData(data.history));
    chart.addVolume().setData(toVolumeData(data.history, chartTheme()));
    chart.fit();
    requestAnimationFrame(() => chart.resize());

    if (data.historyNote) {
      this.body.append(el('div', { class: 'result-note dim', text: data.historyNote }));
    }
  }

  override onResize(): void {
    this.#chart?.resize();
  }

  override destroy(): void {
    this.#chart?.destroy();
    this.#chart = undefined;
    super.destroy();
  }
}

function greekTile(label: string, value: string, explanation: string): HTMLElement {
  return el('div', { class: 'greek-tile', title: explanation }, [
    el('span', { class: 'greek-label', text: label }),
    el('span', { class: 'greek-value', text: value }),
  ]);
}

/** A clickable command, for cross-references inside prose. */
function linkSpan(command: string, context: PanelContext): HTMLElement {
  const node = el('span', { class: 'inline-command', text: command, title: `Run ${command}` });
  node.addEventListener('click', () => context.run(command));
  return node;
}

/* ------------------------------------------------------- VOL smile and term */

export interface VolPanelOptions {
  symbol: string;
  expiry?: string;
}

export class VolPanel extends Panel<OptionSurface> {
  override readonly kind = 'VOL';

  #options: VolPanelOptions;
  #smile: Plot | undefined;
  #term: Plot | undefined;
  #smileLegend: HTMLElement | undefined;
  #termLegend: HTMLElement | undefined;
  #latest: OptionSurface | undefined;

  constructor(id: string, context: PanelContext, options: VolPanelOptions) {
    super(id, context);
    this.#options = options;
    this.refreshMs = 60_000;
  }

  static idFor(symbol: string): string {
    return `vol:${symbol.toUpperCase()}`;
  }

  protected override title(): string {
    return this.#options.symbol;
  }

  protected override subtitle(): string {
    const surface = this.#latest;
    if (!surface) return '';
    const parts = [surface.expiry.date];
    if (surface.atmIv !== null) parts.push(`ATM ${volPercent(surface.atmIv)}`);
    if (surface.skew !== null) parts.push(`25Δ RR ${signedPercent(surface.skew * 100)}`);
    return parts.join(' · ');
  }

  protected override load(signal: AbortSignal): Promise<OptionSurface> {
    return optionsApi.surface(this.#options.symbol, this.#options.expiry, signal);
  }

  protected override render(surface: OptionSurface): void {
    this.#latest = surface;
    const palette = overlayPalette();

    this.body.append(
      el('div', { class: 'meta-strip' }, [
        field('SPOT', priceText(surface.spot)),
        field('FWD', priceText(surface.forward, surface.spot ?? undefined)),
        field('EXP', day(surface.expiry.date)),
        field('DTE', `${surface.expiry.daysToExpiry}d`),
        field('ATM IV', volPercent(surface.atmIv)),
        field(
          '25Δ RR',
          surface.skew === null ? '--' : signedPercent(surface.skew * 100),
          direction(surface.skew),
        ),
        field('SRC', `${surface.venue} · ${surface.source}`),
      ]),
    );

    const picker = el('div', { class: 'implied-picker' });
    renderExpiryPicker(
      picker,
      surface.term.map((t) => ({
        expiry: t.expiry,
        date: t.date,
        yearsToExpiry: 0,
        daysToExpiry: t.daysToExpiry,
        contracts: 0,
        openInterest: t.openInterest,
        volume: t.volume,
      })),
      surface.expiry.expiry,
      (expiry) => {
        this.#options = { ...this.#options, expiry: expiry.date };
        void this.refresh();
      },
    );
    this.body.append(picker);

    if (surface.note) this.body.append(el('div', { class: 'result-note', text: surface.note }));

    // ---- smile -------------------------------------------------------------
    const smileLegend = el('div', { class: 'chart-legend' });
    const smileHost = el('div', { class: 'plot-host' });
    this.#smileLegend = smileLegend;

    // ---- term structure ----------------------------------------------------
    const termLegend = el('div', { class: 'chart-legend' });
    const termHost = el('div', { class: 'plot-host' });
    this.#termLegend = termLegend;

    this.body.append(
      el('div', { class: 'plot-section' }, [
        el('div', { class: 'plot-title', text: `SMILE · ${surface.expiry.date}` }),
        smileLegend,
        smileHost,
      ]),
      el('div', { class: 'plot-section' }, [
        el('div', { class: 'plot-title', text: 'TERM STRUCTURE · at-the-money' }),
        termLegend,
        termHost,
      ]),
    );

    const smileSeries: PlotSeries[] = [
      {
        label: 'CALL',
        color: palette[0]!,
        markers: true,
        points: surface.smile
          .filter((s) => s.callIv !== null)
          .map((s) => ({ x: s.strike, y: s.callIv! * 100 })),
      },
      {
        label: 'PUT',
        color: palette[1]!,
        markers: true,
        dashed: true,
        points: surface.smile
          .filter((s) => s.putIv !== null)
          .map((s) => ({ x: s.strike, y: s.putIv! * 100 })),
      },
    ].filter((s) => s.points.length > 0);

    this.#smile?.destroy();
    const smile = new Plot(smileHost);
    this.#smile = smile;
    smile.setData(smileSeries, {
      formatX: (v) => priceText(v, surface.spot ?? undefined),
      formatY: (v) => `${v.toFixed(0)}%`,
      markers: [
        ...(surface.spot !== null ? [{ x: surface.spot, label: 'SPOT' }] : []),
        ...(surface.forward !== null && surface.forward !== surface.spot
          ? [{ x: surface.forward, label: 'FWD', color: chartTheme().dim }]
          : []),
      ],
      onHover: (x, values) => {
        if (x === null) {
          this.#setSmileLegend(surface, null, []);
          return;
        }
        this.#setSmileLegend(surface, x, values.map((v) => [v.series.label, v.point.y]));
      },
    });

    this.#term?.destroy();
    const term = new Plot(termHost);
    this.#term = term;
    term.setData(
      [
        {
          label: 'ATM IV',
          color: palette[2]!,
          markers: true,
          points: surface.term
            .filter((t) => t.atmIv !== null)
            .map((t) => ({ x: t.daysToExpiry, y: t.atmIv! * 100, label: t.date })),
        },
      ],
      {
        formatX: (v) => `${Math.round(v)}d`,
        formatY: (v) => `${v.toFixed(0)}%`,
        markers: [{ x: surface.expiry.daysToExpiry, label: 'SEL' }],
        onHover: (x, values) => {
          const first = values[0];
          if (x === null || !first) {
            this.#setTermLegend(surface, null, null, null);
            return;
          }
          this.#setTermLegend(surface, x, first.point.y, first.point.label ?? null);
        },
      },
    );

    this.#setSmileLegend(surface, null, []);
    this.#setTermLegend(surface, null, null, null);

    // The plots size themselves from their host, which has no height until the
    // grid has laid the tile out.
    requestAnimationFrame(() => {
      smile.resize();
      term.resize();
    });
  }

  #setSmileLegend(surface: OptionSurface, strike: number | null, values: [string, number][]): void {
    const legend = this.#smileLegend;
    if (!legend) return;

    if (strike === null) {
      legend.replaceChildren(
        el('span', { class: 'legend-time', text: 'ATM' }),
        legendPair('IV', volPercent(surface.atmIv)),
        legendPair('25Δ RR', surface.skew === null ? '--' : signedPercent(surface.skew * 100)),
        legendPair('RUNGS', String(surface.smile.length)),
      );
      return;
    }

    const rung = surface.smile.find((s) => s.strike === strike);
    legend.replaceChildren(
      el('span', { class: 'legend-time', text: priceText(strike, surface.spot ?? undefined) }),
      ...values.map(([label, value]) => legendPair(label, `${value.toFixed(1)}%`)),
      legendPair('OI', compact(rung?.openInterest ?? null)),
      legendPair('MNY', rung?.moneyness === null || rung === undefined ? '--' : rung.moneyness.toFixed(3)),
    );
  }

  #setTermLegend(
    surface: OptionSurface,
    days: number | null,
    iv: number | null,
    date: string | null,
  ): void {
    const legend = this.#termLegend;
    if (!legend) return;

    if (days === null) {
      const quoted = surface.term.filter((t) => t.atmIv !== null).length;
      legend.replaceChildren(
        el('span', { class: 'legend-time', text: `${quoted} expiries` }),
        legendPair('FRONT', volPercent(surface.term.find((t) => t.atmIv !== null)?.atmIv ?? null)),
        legendPair(
          'BACK',
          volPercent([...surface.term].reverse().find((t) => t.atmIv !== null)?.atmIv ?? null),
        ),
      );
      return;
    }

    legend.replaceChildren(
      el('span', { class: 'legend-time', text: date ?? `${Math.round(days)}d` }),
      legendPair('ATM IV', iv === null ? '--' : `${iv.toFixed(1)}%`),
      legendPair('DTE', `${Math.round(days)}d`),
    );
  }

  reconfigure(options: Partial<VolPanelOptions>): void {
    this.#options = { ...this.#options, ...options };
    void this.refresh();
  }

  override onResize(): void {
    this.#smile?.resize();
    this.#term?.resize();
  }

  override destroy(): void {
    this.#smile?.destroy();
    this.#term?.destroy();
    this.#smile = undefined;
    this.#term = undefined;
    super.destroy();
  }
}

function legendPair(key: string, value: string): HTMLElement {
  return el('span', { class: 'legend-pair' }, [
    el('span', { class: 'legend-key', text: key }),
    el('span', { text: value }),
  ]);
}

/* --------------------------------------------------- OI open interest ladder */

export interface OpenInterestPanelOptions {
  symbol: string;
  expiry?: string;
}

export class OpenInterestPanel extends Panel<OptionPositioning> {
  override readonly kind = 'OI';

  #options: OpenInterestPanelOptions;
  #latest: OptionPositioning | undefined;

  constructor(id: string, context: PanelContext, options: OpenInterestPanelOptions) {
    super(id, context);
    this.#options = options;
    this.refreshMs = 60_000;
  }

  static idFor(symbol: string): string {
    return `oi:${symbol.toUpperCase()}`;
  }

  protected override title(): string {
    return this.#options.symbol;
  }

  protected override subtitle(): string {
    const data = this.#latest;
    if (!data) return '';
    const parts = [data.expiry.date];
    if (data.maxPain !== null) parts.push(`pain ${priceText(data.maxPain, data.spot ?? undefined)}`);
    return parts.join(' · ');
  }

  protected override load(signal: AbortSignal): Promise<OptionPositioning> {
    return optionsApi.positioning(this.#options.symbol, this.#options.expiry, signal);
  }

  protected override render(data: OptionPositioning): void {
    this.#latest = data;
    const scale = data.spot ?? undefined;

    this.body.append(
      el('div', { class: 'meta-strip' }, [
        field('SPOT', priceText(data.spot)),
        field('EXP', day(data.expiry.date)),
        field('DTE', `${data.expiry.daysToExpiry}d`),
        field('MAX PAIN', priceText(data.maxPain, scale), 'strong'),
        field(
          'P/C OI',
          data.putCallOpenInterest === null ? '--' : data.putCallOpenInterest.toFixed(2),
          direction(data.putCallOpenInterest === null ? null : data.putCallOpenInterest - 1),
        ),
        field(
          'P/C VOL',
          data.putCallVolume === null ? '--' : data.putCallVolume.toFixed(2),
          direction(data.putCallVolume === null ? null : data.putCallVolume - 1),
        ),
        field('CALL OI', compact(data.totalCallOpenInterest)),
        field('PUT OI', compact(data.totalPutOpenInterest)),
        field('SRC', `${data.venue} · ${data.source}`),
      ]),
    );

    const picker = el('div', { class: 'implied-picker' });
    renderExpiryPicker(picker, [data.expiry], data.expiry.expiry, () => {});
    this.body.append(picker);

    this.body.append(
      el('div', {
        class: 'result-note',
        text:
          'Max pain is the settlement price at which the least option value pays out. ' +
          'It is a read on where open interest sits, not a forecast of where price goes.',
      }),
    );

    if (data.strikes.length === 0) {
      this.body.append(el('div', { class: 'panel-empty', text: 'No open interest on this expiry.' }));
      return;
    }

    const peak = Math.max(
      1,
      ...data.strikes.map((s) => Math.max(s.callOpenInterest, s.putOpenInterest)),
    );

    // Centre the ladder on the money, the way the chain is windowed.
    const anchor = data.spot;
    let strikes = data.strikes;
    if (anchor !== null && strikes.length > 40) {
      const index = strikes.reduce(
        (best, s, i) =>
          Math.abs(s.strike - anchor) < Math.abs(strikes[best]!.strike - anchor) ? i : best,
        0,
      );
      strikes = strikes.slice(Math.max(0, index - 20), index + 21);
    }

    const rows = strikes.map((stat) => {
      const isPain = data.maxPain !== null && stat.strike === data.maxPain;
      const nearest =
        anchor !== null &&
        strikes.every((o) => Math.abs(o.strike - anchor) >= Math.abs(stat.strike - anchor));

      const tr = row(
        [
          cell(compact(stat.callVolume), 'num dim'),
          cell(compact(stat.callOpenInterest), 'num opt-call'),
          barCell(stat.callOpenInterest / peak, 'call'),
          cell(priceText(stat.strike, scale), `num strike${nearest ? ' atm' : ''}`),
          barCell(stat.putOpenInterest / peak, 'put'),
          cell(compact(stat.putOpenInterest), 'num opt-put'),
          cell(compact(stat.putVolume), 'num dim'),
        ],
        isPain ? 'max-pain' : '',
      );
      if (isPain) tr.title = 'Max pain';
      return tr;
    });

    this.body.append(
      table(['C VOL', 'C OI', '', 'STRIKE', '', 'P OI', 'P VOL'], rows, 'oi-ladder'),
    );
  }

  reconfigure(options: Partial<OpenInterestPanelOptions>): void {
    this.#options = { ...this.#options, ...options };
    void this.refresh();
  }
}

/** A proportional bar, mirroring the order-book depth ladder. */
function barCell(fraction: number, side: 'call' | 'put'): HTMLTableCellElement {
  const td = document.createElement('td');
  td.className = `oi-bar-cell ${side}`;
  const bar = el('div', { class: `oi-bar ${side}` });
  bar.style.width = `${Math.max(0, Math.min(1, fraction)) * 100}%`;
  td.append(bar);
  return td;
}
