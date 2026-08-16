/**
 * Crypto options from Deribit's public API.
 *
 * There was no real choice to make here, which is worth stating plainly rather
 * than dressing up as a survey: Deribit is where crypto options trade. It has
 * consistently carried the large majority of global open interest in BTC and
 * ETH options, and its expiries are the ones every desk quotes off. A crypto
 * options panel sourced from anywhere else would be showing a shadow of the
 * real board.
 *
 * It also happens to fit this codebase's constraints exactly — the same ones
 * that picked Coinbase for spot in `sources/crypto.ts`: no key, no account, no
 * geo-fence, documented rate limits, and it answers from datacentre IPs, which
 * the equity feeds mostly do not.
 *
 * Two product families, and the difference leaks into the arithmetic:
 *
 *   * **Inverse** (`BTC-25DEC26-104000-C`) — coin-margined, and *quoted in the
 *     coin*. A mark of `0.623` means 0.623 BTC, so every price is multiplied by
 *     the index to reach the USD this terminal displays everywhere else.
 *   * **Linear** (`SOL_USDC-17AUG26-66-C`) — USDC-margined and already quoted
 *     in dollars per unit of underlying.
 *
 * The whole board arrives in two requests per currency — `get_instruments` for
 * the contract definitions and `get_book_summary_by_currency` for every quote —
 * rather than one request per contract, which for BTC alone would be 818.
 */

import type { CandleInterval, OptionUnderlying, SpotCandle } from '../../shared/types.js';
import type { BoardQuote, OptionBoard } from './optionboard.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const BASE = process.env.DERIBIT_API_BASE ?? 'https://www.deribit.com/api/v2';

/**
 * The underlyings Deribit lists options on, and how to reach each one.
 *
 * BTC and ETH are taken from the inverse book rather than their USDC-linear
 * twins: both exist, and the coin-margined board is the deeper and older one by
 * a wide margin, so it is the price discovery venue. Everything else is only
 * listed as USDC-linear.
 */
const UNDERLYINGS: Record<string, { currency: string; linear: boolean; name: string }> = {
  BTC: { currency: 'BTC', linear: false, name: 'Bitcoin' },
  ETH: { currency: 'ETH', linear: false, name: 'Ether' },
  SOL: { currency: 'USDC', linear: true, name: 'Solana' },
  XRP: { currency: 'USDC', linear: true, name: 'XRP' },
  AVAX: { currency: 'USDC', linear: true, name: 'Avalanche' },
  HYPE: { currency: 'USDC', linear: true, name: 'Hyperliquid' },
  TRX: { currency: 'USDC', linear: true, name: 'TRON' },
};

/** Common spellings that mean one of the above. */
const ALIASES: Record<string, string> = {
  XBT: 'BTC',
  BITCOIN: 'BTC',
  ETHEREUM: 'ETH',
  ETHER: 'ETH',
  SOLANA: 'SOL',
  RIPPLE: 'XRP',
  AVALANCHE: 'AVAX',
  HYPERLIQUID: 'HYPE',
  TRON: 'TRX',
};

export function listUnderlyings(): OptionUnderlying[] {
  return Object.entries(UNDERLYINGS).map(([symbol, info]) => ({
    symbol,
    name: info.name,
    assetClass: 'crypto' as const,
    venue: 'Deribit',
  }));
}

/** `BTC-USD`, `bitcoin`, `btc` → `BTC`. `null` when Deribit lists no board. */
export function resolveSymbol(symbol: string): string | null {
  const upper = symbol.trim().toUpperCase();
  const base = upper.split(/[-_/]/)[0] ?? upper;
  const candidate = ALIASES[base] ?? base;
  return candidate in UNDERLYINGS ? candidate : null;
}

export function supports(symbol: string): boolean {
  return resolveSymbol(symbol) !== null;
}

/* ------------------------------------------------------------------- wire */

interface RawEnvelope<T> {
  result?: T;
  error?: { code?: number; message?: string };
}

interface RawInstrument {
  instrument_name: string;
  base_currency?: string;
  quote_currency?: string;
  counter_currency?: string;
  price_index?: string;
  instrument_type?: string;
  option_type?: string;
  strike?: number;
  expiration_timestamp?: number;
  contract_size?: number;
  is_active?: boolean;
}

