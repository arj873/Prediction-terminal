/**
 * Kalshi strike ladders → an implied price for the underlying, live and
 * historical.
 *
 * The maths lives in `shared/implied.ts`; this module is the plumbing around
 * it — which ladders price a given symbol, which of their strikes are worth
 * reading, and how to turn N per-strike candle series into one price series.
 *
 * Two facts about Kalshi shape everything here:
 *
 * **The price ladders are not in the search corpus.** `sources/kalshi.ts` builds
 * its snapshot from the first 4,000 open events, and the crypto and index
 * ladders fall outside that window. So candidates are resolved by querying
 * `/events?series_ticker=` for a known set of series rather than by searching.
 *
 * **A ladder is the unit, not a contract.** One binary contract quotes one
 * probability; it takes a ladder of them to locate a price. That is why the
 * picker offers events and not markets.
 */

import type {
  AssetClass,
  CandleInterval,
  Candle,
  ImpliedCandidate,
  ImpliedCandidatesResponse,
  ImpliedMethod,
  ImpliedPoint,
  ImpliedSeriesResponse,
  KalshiEvent,
  Market,
} from '../../shared/types.js';
import { impliedPrice, legFromStrike, quoteProbability, type ImpliedLeg } from '../../shared/implied.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError } from '../lib/http.js';
import { getCandles, getEvent, listEvents } from './kalshi.js';

/* ---------------------------------------------------------------- registry */

export interface Underlying {
  /** Canonical symbol, as typed into the terminal. */
  symbol: string;
  name: string;
  assetClass: AssetClass;
  /** What the spot source is asked for — a Yahoo symbol or Coinbase product. */
  spotSymbol: string;
  /**
   * Kalshi series that quote this underlying's *settlement level*.
   *
   * Deliberately not "every series mentioning the asset". A running-maximum
   * product (`KXWTIMAX`, "how high will oil get this year") is a ladder over
   * the path's high, not over where it settles — collapsing one into an
   * implied price would produce a confidently wrong number. Only terminal-value
   * ladders belong here.
   *
   * Series that currently list no open events are harmless: they resolve to
   * nothing and are skipped. Keeping them listed means the terminal keeps
   * working as Kalshi rotates products in and out.
   */
  series: string[];
  aliases?: string[];
}

