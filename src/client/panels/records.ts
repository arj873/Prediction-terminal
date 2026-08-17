/**
 * The four publishers whose output is records rather than observations.
 *
 * A Commitments of Traders report, a bill's legislative history, an EDGAR
 * filing list and a data.gov dataset entry are not time series, and flattening
 * them into one would lose exactly what makes each useful: who is positioned
 * which way, where a bill has got to, which form was filed this morning, what
 * format a dataset comes in. So they keep their own shapes — but every one of
 * them is a table with clickable rows that run the next command, which is the
 * pattern the rest of the terminal already uses.
 */

import type {
  BillDetail,
  BillSearchResponse,
  CotReport,
  DataGovSearchResponse,
  SecConceptResponse,
  SecFilingsResponse,
} from '../../shared/types.js';
import { data } from '../lib/api.js';
import { TerminalChart, toFredData, type MouseEventParams } from '../lib/chart.js';
import { cell, el, field, row, table } from '../lib/dom.js';
import { compact, day, direction, group, metric, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';

/* -------------------------------------------------------------------- COT */

export interface CotPanelOptions {
  market: string;
  report: string;
}

export class CotPanel extends Panel<CotReport> {
  override readonly kind = 'COT';

  readonly #options: CotPanelOptions;

  constructor(id: string, context: PanelContext, options: CotPanelOptions) {
    super(id, context);
    this.#options = options;
    // Published Friday afternoons. Hourly is generous and costs one request.
    this.refreshMs = 60 * 60_000;
  }

  static idFor(options: CotPanelOptions): string {
    return `cot:${options.report}:${options.market.toLowerCase()}`;
  }

  protected override title(): string {
    return this.latest?.market ?? this.#options.market.toUpperCase();
  }

  protected override subtitle(): string {
    const report = this.latest;
    return report ? `${report.exchange} · ${day(report.date)}` : this.#options.report;
  }

  protected override load(signal: AbortSignal): Promise<CotReport> {
    return data.cot(this.#options.market, this.#options.report, signal);
  }

  protected override render(report: CotReport): void {
    this.body.append(
      el('div', { class: 'meta-strip' }, [
        field('REPORT', report.reportLabel),
        field('EXCHANGE', report.exchange || '—'),
        field('CODE', report.contractCode || '—'),
        field('AS OF', day(report.date)),
        field('OPEN INT', group(report.openInterest)),
        field(
          'OI CHG',
          report.openInterestChange === null ? '—' : signed(report.openInterestChange),
          direction(report.openInterestChange),
        ),
      ]),
    );

    const rows = report.categories.map((category) => {
      const tr = row([
        cell(category.name),
        cell(group(category.long), 'num'),
        cell(group(category.short), 'num'),
        cell(category.spreading === null ? '—' : group(category.spreading), 'num dim'),
        cell(group(category.net), `num strong ${direction(category.net)}`),
        cell(
          category.netChange === null ? '—' : signed(category.netChange),
          `num ${direction(category.netChange)}`,
        ),
        cell(percentOrDash(category.percentLong), 'num dim'),
        cell(percentOrDash(category.percentShort), 'num dim'),
      ]);

      // The net-position history is the chart people actually want out of a COT
      // table, and the id for it is derivable from the row — so the row runs it.
      const key = categoryKey(category.name);
      if (key) {
        tr.classList.add('clickable');
        tr.title = `Chart ${category.name} net position`;
        tr.addEventListener('click', () =>
          this.context.run(`ECO cftc:${report.report}/${report.contractCode}/${key}_net`),
        );
      }
      return tr;
    });

    this.body.append(
      table(['CATEGORY', 'LONG', 'SHORT', 'SPREAD', 'NET', 'NET CHG', '%LONG', '%SHORT'], rows),
      el('div', {
        class: 'result-note dim',
        text:
          'Positions are as of the report Tuesday and published the following Friday. ' +
          'Click a row to chart its net position since 1986.',
      }),
    );
  }
}

function signed(value: number): string {
  return `${value > 0 ? '+' : ''}${group(value)}`;
}

function percentOrDash(value: number | null): string {
  return value === null ? '—' : `${value.toFixed(1)}%`;
}

/**
 * A category name back to the key a series id uses.
 *
 * The report table and the series ids are two views of the same table on the
 * server; this is the one place the client has to reconnect them, so it does it
 * by matching the name's leading word rather than by keeping a second copy of
 * the mapping that could drift.
 */
function categoryKey(name: string): string | null {
  const lower = name.toLowerCase();
  if (lower.startsWith('non-commercial')) return 'noncomm';
  if (lower.startsWith('commercial')) return 'comm';
  if (lower.startsWith('non-reportable')) return 'nonrept';
  if (lower.startsWith('producer')) return 'prod';
  if (lower.startsWith('swap')) return 'swap';
  if (lower.startsWith('managed money')) return 'mmoney';
  if (lower.startsWith('dealer')) return 'dealer';
  if (lower.startsWith('asset manager')) return 'assetmgr';
  if (lower.startsWith('leveraged')) return 'levfund';
  if (lower.startsWith('other reportable')) return 'other';
  return null;
}

/* --------------------------------------------------------------- congress */

export class BillsPanel extends Panel<BillSearchResponse> {
  override readonly kind = 'CONG';

  readonly #query: string;
  readonly #congress: number | undefined;

  constructor(id: string, context: PanelContext, query: string, congress?: number) {
    super(id, context);
    this.#query = query;
    this.#congress = congress;
    this.refreshMs = 10 * 60_000;
  }

  static idFor(query: string, congress?: number): string {
    return `cong:${congress ?? ''}:${query.toLowerCase()}`;
  }

  protected override title(): string {
    return this.#query ? `"${this.#query}"` : 'RECENT BILLS';
  }

  protected override subtitle(): string {
    const congress = this.latest?.congress ?? this.#congress;
    return congress ? `${congress}th Congress` : '';
  }

  protected override load(signal: AbortSignal): Promise<BillSearchResponse> {
    return data.bills(this.#query, this.#congress, 60, signal);
  }

  protected override render(payload: BillSearchResponse): void {
    if (payload.bills.length === 0) {
      this.body.append(
        el('div', {
          class: 'panel-empty',
          text: `No bills in the ${payload.congress}th Congress match "${payload.query}".`,
        }),
        el('div', {
          class: 'result-note dim',
          text:
            'Congress.gov publishes no full-text search API, so this matches titles and ' +
            'sponsors among that Congress’s most recently active bills.',
        }),
      );
      return;
    }

    const rows = payload.bills.map((bill) => {
      const tr = row([
        cell(`${bill.type.toUpperCase()} ${bill.number}`, 'mono strong'),
        cell(truncate(bill.title, 66)),
        cell(bill.sponsor ? `${bill.sponsor}` : '', 'dim'),
        cell(day(bill.introducedDate), 'dim'),
        cell(truncate(bill.latestAction?.text ?? '', 44), 'dim'),
        cell(bill.becameLaw ? 'LAW' : '', bill.becameLaw ? 'up strong' : ''),
      ]);
      tr.classList.add('clickable');
      tr.title = `Open ${bill.label}`;
      tr.addEventListener('click', () =>
        this.context.run(`CONG BILL ${bill.congress} ${bill.type} ${bill.number}`),
      );
      return tr;
    });

    this.body.append(
      el('div', {
        class: 'result-note',
        text: `${payload.bills.length} bills · via ${payload.source}`,
      }),
      table(['BILL', 'TITLE', 'SPONSOR', 'INTRODUCED', 'LATEST ACTION', ''], rows),
    );
  }
}

export interface BillPanelOptions {
  congress: number;
  type: string;
  number: string;
}

export class BillPanel extends Panel<BillDetail> {
  override readonly kind = 'BILL';

  readonly #options: BillPanelOptions;

  constructor(id: string, context: PanelContext, options: BillPanelOptions) {
    super(id, context);
    this.#options = options;
    this.refreshMs = 10 * 60_000;
  }

  static idFor(options: BillPanelOptions): string {
    return `bill:${options.congress}:${options.type}:${options.number}`.toLowerCase();
  }

  protected override title(): string {
    return (
      this.latest?.label ??
      `${this.#options.type.toUpperCase()} ${this.#options.number} (${this.#options.congress})`
    );
  }

  protected override subtitle(): string {
    return this.latest ? truncate(this.latest.title, 64) : '';
  }

  protected override load(signal: AbortSignal): Promise<BillDetail> {
    return data.bill(this.#options.congress, this.#options.type, this.#options.number, signal);
  }

  protected override render(bill: BillDetail): void {
    this.body.append(
      el('div', { class: 'meta-strip' }, [
        field('SPONSOR', [bill.sponsor, bill.sponsorParty, bill.sponsorState].filter(Boolean).join(' · ') || '—'),
        field('COSPONSORS', bill.cosponsors === null ? '—' : String(bill.cosponsors)),
        field('CHAMBER', bill.originChamber || '—'),
        field('INTRODUCED', day(bill.introducedDate)),
        field('POLICY', bill.policyArea || '—'),
        field('STATUS', bill.becameLaw ? 'BECAME LAW' : 'PENDING', bill.becameLaw ? 'up' : ''),
      ]),
      el('div', { class: 'headline' }, [
        el('span', { class: 'headline-date', text: bill.title }),
      ]),
    );

    if (bill.committees.length > 0) {
      this.body.append(
        el('div', { class: 'result-note dim', text: `Committees: ${bill.committees.join(' · ')}` }),
      );
    }

    if (bill.summary) {
      this.body.append(
        el('details', { class: 'notes', open: true }, [
          el('summary', { text: 'SUMMARY' }),
          el('p', { text: bill.summary }),
        ]),
      );
    }

    const rows = bill.actions
      .slice(0, 120)
      .map((action) =>
        row([
          cell(day(action.date), 'mono dim'),
          cell(action.chamber, 'dim'),
          cell(action.text),
        ]),
      );

    this.body.append(
      table(['DATE', 'CHAMBER', 'ACTION'], rows),
      el('div', {}, [
        el('a', {
          href: bill.url,
          target: '_blank',
          rel: 'noopener noreferrer',
          class: 'result-note',
          text: bill.url,
        }),
      ]),
    );
  }
}

/* -------------------------------------------------------------- sec edgar */

export interface SecPanelOptions {
  query: string;
  form?: string;
}

export class SecFilingsPanel extends Panel<SecFilingsResponse> {
  override readonly kind = 'SEC';

  readonly #options: SecPanelOptions;

  constructor(id: string, context: PanelContext, options: SecPanelOptions) {
    super(id, context);
    this.#options = options;
    // An 8-K is on the wire within seconds of acceptance; five minutes is the
    // shortest useful cadence given the server caches for the same.
    this.refreshMs = 5 * 60_000;
  }

  static idFor(options: SecPanelOptions): string {
    return `sec:${options.query.toLowerCase()}:${(options.form ?? '').toLowerCase()}`;
  }

  protected override title(): string {
    return this.latest?.company.name ?? this.#options.query.toUpperCase();
  }

  protected override subtitle(): string {
    const company = this.latest?.company;
    if (!company) return this.#options.form ?? '';
    return [company.tickers.join('/'), company.sicDescription, this.#options.form]
      .filter(Boolean)
      .join(' · ');
  }

  protected override load(signal: AbortSignal): Promise<SecFilingsResponse> {
    return data.secFilings(this.#options.query, this.#options.form, 60, signal);
  }

  protected override render(payload: SecFilingsResponse): void {
    const { company } = payload;

    this.body.append(
      el('div', { class: 'meta-strip' }, [
        field('CIK', company.cik),
        field('TICKERS', company.tickers.join(', ') || '—'),
        field('EXCHANGES', company.exchanges.join(', ') || '—'),
        field('SIC', company.sic ? `${company.sic} ${company.sicDescription}` : '—'),
        field('FY END', company.fiscalYearEnd || '—'),
      ]),
    );

    if (payload.filings.length === 0) {
      this.body.append(el('div', { class: 'panel-empty', text: 'No filings returned.' }));
      return;
    }

    const rows = payload.filings.map((filing) => {
      const tr = row([
        cell(filing.form, 'mono strong'),
        cell(day(filing.filed), 'mono'),
        cell(filing.reportDate ? day(filing.reportDate) : '', 'dim'),
        cell(truncate(filing.description || filing.primaryDocument, 52)),
        cell(filing.items, 'dim'),
        cell(filing.size === null ? '' : compact(filing.size), 'num dim'),
      ]);
      tr.classList.add('clickable');
      tr.title = `Open ${filing.accession} on sec.gov`;
      tr.addEventListener('click', () => window.open(filing.url, '_blank', 'noopener'));
      return tr;
    });

    this.body.append(
      el('div', {
        class: 'result-note',
        text: `${payload.filings.length} filings · forms present: ${payload.forms.slice(0, 14).join(', ')}`,
      }),
      table(['FORM', 'FILED', 'PERIOD', 'DOCUMENT', 'ITEMS', 'SIZE'], rows),
    );
  }
}

/**
 * One XBRL concept's whole reported history, charted.
 *
 * Restatements are the interesting part and are preserved rather than
 * deduplicated: the same period reported twice, by two filings, is the company
 * changing its mind — which the table shows with the form and filing date
 * beside each value.
 */
export class SecConceptPanel extends Panel<SecConceptResponse> {
  override readonly kind = 'SECF';

  readonly #query: string;
  readonly #tag: string;
  #chart: TerminalChart | undefined;
  #legend: HTMLElement | undefined;

  constructor(id: string, context: PanelContext, query: string, tag: string) {
    super(id, context);
    this.#query = query;
    this.#tag = tag;
    this.refreshMs = 30 * 60_000;
  }

  static idFor(query: string, tag: string): string {
    return `secf:${query.toLowerCase()}:${tag.toLowerCase()}`;
  }

  protected override title(): string {
    return `${(this.latest?.company.tickers[0] ?? this.#query).toUpperCase()} ${this.#tag}`;
  }

  protected override subtitle(): string {
    const payload = this.latest;
    return payload ? truncate(`${payload.company.name} · ${payload.label}`, 68) : '';
  }

  protected override load(signal: AbortSignal): Promise<SecConceptResponse> {
    return data.secConcept(this.#query, this.#tag, 'us-gaap', signal);
  }

  protected override render(payload: SecConceptResponse): void {
    this.#chart?.destroy();
    this.#chart = undefined;

    const { facts } = payload;
    const last = facts.at(-1);

    this.body.append(
      el('div', { class: 'headline' }, [
        el('span', { class: 'headline-value', text: metric(last?.value ?? null) }),
        el('span', { class: 'headline-unit', text: payload.unit }),
        el('span', { class: 'headline-date', text: last ? day(last.end) : '' }),
      ]),
      el('div', { class: 'meta-strip' }, [
        field('TAXONOMY', payload.taxonomy),
        field('TAG', payload.tag),
        field('UNIT', payload.unit || '—'),
        field('FACTS', String(facts.length)),
        field('CIK', payload.company.cik),
      ]),
    );

    if (facts.length === 0) {
      this.body.append(el('div', { class: 'panel-empty', text: 'No reported values.' }));
      return;
    }

    const legend = el('div', { class: 'chart-legend' });
    const host = el('div', { class: 'chart-host' });
    this.#legend = legend;
    this.body.append(el('div', { class: 'chart-wrap' }, [legend, host]));

    const chart = new TerminalChart(host);
    this.#chart = chart;

    // One point per period end. A restatement shares an `end` with the value it
    // replaces, so the later filing — which sorts last — is the one drawn, and
    // the table below keeps both.
    const series = chart.addFredArea();
    series.setData(toFredData(facts.map((f) => ({ date: f.end, value: f.value }))));

    chart.chart.subscribeCrosshairMove((param: MouseEventParams) => {
      // The *last* fact for a period, not the first: when a value has been
      // restated, the crosshair should read the same number the line is drawn
      // from, which is the most recently filed one.
      const at = param.time
        ? facts.filter((f) => f.end === String(param.time)).at(-1)
        : last;
      this.#setLegend(at?.end ?? null, at?.value ?? null);
    });

    chart.fit();
    requestAnimationFrame(() => chart.resize());
    this.#setLegend(last?.end ?? null, last?.value ?? null);

    const rows = facts
      .slice()
      .reverse()
      .slice(0, 60)
      .map((fact) =>
        row([
          cell(day(fact.end), 'mono'),
          cell(fact.start ? day(fact.start) : '', 'mono dim'),
          cell(metric(fact.value), 'num strong'),
          cell(fact.fiscalYear === null ? '' : `${fact.fiscalYear} ${fact.fiscalPeriod}`, 'dim'),
          cell(fact.form, 'mono dim'),
          cell(day(fact.filed), 'dim'),
        ]),
      );

    this.body.append(table(['PERIOD END', 'START', 'VALUE', 'FISCAL', 'FORM', 'FILED'], rows));

    if (payload.description) {
      this.body.append(
        el('details', { class: 'notes' }, [
          el('summary', { text: 'DEFINITION' }),
          el('p', { text: payload.description }),
        ]),
      );
    }
  }

  #setLegend(date: string | null, value: number | null): void {
    if (!this.#legend) return;
    this.#legend.replaceChildren(
      el('span', { class: 'legend-time', text: date ? day(date) : '—' }),
      el('span', { class: 'legend-pair' }, [
        el('span', { class: 'legend-key', text: 'VAL' }),
        el('span', { text: metric(value) }),
      ]),
    );
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

/* --------------------------------------------------------------- data.gov */

export class DataGovPanel extends Panel<DataGovSearchResponse> {
  override readonly kind = 'DGOV';

  readonly #query: string;

  constructor(id: string, context: PanelContext, query: string) {
    super(id, context);
    this.#query = query;
  }

  static idFor(query: string): string {
    return `dgov:${query.toLowerCase()}`;
  }

  protected override title(): string {
    return `"${this.#query}"`;
  }

  protected override load(signal: AbortSignal): Promise<DataGovSearchResponse> {
    return data.gov(this.#query, 50, undefined, signal);
  }

  protected override render(payload: DataGovSearchResponse): void {
    if (payload.datasets.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty', text: `No datasets match "${payload.query}".` }),
      );
      return;
    }

    const rows = payload.datasets.map((dataset) => {
      const tr = row([
        cell(truncate(dataset.title, 62)),
        cell(truncate(dataset.publisher, 32), 'dim'),
        cell(day(dataset.modified), 'mono dim'),
        cell(dataset.frequency, 'dim'),
        cell(dataset.formats.slice(0, 4).join(' '), 'mono dim'),
      ]);
      if (dataset.description) tr.title = dataset.description;
      if (dataset.url) {
        tr.classList.add('clickable');
        tr.addEventListener('click', () => window.open(dataset.url, '_blank', 'noopener'));
      }
      return tr;
    });

    this.body.append(
      el('div', {
        class: 'result-note',
        text: `${payload.datasets.length} datasets · via ${payload.source}`,
      }),
      table(['DATASET', 'PUBLISHER', 'UPDATED', 'FREQUENCY', 'FORMATS'], rows),
    );
  }
}