interface RawSummary {
  instrument_name: string;
  base_currency?: string;
  bid_price?: number | null;
  ask_price?: number | null;
  mid_price?: number | null;
  mark_price?: number | null;
  last?: number | null;
  price_change?: number | null;
  volume?: number | null;
  open_interest?: number | null;
  mark_iv?: number | null;
  underlying_price?: number | null;
  interest_rate?: number | null;
  estimated_delivery_price?: number | null;
}

interface RawChart {
  ticks?: number[];
  open?: number[];
  high?: number[];
  low?: number[];
  close?: number[];
  volume?: number[];
  status?: string;
}

async function get<T>(path: string, ttlMs: number): Promise<T> {
  const body = await cache.cached(`deribit:${path}`, ttlMs, () =>
    fetchJson<RawEnvelope<T>>(`${BASE}${path}`, { timeoutMs: 20_000, retries: 1 }),
  );

  if (body.error) {
    throw new UpstreamError(`Deribit rejected the request: ${body.error.message ?? 'unknown'}`, {
      code: 'upstream_error',
      hint: 'Deribit answered with a JSON-RPC error rather than data.',
    });
  }
  if (body.result === undefined) {
    throw new UpstreamError('Deribit returned no result', { code: 'bad_upstream_body' });
  }
  return body.result;
}

function num(value: number | null | undefined): number | null {
  return value === null || value === undefined || !Number.isFinite(value) ? null : value;
}

/** Deribit reports an empty book side as `null`, and a dead one as `0`. */
function quotePrice(value: number | null | undefined): number | null {
  const n = num(value);
  return n === null || n <= 0 ? null : n;
}

/* ------------------------------------------------------------------ board */

/**
 * Split a Deribit instrument name into its parts.
 *
 * Both families are handled by one rule because both put the expiry, the strike
 * and the leg in the same three trailing segments; only the head differs
 * (`BTC` versus `SOL_USDC`).
 */
export function parseInstrument(
  name: string,
): { base: string; currency: string; expiryLabel: string; strike: number; type: 'call' | 'put' } | null {
  const parts = name.trim().toUpperCase().split('-');
  if (parts.length !== 4) return null;
  const [head, expiryLabel, strike, leg] = parts as [string, string, string, string];

  const [base, quote] = head.includes('_')
    ? (head.split('_') as [string, string])
    : ([head, head] as [string, string]);

  const strikeValue = Number(strike);
  if (!Number.isFinite(strikeValue) || strikeValue <= 0) return null;
  if (leg !== 'C' && leg !== 'P') return null;

  return {
    base,
    currency: quote,
    expiryLabel,
    strike: strikeValue,
    type: leg === 'C' ? 'call' : 'put',
  };
}

/** Which Deribit currency bucket a contract name belongs to. */
function currencyOf(contract: string): string | null {
  const parsed = parseInstrument(contract);
  if (!parsed) return null;
  // `SOL_USDC-…` lives under USDC; `BTC-…` lives under BTC.
  return parsed.currency;
}

async function instruments(currency: string): Promise<RawInstrument[]> {
  const raw = await get<RawInstrument[]>(
    `/public/get_instruments?currency=${encodeURIComponent(currency)}&kind=option&expired=false`,
    TTL.catalogue,
  );
  return Array.isArray(raw) ? raw : [];
}

async function summaries(currency: string): Promise<RawSummary[]> {
  const raw = await get<RawSummary[]>(
    `/public/get_book_summary_by_currency?currency=${encodeURIComponent(currency)}&kind=option`,
    TTL.quote,
  );
  return Array.isArray(raw) ? raw : [];
}

async function indexPrice(indexName: string): Promise<number | null> {
  const raw = await get<{ index_price?: number }>(
    `/public/get_index_price?index_name=${encodeURIComponent(indexName)}`,
    TTL.quote,
  ).catch(() => null);
  return num(raw?.index_price ?? null);
}

/**
 * Every live contract on one underlying, normalised to USD per unit.
 *
 * Exported separately from the fetch so the normalisation can be tested against
 * a captured payload without a network — the conversion from an inverse mark in
 * BTC to a dollar price is exactly the kind of arithmetic that looks right and
 * is off by a factor of the index.
 */