export const UNDERLYINGS: Underlying[] = [
  // --- crypto -------------------------------------------------------------
  { symbol: 'BTC', name: 'Bitcoin', assetClass: 'crypto', spotSymbol: 'BTC-USD', aliases: ['BITCOIN', 'XBT'], series: ['KXBTCD', 'KXBTC', 'KXBTCY', 'KXBTCW', 'KXBTCQ'] },
  { symbol: 'ETH', name: 'Ethereum', assetClass: 'crypto', spotSymbol: 'ETH-USD', aliases: ['ETHEREUM'], series: ['KXETHD', 'KXETH', 'KXETHY', 'KXETHW'] },
  { symbol: 'SOL', name: 'Solana', assetClass: 'crypto', spotSymbol: 'SOL-USD', aliases: ['SOLANA'], series: ['KXSOLD', 'KXSOLE', 'KXSOL'] },
  { symbol: 'XRP', name: 'XRP', assetClass: 'crypto', spotSymbol: 'XRP-USD', aliases: ['RIPPLE'], series: ['KXXRPD', 'KXXRP'] },
  { symbol: 'BNB', name: 'BNB', assetClass: 'crypto', spotSymbol: 'BNB-USD', series: ['KXBNBD', 'KXBNB', 'KXBNBY'] },
  { symbol: 'HYPE', name: 'Hyperliquid', assetClass: 'crypto', spotSymbol: 'HYPE-USD', series: ['KXHYPED', 'KXHYPE'] },
  { symbol: 'DOGE', name: 'Dogecoin', assetClass: 'crypto', spotSymbol: 'DOGE-USD', series: ['KXDOGED', 'KXDOGE'] },
  { symbol: 'ADA', name: 'Cardano', assetClass: 'crypto', spotSymbol: 'ADA-USD', series: ['KXADAD', 'KXADA'] },
  { symbol: 'AVAX', name: 'Avalanche', assetClass: 'crypto', spotSymbol: 'AVAX-USD', series: ['KXAVAXD', 'KXAVAX'] },
  { symbol: 'LINK', name: 'Chainlink', assetClass: 'crypto', spotSymbol: 'LINK-USD', series: ['KXLINKD', 'KXLINK'] },
  { symbol: 'LTC', name: 'Litecoin', assetClass: 'crypto', spotSymbol: 'LTC-USD', series: ['KXLTCD', 'KXLTC'] },
  { symbol: 'BCH', name: 'Bitcoin Cash', assetClass: 'crypto', spotSymbol: 'BCH-USD', series: ['KXBCHD', 'KXBCH'] },
  { symbol: 'DOT', name: 'Polkadot', assetClass: 'crypto', spotSymbol: 'DOT-USD', series: ['KXDOTD', 'KXDOT'] },
  { symbol: 'XLM', name: 'Stellar', assetClass: 'crypto', spotSymbol: 'XLM-USD', series: ['KXXLMD', 'KXXLM'] },
  { symbol: 'ZEC', name: 'Zcash', assetClass: 'crypto', spotSymbol: 'ZEC-USD', series: ['KXZECD', 'KXZEC'] },

  // --- indices, commodities and FX ---------------------------------------
  { symbol: '^GSPC', name: 'S&P 500', assetClass: 'stock', spotSymbol: '^GSPC', aliases: ['SPX', 'SP500', 'INX', 'GSPC'], series: ['KXINXU', 'KXINX', 'KXINXY'] },
  { symbol: '^NDX', name: 'Nasdaq-100', assetClass: 'stock', spotSymbol: '^NDX', aliases: ['NDX', 'NASDAQ100', 'NASDAQ'], series: ['KXNASDAQ100U', 'KXNASDAQ100', 'KXNASDAQ100Y'] },
  { symbol: '^DJI', name: 'Dow Jones Industrial Average', assetClass: 'stock', spotSymbol: '^DJI', aliases: ['DJI', 'DJIA', 'DOW'], series: ['KXDJI'] },
  { symbol: 'GC=F', name: 'Gold', assetClass: 'stock', spotSymbol: 'GC=F', aliases: ['GOLD', 'XAU'], series: ['KXGOLDW', 'KXGOLD', 'KXGOLDY'] },
  { symbol: 'CL=F', name: 'WTI Crude Oil', assetClass: 'stock', spotSymbol: 'CL=F', aliases: ['WTI', 'OIL', 'CRUDE'], series: ['KXWTI', 'KXWTIW'] },
  { symbol: 'EURUSD=X', name: 'EUR / USD', assetClass: 'stock', spotSymbol: 'EURUSD=X', aliases: ['EURUSD', 'EUR'], series: ['KXEURUSD'] },
  { symbol: 'USDJPY=X', name: 'USD / JPY', assetClass: 'stock', spotSymbol: 'USDJPY=X', aliases: ['USDJPY', 'JPY'], series: ['KXUSDJPY'] },
];

const BY_KEY = new Map<string, Underlying>();
for (const underlying of UNDERLYINGS) {
  BY_KEY.set(underlying.symbol.toUpperCase(), underlying);
  for (const alias of underlying.aliases ?? []) BY_KEY.set(alias.toUpperCase(), underlying);
}

/** Resolve a typed symbol to a registry entry, or `undefined` if unknown. */
export function findUnderlying(symbol: string): Underlying | undefined {
  return BY_KEY.get(symbol.trim().toUpperCase());
}

/** Every symbol the implied overlay can price, for `HELP` and the picker. */
export function listUnderlyings(): Underlying[] {
  return UNDERLYINGS;
}

/* ------------------------------------------------------------- ladder legs */

/** Contracts with a numeric strike. Everything else cannot locate a price. */
function ladderMarkets(event: KalshiEvent): Market[] {
  return event.markets.filter(
    (m) => m.strikeType !== null && (m.floorStrike !== null || m.capStrike !== null),
  );
}

/** A ladder needs enough rungs to bracket a crossing. Three is the floor. */
const MIN_STRIKES = 3;

/** Build legs from the live book. */
function liveLegs(markets: Market[]): ImpliedLeg[] {
  const legs: ImpliedLeg[] = [];
  for (const market of markets) {
    const p = quoteProbability(market.yesBid, market.yesAsk, market.lastPrice);
    if (p === null) continue;
    const leg = legFromStrike(market.strikeType, market.floorStrike, market.capStrike, p, market.ticker);
    if (leg) legs.push(leg);
  }
  return legs;
}

/* --------------------------------------------------------------- discovery */

/**
 * Ladders that price `symbol`, newest expiry first, each with its live implied
 * price already computed.
 *
 * Computing the implied price here rather than on selection is the point: the
 * picker can show what each ladder currently implies, so choosing between four
 * expiries is an informed choice rather than a guess.
 */
export async function getCandidates(symbol: string): Promise<ImpliedCandidatesResponse> {
  const underlying = findUnderlying(symbol);

  // An unmapped symbol is an empty result, not an error: the endpoint exists,
  // the question is well-formed, and the answer is "none". Answering 404 here
  // made every plain stock chart log a failed request while rendering fine.
  if (!underlying) {
    return {
      symbol: symbol.trim().toUpperCase(),
      assetClass: 'stock',
      name: symbol.trim().toUpperCase(),
      candidates: [],
      note:
        `Kalshi lists price ladders for major crypto, the index complex, gold, oil ` +
        `and two FX pairs — not for individual equities. Known symbols: ` +
        `${UNDERLYINGS.map((u) => u.symbol).join(', ')}.`,
    };
  }

  const candidates = await cache.cached(
    `implied:candidates:${underlying.symbol}`,
    TTL.quote,
    async () => {
      // One request per series, run together. A series with no open events
      // resolves to nothing, which is the normal state for the rotated-out ones.
      const perSeries = await Promise.all(
        underlying.series.map((series) =>
          listEvents({ seriesTicker: series, status: 'open', withNestedMarkets: true, limit: 200 })
            .then((response) => response.events)
            .catch(() => [] as KalshiEvent[]),
        ),
      );

      const found: ImpliedCandidate[] = [];
      for (const event of perSeries.flat()) {
        const markets = ladderMarkets(event);
        if (markets.length < MIN_STRIKES) continue;
        found.push(describeCandidate(event, markets));
      }

      // Soonest expiry first: that is the ladder most tightly anchored to spot,
      // and the one a reader almost always wants as their first overlay.
      found.sort(
        (a, b) =>
          (a.strikeDate || '9999').localeCompare(b.strikeDate || '9999') ||
          b.volume24h - a.volume24h,
      );
      return found;
    },
  );

  return {
    symbol: underlying.symbol,
    assetClass: underlying.assetClass,
    name: underlying.name,
    candidates,
    ...(candidates.length === 0
      ? {
          note:
            `Kalshi maps ${underlying.series.length} ladder series to ${underlying.name}, ` +
            `but none has an open event right now.`,
        }
      : {}),
  };
}

function describeCandidate(event: KalshiEvent, markets: Market[]): ImpliedCandidate {
  const legs = liveLegs(markets);
  const result = impliedPrice(legs, 'median');

  const strikes = markets
    .flatMap((m) => [m.floorStrike, m.capStrike])
    .filter((s): s is number => s !== null);

  return {
    eventTicker: event.eventTicker,
    seriesTicker: event.seriesTicker,
    title: event.title,
    subTitle: event.subTitle,
    strikeDate: firstCloseTime(markets),
    strikes: markets.length,
    quoted: legs.length,
    volume24h: markets.reduce((sum, m) => sum + m.volume24h, 0),
    strikeLow: strikes.length ? Math.min(...strikes) : null,
    strikeHigh: strikes.length ? Math.max(...strikes) : null,
    implied: result.value,
    tailMass: result.knots.length ? result.tailMass : null,
  };
}