export function buildBoard(
  symbol: string,
  raw: { instruments: RawInstrument[]; summaries: RawSummary[]; index: number | null },
): OptionBoard {
  const info = UNDERLYINGS[symbol]!;
  const bySummary = new Map(raw.summaries.map((s) => [s.instrument_name, s]));

  const live = raw.instruments.filter(
    (instrument) =>
      instrument.is_active !== false &&
      (instrument.base_currency ?? '').toUpperCase() === symbol &&
      instrument.option_type !== undefined &&
      Number.isFinite(instrument.strike) &&
      Number.isFinite(instrument.expiration_timestamp),
  );

  // Inverse marks are quoted in the coin; multiply through by the index once,
  // here, so nothing downstream has to remember which family it is holding.
  const index = raw.index;
  const scale = info.linear ? 1 : index;

  const quotes: BoardQuote[] = [];

  for (const instrument of live) {
    const summary = bySummary.get(instrument.instrument_name);
    if (!summary) continue;
    if (scale === null) continue;

    const type = instrument.option_type === 'put' ? 'put' : 'call';
    const markIv = num(summary.mark_iv);

    quotes.push({
      contract: instrument.instrument_name,
      type,
      strike: instrument.strike!,
      expiry: Math.floor(instrument.expiration_timestamp! / 1000),
      bid: scaled(quotePrice(summary.bid_price), scale),
      ask: scaled(quotePrice(summary.ask_price), scale),
      last: scaled(quotePrice(summary.last), scale),
      mark: scaled(quotePrice(summary.mark_price), scale),
      // `price_change` is a percentage move, not a price move.
      change: num(summary.price_change),
      volume: num(summary.volume),
      openInterest: num(summary.open_interest),
      // Deribit quotes volatility in percentage points.
      venueIv: markIv !== null && markIv > 0 ? markIv / 100 : null,
      // `underlying_price` is the synthetic future for this expiry — the
      // forward Deribit's own marks are struck against.
      venueForward: num(summary.underlying_price),
      venueDiscount: discountFrom(num(summary.underlying_price), index),
    });
  }

  const contractSize = live.find((i) => Number.isFinite(i.contract_size))?.contract_size ?? 1;

  const board: OptionBoard = {
    symbol,
    name: info.name,
    assetClass: 'crypto',
    currency: 'USD',
    venue: 'Deribit',
    source: 'deribit',
    contractSize,
    spot: index,
    quotes,
  };

  if (!info.linear) {
    board.note =
      `${symbol} options are coin-margined and quoted in ${symbol} on Deribit; ` +
      `prices here are converted to USD at the index (${fmt(index)}).`;
  }

  return board;
}

function scaled(value: number | null, scale: number | null): number | null {
  if (value === null || scale === null) return null;
  return value * scale;
}

function fmt(value: number | null): string {
  return value === null ? 'unavailable' : value.toLocaleString('en-US', { maximumFractionDigits: 2 });
}

/**
 * The discount factor for an expiry: `spot / forward`.
 *
 * Deribit does publish an `interest_rate` per instrument, and it reads `0.0` —
 * while its own forward for a ten-month expiry sits 3.9% above the index, which
 * is a 4.5% annualised rate. Trusting the field over the forward mispriced a
 * long-dated contract by that same 3.9% and left every delta on the board
 * disagreeing with Deribit's own.
 *
 * The forward is the market's statement of the carry, so it is what gets used.
 * A crypto option has no dividend, so all of that carry is interest and
 * `DF = S/F` follows exactly — which is also what makes the spot delta computed
 * here reproduce Deribit's published delta to five decimal places.
 */
function discountFrom(forward: number | null, spot: number | null): number | null {
  if (forward === null || spot === null || forward <= 0 || spot <= 0) return null;
  const discount = spot / forward;
  // A forward more than 50% from spot is a bad tick, not a basis.
  return discount > 0.5 && discount < 1.5 ? discount : null;
}

export async function getBoard(symbol: string): Promise<OptionBoard> {
  const resolved = resolveSymbol(symbol);
  if (!resolved) {
    throw new UpstreamError(`Deribit lists no options on ${symbol.toUpperCase()}`, {
      code: 'not_found',
      hint: `Crypto option boards exist for ${Object.keys(UNDERLYINGS).join(', ')}.`,
    });
  }

  const info = UNDERLYINGS[resolved]!;
  const [instrumentList, summaryList] = await Promise.all([
    instruments(info.currency),
    summaries(info.currency),
  ]);

  // Read the index name off the instrument rather than assembling it: Deribit
  // spells them `btc_usd` for inverse and `sol_usdc` for linear, and guessing
  // that mapping is a needless way to be wrong.
  const indexName =
    instrumentList.find(
      (i) => (i.base_currency ?? '').toUpperCase() === resolved && i.price_index,
    )?.price_index ?? `${resolved.toLowerCase()}_${info.linear ? 'usdc' : 'usd'}`;

  const index = await indexPrice(indexName);

  const board = buildBoard(resolved, {
    instruments: instrumentList,
    summaries: summaryList,
    index,
  });

  if (board.quotes.length === 0) {
    throw new UpstreamError(`Deribit returned no live ${resolved} option contracts`, {
      code: 'not_found',
      hint: 'The board may be between listings. Try again shortly.',
    });
  }

  return board;
}