/** Every rung of a ladder closes together, so the first close time is the expiry. */
function firstCloseTime(markets: Market[]): string {
  for (const market of markets) if (market.closeTime) return market.closeTime;
  return '';
}

/** An ISO close time as epoch seconds, or `undefined` if Kalshi gave none. */
function epochSeconds(iso: string): number | undefined {
  const ms = Date.parse(iso);
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : undefined;
}

/* --------------------------------------------------------------- historical */

/**
 * Cap on strikes read per ladder.
 *
 * Some ladders run to 400 rungs. Reading all of them would mean 400 upstream
 * candle requests for one overlay line, and the far wings contribute almost
 * nothing — a strike quoted at 99¢ or 1¢ moves the 50% crossing not at all.
 * The strikes that matter are the ones near the money, which is what
 * {@link selectStrikes} keeps.
 */
const MAX_STRIKES = 48;

/** Parallel candle requests in flight. Enough to be quick, not enough to flood. */
const FAN_OUT = 6;

/**
 * Choose which rungs to read.
 *
 * Preference order is: has ever been quoted (open interest or volume), then
 * proximity to the live implied price. Ordering by proximity rather than by
 * volume matters — volume clusters at round numbers, but the 50% crossing is
 * located by the strikes bracketing it, whichever they are.
 */
export function selectStrikes(markets: Market[], centre: number | null): Market[] {
  const active = markets.filter((m) => m.openInterest > 0 || m.volume > 0);
  const pool = active.length >= MIN_STRIKES ? active : markets;
  if (pool.length <= MAX_STRIKES) return pool;

  const midpoint = (m: Market): number => {
    const lo = m.floorStrike;
    const hi = m.capStrike;
    if (lo !== null && hi !== null) return (lo + hi) / 2;
    return lo ?? hi ?? 0;
  };

  const anchor = centre ?? median(pool.map(midpoint));
  return [...pool]
    .sort((a, b) => Math.abs(midpoint(a) - anchor) - Math.abs(midpoint(b) - anchor))
    .slice(0, MAX_STRIKES);
}

function median(values: number[]): number {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0 ? ((sorted[mid - 1] ?? 0) + (sorted[mid] ?? 0)) / 2 : (sorted[mid] ?? 0);
}

/** Run `worker` over `items` with bounded concurrency, preserving order. */
async function mapLimit<T, R>(items: T[], limit: number, worker: (item: T) => Promise<R>): Promise<R[]> {
  const results = new Array<R>(items.length);
  let next = 0;

  const runners = Array.from({ length: Math.min(limit, items.length) }, async () => {
    for (;;) {
      const index = next++;
      if (index >= items.length) return;
      results[index] = await worker(items[index]!);
    }
  });

  await Promise.all(runners);
  return results;
}

/**
 * The implied price of the underlying through time, from one ladder.
 *
 * Every rung is read as its own candle series, then the ladder is re-priced at
 * each bucket on the union of their timestamps. Within a bucket a rung that has
 * no candle yet contributes nothing, and one that has stopped printing carries
 * its last quote forward — which is what a resting book actually does, and the
 * alternative (dropping it) would silently shrink the ladder and drag the
 * crossing around.
 */
export async function getImpliedSeries(
  eventTicker: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
  method: ImpliedMethod = 'median',
): Promise<ImpliedSeriesResponse> {
  // One overlay costs up to MAX_STRIKES candle requests, and panels poll. The
  // per-market candle cache alone would still re-run the whole fan-out on every
  // tick, so the assembled series is cached as a unit. The route quantises
  // `end`, which is what makes this key stable between two ticks a second apart.
  return cache.cached(
    `implied:series:${eventTicker}:${interval}:${startTs}:${endTs}:${method}`,
    TTL.candles,
    () => computeImpliedSeries(eventTicker, interval, startTs, endTs, method),
  );
}

async function computeImpliedSeries(
  eventTicker: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
  method: ImpliedMethod,
): Promise<ImpliedSeriesResponse> {
  const event = await getEvent(eventTicker);
  const markets = ladderMarkets(event);

  if (markets.length < MIN_STRIKES) {
    throw new UpstreamError(`${eventTicker} is not a price ladder`, {
      code: 'bad_request',
      hint:
        `An implied price needs at least ${MIN_STRIKES} contracts with numeric strikes ` +
        `over one underlying. This event has ${markets.length}.`,
    });
  }

  // Anchor the strike selection on where the ladder currently prices the
  // underlying, so the rungs read are the ones bracketing the crossing.
  const live = impliedPrice(liveLegs(markets), method);
  const selected = selectStrikes(markets, live.value);

  const series = await mapLimit(selected, FAN_OUT, async (market) => {
    const candles = await getCandles(
      market.ticker,
      interval,
      startTs,
      endTs,
      event.seriesTicker,
    ).catch(() => null);
    return { market, candles: candles?.candles ?? [] };
  });

  const strikeDate = firstCloseTime(markets);
  const contributing = series.filter((s) => s.candles.length > 0);
  const points = buildPoints(contributing, method, epochSeconds(strikeDate));

  return {
    eventTicker: event.eventTicker,
    title: event.title,
    strikeDate,
    method,
    interval,
    contributors: contributing.map((s) => s.market.ticker),
    skipped: selected.length - contributing.length,
    points,
  };
}

/**
 * Re-price the ladder at every bucket on the union timeline.
 *
 * A pointer per rung walks its candles forward as the timeline advances, so
 * this stays linear in total candles rather than quadratic — a 48-strike ladder
 * over a year of hourly bars is otherwise a few hundred million comparisons.
 *
 * `closeTs` ends the line at the ladder's expiry. Kalshi keeps printing candles
 * after settlement, and a settled ladder is no longer a distribution over where
 * price *might* land — every rung is worth exactly 0 or 1, most books empty
 * out, and what survives collapses the crossing onto an arbitrary strike. On a
 * settled S&P ladder that phantom bucket read 7575 against a 7785.76 close, and
 * because it is the last point it is both the end of the drawn line and the
 * number the legend reports. A ladder implies nothing after it settles.
 */
export function buildPoints(
  series: { market: Market; candles: Candle[] }[],
  method: ImpliedMethod,
  closeTs?: number,
): ImpliedPoint[] {
  const timeline = [...new Set(series.flatMap((s) => s.candles.map((c) => c.time)))]
    .filter((time) => closeTs === undefined || time <= closeTs)
    .sort((a, b) => a - b);

  const cursors = series.map(() => ({ index: -1 }));
  const points: ImpliedPoint[] = [];

  for (const time of timeline) {
    const legs: ImpliedLeg[] = [];

    for (let i = 0; i < series.length; i++) {
      const entry = series[i]!;
      const cursor = cursors[i]!;

      // Advance to the newest candle at or before `time`; never past it, so a
      // rung cannot be priced with information it did not yet have.
      while (
        cursor.index + 1 < entry.candles.length &&
        entry.candles[cursor.index + 1]!.time <= time
      ) {
        cursor.index++;
      }
      if (cursor.index < 0) continue;

      const candle = entry.candles[cursor.index]!;
      const p = quoteProbability(candle.bid, candle.ask, candle.close);
      if (p === null) continue;

      const leg = legFromStrike(
        entry.market.strikeType,
        entry.market.floorStrike,
        entry.market.capStrike,
        p,
        entry.market.ticker,
      );
      if (leg) legs.push(leg);
    }

    if (legs.length < MIN_STRIKES) {
      points.push({ time, value: null, strikes: legs.length, tailMass: 1 });
      continue;
    }

    const result = impliedPrice(legs, method);
    points.push({
      time,
      value: result.value,
      strikes: legs.length,
      tailMass: result.tailMass,
    });
  }

  return points;
}