/* ---------------------------------------------------------------- history */

/** Deribit's chart resolutions, keyed by this terminal's candle intervals. */
const RESOLUTION: Record<CandleInterval, string> = { 1: '1', 60: '60', 1440: '1D' };

export function normaliseChart(raw: RawChart): SpotCandle[] {
  const ticks = raw.ticks ?? [];
  const candles: SpotCandle[] = [];

  for (let i = 0; i < ticks.length; i++) {
    const time = ticks[i];
    const close = raw.close?.[i];
    if (time === undefined || close === undefined || !Number.isFinite(close)) continue;
    const open = raw.open?.[i] ?? close;
    candles.push({
      time: Math.floor(time / 1000),
      open,
      high: raw.high?.[i] ?? Math.max(open, close),
      low: raw.low?.[i] ?? Math.min(open, close),
      close,
      volume: raw.volume?.[i] ?? 0,
    });
  }

  return candles.sort((a, b) => a.time - b.time);
}

/**
 * Price history for one contract.
 *
 * Inverse contracts print in the coin, so the series is converted bar by bar
 * against the perpetual's own history rather than against today's index — using
 * the current index for a month-old bar would bake this week's spot move into
 * an option price that never traded there, which is precisely the artefact that
 * makes a converted chart worse than an unconverted one. The perpetual tracks
 * the index within a few basis points, which is invisible at chart scale.
 */
export async function getHistory(
  contract: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
): Promise<{ candles: SpotCandle[]; note?: string }> {
  const parsed = parseInstrument(contract);
  if (!parsed) {
    throw new UpstreamError(`"${contract}" is not a Deribit instrument name`, {
      code: 'bad_request',
      hint: 'Deribit contracts look like `BTC-25DEC26-104000-C`.',
    });
  }

  const path =
    `/public/get_tradingview_chart_data?instrument_name=${encodeURIComponent(contract)}` +
    `&start_timestamp=${Math.floor(startTs * 1000)}&end_timestamp=${Math.floor(endTs * 1000)}` +
    `&resolution=${RESOLUTION[interval]}`;

  const raw = await get<RawChart>(path, TTL.candles).catch(() => null);
  const candles = raw ? normaliseChart(raw) : [];
  if (candles.length === 0) return { candles: [] };

  // Linear contracts are already in dollars.
  const linear = parsed.currency !== parsed.base;
  if (linear) return { candles };

  const perpetual = await get<RawChart>(
    `/public/get_tradingview_chart_data?instrument_name=${encodeURIComponent(`${parsed.base}-PERPETUAL`)}` +
      `&start_timestamp=${Math.floor(startTs * 1000)}&end_timestamp=${Math.floor(endTs * 1000)}` +
      `&resolution=${RESOLUTION[interval]}`,
    TTL.candles,
  ).catch(() => null);

  const underlying = perpetual ? normaliseChart(perpetual) : [];
  if (underlying.length === 0) {
    return {
      candles,
      note: `Quoted in ${parsed.base} — Deribit's underlying history was unavailable to convert it to USD.`,
    };
  }

  const byTime = new Map(underlying.map((c) => [c.time, c]));
  const converted: SpotCandle[] = [];
  for (const candle of candles) {
    const reference = byTime.get(candle.time);
    if (!reference) continue;
    converted.push({
      time: candle.time,
      open: candle.open * reference.open,
      high: candle.high * reference.high,
      low: candle.low * reference.low,
      close: candle.close * reference.close,
      volume: candle.volume,
    });
  }

  return {
    candles: converted,
    note: `Converted from ${parsed.base} to USD bar-by-bar against ${parsed.base}-PERPETUAL.`,
  };
}

/** Which underlying a contract name belongs to, for routing a contract lookup. */
export function symbolOfContract(contract: string): string | null {
  const parsed = parseInstrument(contract);
  if (!parsed) return null;
  const currency = currencyOf(contract);
  if (currency === null) return null;
  return parsed.base in UNDERLYINGS ? parsed.base : null;
}
