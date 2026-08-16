/**
 * The option board, and everything derived from it.
 *
 * Two venues answer very different questions with very different payloads —
 * Deribit publishes a mark volatility and its own per-expiry forward, Nasdaq
 * publishes four prices and nothing else — so each source normalises into one
 * {@link OptionBoard} and every view is derived from that. The chain, the
 * volatility surface and the open-interest ladder are then the *same* numbers
 * seen three ways, rather than three pipelines that can disagree with each
 * other about what the delta of a contract is.
 *
 * The derivation is deliberately layered, because each step can fail
 * independently and a caller needs to know which one did:
 *
 *   1. **Forward.** The venue's, else fitted from put-call parity, else assumed
 *      from a configured rate. Recorded as `forwardSource` either way.
 *   2. **Volatility.** The venue's mark IV, else solved from the book mid.
 *   3. **Greeks.** Only once 1 and 2 both produced a number. A contract with no
 *      volatility gets `null` Greeks, never zeroes.
 */

import type {
  AssetClass,
  ForwardSource,
  OptionChain,
  OptionContract,
  OptionExpiry,
  OptionPositioning,
  OptionSmilePoint,
  OptionStrikeStat,
  OptionSurface,
  OptionTermPoint,
  OptionType,
} from '../../shared/types.js';
import {
  blackGreeks,
  blackPrice,
  breakeven,
  fitForward,
  impliedVol,
  intrinsicValue,
  maxPain,
  moneyness,
  yearsToExpiry,
} from '../../shared/greeks.js';
import { UpstreamError } from '../lib/http.js';

/**
 * Rate used only when nothing in the market will say what the carry is.
 *
 * It exists so a chain still renders when parity cannot be fitted — a thin
 * name, a one-sided board — and every response derived from it is stamped
 * `forwardSource: "assumed"` so the panel can tell the reader that the Greeks
 * rest on an assumption rather than on a quote.
 */
const ASSUMED_RATE = Number(process.env.OPTIONS_RATE ?? 0.04);

/** One live contract, normalised: prices per unit of underlying, in `currency`. */
export interface BoardQuote {
  contract: string;
  type: OptionType;
  strike: number;
  /** Expiry instant, unix seconds. */
  expiry: number;
  bid: number | null;
  ask: number | null;
  last: number | null;
  /** The venue's own mark, where it publishes one. */
  mark: number | null;
  change: number | null;
  volume: number | null;
  openInterest: number | null;
  /** The venue's own implied volatility, decimal. `null` when it publishes none. */
  venueIv: number | null;
  /** The venue's own forward for this expiry, decimal. `null` when it publishes none. */
  venueForward: number | null;
  /** The venue's own discount factor for this expiry. `null` when it publishes none. */
  venueDiscount: number | null;
}

export interface OptionBoard {
  symbol: string;
  name: string;
  assetClass: AssetClass;
  currency: string;
  venue: string;
  source: string;
  /** Underlying units per contract — 100 for US listed equity options, 1 on Deribit. */
  contractSize: number;
  spot: number | null;
  quotes: BoardQuote[];
  note?: string;
}

/* --------------------------------------------------------------- expiries */

/** Board → the expiry strip, soonest first. */
export function expiriesOf(board: OptionBoard, now = Date.now() / 1000): OptionExpiry[] {
  const byExpiry = new Map<number, { contracts: number; openInterest: number; volume: number }>();

  for (const quote of board.quotes) {
    const bucket = byExpiry.get(quote.expiry) ?? { contracts: 0, openInterest: 0, volume: 0 };
    bucket.contracts += 1;
    bucket.openInterest += quote.openInterest ?? 0;
    bucket.volume += quote.volume ?? 0;
    byExpiry.set(quote.expiry, bucket);
  }

  return [...byExpiry.entries()]
    .map(([expiry, bucket]) => ({
      expiry,
      date: new Date(expiry * 1000).toISOString().slice(0, 10),
      yearsToExpiry: yearsToExpiry(expiry, now),
      // Round rather than floor: an expiry 23h50m out is "tomorrow", not "today".
      daysToExpiry: Math.round((expiry - now) / 86400),
      ...bucket,
    }))
    .sort((a, b) => a.expiry - b.expiry);
}

/**
 * Pick the expiry a request means.
 *
 * `undefined` means "the front month", which is the first expiry that has not
 * already passed — not simply the first on the board, because a chain fetched
 * at 16:05 on expiry day still lists the contracts that stopped trading five
 * minutes ago and defaulting to them would show a board of frozen quotes.
 */
export function resolveExpiry(
  expiries: OptionExpiry[],
  wanted: string | undefined,
  now = Date.now() / 1000,
): OptionExpiry {
  if (expiries.length === 0) {
    throw new UpstreamError('No option expiries are listed for this underlying', {
      code: 'not_found',
      hint: 'The venue lists no live option board for this symbol.',
    });
  }

  const live = expiries.filter((e) => e.expiry > now);

  if (wanted === undefined || wanted === '') return live[0] ?? expiries[expiries.length - 1]!;

  const needle = wanted.trim().toLowerCase();

  // An exact date, or a unix instant.
  const exact = expiries.find((e) => e.date === needle || String(e.expiry) === needle);
  if (exact) return exact;

  // `30d` / `3m` — the listed expiry closest to that horizon, which is how a
  // board is actually navigated when the exact Friday is not memorised.
  const horizon = /^(\d+)\s*([dwmy])$/.exec(needle);
  if (horizon) {
    const [, count, unit] = horizon;
    const days =
      Number(count) * (unit === 'd' ? 1 : unit === 'w' ? 7 : unit === 'm' ? 30 : 365);
    const pool = live.length > 0 ? live : expiries;
    return pool.reduce((best, candidate) =>
      Math.abs(candidate.daysToExpiry - days) < Math.abs(best.daysToExpiry - days)
        ? candidate
        : best,
    );
  }

  // A bare index into the strip: `1` is the front expiry.
  const index = /^\d{1,2}$/.test(needle) ? Number(needle) : Number.NaN;
  const pool = live.length > 0 ? live : expiries;
  if (Number.isInteger(index) && index >= 1 && index <= pool.length) return pool[index - 1]!;

  throw new UpstreamError(`No listed expiry matches "${wanted}"`, {
    code: 'not_found',
    hint:
      `Listed expiries: ${expiries.slice(0, 12).map((e) => e.date).join(', ')}` +
      `${expiries.length > 12 ? ', …' : ''}. A horizon like \`30d\` picks the nearest one.`,
  });
}

/* ---------------------------------------------------------------- forwards */

export interface ExpiryCarry {
  forward: number | null;
  discount: number | null;
  rate: number | null;
  carry: number | null;
  source: ForwardSource;
}

/**
 * The forward and discount factor for one expiry.
 *
 * Order matters and is the point of the function: a venue that publishes its
 * own forward is authoritative — its marks are struck against that number, so
 * using anything else would produce Greeks inconsistent with the prices next to
 * them. Only when the venue is silent does parity get a turn, and only when
 * parity fails is anything assumed.
 */
export function carryFor(
  quotes: BoardQuote[],
  spot: number | null,
  years: number,
): ExpiryCarry {
  const none: ExpiryCarry = {
    forward: null,
    discount: null,
    rate: null,
    carry: null,
    source: 'assumed',
  };
  if (spot === null || !Number.isFinite(spot) || spot <= 0) return none;

  // 1. The venue's own forward.
  const venueForward = quotes.find(
    (q) => q.venueForward !== null && Number.isFinite(q.venueForward) && q.venueForward > 0,
  )?.venueForward;

  if (venueForward !== undefined && venueForward !== null) {
    const venueDiscount = quotes.find((q) => q.venueDiscount !== null)?.venueDiscount;
    const discount =
      venueDiscount !== undefined && venueDiscount !== null && venueDiscount > 0
        ? venueDiscount
        : 1;
    const rate = years > 0 ? -Math.log(discount) / years : 0;
    return {
      forward: venueForward,
      discount,
      rate,
      carry: years > 0 ? rate - Math.log(venueForward / spot) / years : 0,
      source: 'venue',
    };
  }

  // 2. Put-call parity, from the quoted book.
  const byStrike = new Map<number, { call?: number; put?: number }>();
  for (const quote of quotes) {
    const mid = midOf(quote);
    if (mid === null || mid <= 0) continue;
    const entry = byStrike.get(quote.strike) ?? {};
    entry[quote.type] = mid;
    byStrike.set(quote.strike, entry);
  }

  const pairs = [...byStrike.entries()]
    .filter(([, legs]) => legs.call !== undefined && legs.put !== undefined)
    .map(([strike, legs]) => ({ strike, callPrice: legs.call!, putPrice: legs.put! }));

  const fitted = fitForward(pairs, spot, years, 0.1, ASSUMED_RATE);
  if (fitted) {
    return {
      forward: fitted.forward,
      discount: fitted.discount,
      rate: fitted.rate,
      carry: fitted.carry,
      source: 'parity',
    };
  }

  // 3. Nothing in the market would say. Assume, and admit it.
  const discount = Math.exp(-ASSUMED_RATE * years);
  return {
    forward: spot / discount,
    discount,
    rate: ASSUMED_RATE,
    carry: 0,
    source: 'assumed',
  };
}

/**
 * Book mid, falling back to the venue's mark and then to the last print.
 *
 * A one-sided book is *not* halved the way a Kalshi binary is: an equity option
 * bid at 4.20 with no offer is worth about 4.20, whereas a binary bid at 1¢
 * with no offer is worth somewhere in [0, 1¢]. The bounded payoff is what makes
 * the binary case different, and options do not have one.
 */
export function midOf(quote: BoardQuote): number | null {
  const { bid, ask, mark, last } = quote;
  const hasBid = bid !== null && Number.isFinite(bid) && bid > 0;
  const hasAsk = ask !== null && Number.isFinite(ask) && ask > 0;

  if (hasBid && hasAsk) return (bid! + ask!) / 2;
  if (mark !== null && Number.isFinite(mark) && mark > 0) return mark;
  if (hasBid) return bid!;
  if (hasAsk) return ask!;
  if (last !== null && Number.isFinite(last) && last > 0) return last;
  return null;
}

/* ------------------------------------------------------------- enrichment */

/**
 * A board quote plus everything the model can say about it.
 *
 * Volatility comes from the venue when it publishes one, because that is the
 * number its own marks and its own risk system use, and solving a slightly
 * different one from a stale mid would put two disagreeing vols on one screen.
 */
export function enrich(
  quote: BoardQuote,
  spot: number | null,
  carry: ExpiryCarry,
  years: number,
): OptionContract {
  const mid = midOf(quote);
  const effectiveSpot = spot ?? carry.forward;

  const base: OptionContract = {
    contract: quote.contract,
    type: quote.type,
    strike: quote.strike,
    expiry: quote.expiry,
    bid: quote.bid,
    ask: quote.ask,
    mid,
    last: quote.last,
    change: quote.change,
    mark: quote.mark,
    volume: quote.volume,
    openInterest: quote.openInterest,
    iv: null,
    ivSource: null,
    greeks: { delta: null, gamma: null, vega: null, theta: null, rho: null },
    theo: null,
    intrinsic: effectiveSpot === null ? null : intrinsicValue(quote.type, quote.strike, effectiveSpot),
    extrinsic: null,
    breakeven: breakeven(quote.type, quote.strike, mid),
    inTheMoney:
      effectiveSpot === null
        ? false
        : quote.type === 'call'
          ? effectiveSpot > quote.strike
          : effectiveSpot < quote.strike,
  };

  if (base.intrinsic !== null && mid !== null) {
    // Can go slightly negative on a crossed or stale quote; that is real
    // information about the book, so it is not clamped away.
    base.extrinsic = mid - base.intrinsic;
  }

  if (carry.forward === null || carry.discount === null || effectiveSpot === null) return base;

  const pricing = {
    spot: effectiveSpot,
    forward: carry.forward,
    strike: quote.strike,
    years,
    discount: carry.discount,
    type: quote.type,
  };

  const iv =
    quote.venueIv !== null && Number.isFinite(quote.venueIv) && quote.venueIv > 0
      ? quote.venueIv
      : impliedVol(mid, pricing);

  if (iv === null) return base;

  base.iv = iv;
  base.ivSource = quote.venueIv !== null && quote.venueIv > 0 ? 'venue' : 'solved';
  base.greeks = blackGreeks({ ...pricing, vol: iv });
  base.theo = blackPrice({ ...pricing, vol: iv });
  return base;
}

/* -------------------------------------------------------------------- chain */

export function chainFor(
  board: OptionBoard,
  expiry: OptionExpiry,
  expiries: OptionExpiry[],
): OptionChain {
  const quotes = board.quotes.filter((q) => q.expiry === expiry.expiry);
  const carry = carryFor(quotes, board.spot, expiry.yearsToExpiry);

  const contracts = quotes.map((q) => enrich(q, board.spot, carry, expiry.yearsToExpiry));
  const calls = contracts.filter((c) => c.type === 'call').sort((a, b) => a.strike - b.strike);
  const puts = contracts.filter((c) => c.type === 'put').sort((a, b) => a.strike - b.strike);

  const chain: OptionChain = {
    symbol: board.symbol,
    name: board.name,
    assetClass: board.assetClass,
    currency: board.currency,
    spot: board.spot,
    forward: carry.forward,
    forwardSource: carry.source,
    discountFactor: carry.discount,
    rate: carry.rate,
    carry: carry.carry,
    expiry,
    expiries,
    calls,
    puts,
    atmIv: atmVol(contracts, carry.forward),
    contractSize: board.contractSize,
    venue: board.venue,
    source: board.source,
  };
  if (board.note) chain.note = board.note;
  return chain;
}

/**
 * At-the-money volatility: the smile interpolated at the forward.
 *
 * Reading the vol of the single nearest strike would make the number jump
 * whenever spot crossed a rung, which on a $5-strike board is several times a
 * session. Interpolating between the two strikes that bracket the forward moves
 * continuously, which is what makes a term structure readable.
 */
export function atmVol(contracts: OptionContract[], forward: number | null): number | null {
  if (forward === null || !Number.isFinite(forward)) return null;

  const rungs = smileRungs(contracts, forward)
    .filter((r) => r.iv !== null)
    .sort((a, b) => a.strike - b.strike);
  if (rungs.length === 0) return null;
  if (rungs.length === 1) return rungs[0]!.iv;

  const above = rungs.find((r) => r.strike >= forward);
  const below = [...rungs].reverse().find((r) => r.strike <= forward);

  if (!above || !below) return (above ?? below)!.iv;
  if (above.strike === below.strike) return above.iv;

  const weight = (forward - below.strike) / (above.strike - below.strike);
  return below.iv! + weight * (above.iv! - below.iv!);
}

interface Rung {
  strike: number;
  callIv: number | null;
  putIv: number | null;
  iv: number | null;
  volume: number;
  openInterest: number;
}

/**
 * Collapse both legs at each strike into one volatility.
 *
 * The out-of-the-money leg wins. In theory the two are equal — same strike,
 * same expiry, one distribution — and in practice the in-the-money leg is where
 * the spread is widest, the volume is thinnest and, on American equity options,
 * where the early-exercise premium sits. Averaging them drags the smile toward
 * whichever side is stale.
 */
function smileRungs(contracts: OptionContract[], forward: number | null): Rung[] {
  const byStrike = new Map<number, Rung>();

  for (const contract of contracts) {
    const rung = byStrike.get(contract.strike) ?? {
      strike: contract.strike,
      callIv: null,
      putIv: null,
      iv: null,
      volume: 0,
      openInterest: 0,
    };
    if (contract.type === 'call') rung.callIv = contract.iv;
    else rung.putIv = contract.iv;
    rung.volume += contract.volume ?? 0;
    rung.openInterest += contract.openInterest ?? 0;
    byStrike.set(contract.strike, rung);
  }

  for (const rung of byStrike.values()) {
    const outOfTheMoney =
      forward === null ? null : rung.strike >= forward ? rung.callIv : rung.putIv;
    rung.iv = outOfTheMoney ?? rung.callIv ?? rung.putIv;
  }

  return [...byStrike.values()].sort((a, b) => a.strike - b.strike);
}

/* ------------------------------------------------------------------ surface */

export function surfaceFor(
  board: OptionBoard,
  expiry: OptionExpiry,
  expiries: OptionExpiry[],
): OptionSurface {
  const quotes = board.quotes.filter((q) => q.expiry === expiry.expiry);
  const carry = carryFor(quotes, board.spot, expiry.yearsToExpiry);
  const contracts = quotes.map((q) => enrich(q, board.spot, carry, expiry.yearsToExpiry));

  const smile: OptionSmilePoint[] = smileRungs(contracts, carry.forward).map((rung) => ({
    strike: rung.strike,
    moneyness: board.spot === null ? null : moneyness(rung.strike, board.spot),
    callIv: rung.callIv,
    putIv: rung.putIv,
    iv: rung.iv,
    volume: rung.volume,
    openInterest: rung.openInterest,
  }));

  // The term structure is the same at-the-money calculation at every expiry, so
  // one board answers "is the front rich against the back?" without any extra
  // upstream traffic.
  const term: OptionTermPoint[] = expiries.map((candidate) => {
    const slice = board.quotes.filter((q) => q.expiry === candidate.expiry);
    const sliceCarry = carryFor(slice, board.spot, candidate.yearsToExpiry);
    const enriched = slice.map((q) => enrich(q, board.spot, sliceCarry, candidate.yearsToExpiry));
    return {
      expiry: candidate.expiry,
      date: candidate.date,
      daysToExpiry: candidate.daysToExpiry,
      atmIv: atmVol(enriched, sliceCarry.forward),
      forward: sliceCarry.forward,
      openInterest: candidate.openInterest,
      volume: candidate.volume,
    };
  });

  const surface: OptionSurface = {
    symbol: board.symbol,
    name: board.name,
    assetClass: board.assetClass,
    spot: board.spot,
    expiry,
    forward: carry.forward,
    atmIv: atmVol(contracts, carry.forward),
    smile,
    term,
    skew: riskReversal(contracts),
    venue: board.venue,
    source: board.source,
  };
  if (board.note) surface.note = board.note;
  return surface;
}

/**
 * 25-delta risk reversal: the put wing's volatility minus the call wing's.
 *
 * Positive means downside is bid — the shape of an equity index board almost
 * always, and the shape of a crypto board only when the market is frightened.
 * Quoted on delta rather than on strike so it is comparable across expiries and
 * across underlyings of wildly different price.
 */
export function riskReversal(contracts: OptionContract[], target = 0.25): number | null {
  const nearest = (type: OptionType, wanted: number): number | null => {
    let best: { distance: number; iv: number } | null = null;
    for (const contract of contracts) {
      if (contract.type !== type || contract.iv === null || contract.greeks.delta === null) continue;
      const distance = Math.abs(contract.greeks.delta - wanted);
      if (best === null || distance < best.distance) best = { distance, iv: contract.iv };
    }
    // A board whose closest contract to 25-delta is at 60-delta has no wing to
    // speak of, and quoting one would be inventing a number.
    return best !== null && best.distance < 0.12 ? best.iv : null;
  };

  const putIv = nearest('put', -target);
  const callIv = nearest('call', target);
  return putIv === null || callIv === null ? null : putIv - callIv;
}

/* -------------------------------------------------------------- positioning */

export function positioningFor(board: OptionBoard, expiry: OptionExpiry): OptionPositioning {
  const quotes = board.quotes.filter((q) => q.expiry === expiry.expiry);
  const byStrike = new Map<number, OptionStrikeStat>();

  for (const quote of quotes) {
    const stat = byStrike.get(quote.strike) ?? {
      strike: quote.strike,
      callOpenInterest: 0,
      putOpenInterest: 0,
      callVolume: 0,
      putVolume: 0,
      painPayout: null,
    };
    if (quote.type === 'call') {
      stat.callOpenInterest += quote.openInterest ?? 0;
      stat.callVolume += quote.volume ?? 0;
    } else {
      stat.putOpenInterest += quote.openInterest ?? 0;
      stat.putVolume += quote.volume ?? 0;
    }
    byStrike.set(quote.strike, stat);
  }

  const strikes = [...byStrike.values()].sort((a, b) => a.strike - b.strike);

  // The pain curve, not just its minimum: the shape is what shows whether the
  // low is a sharp pin or a flat basin the underlying could sit anywhere in.
  for (const stat of strikes) {
    let payout = 0;
    for (const other of strikes) {
      payout += Math.max(0, stat.strike - other.strike) * other.callOpenInterest;
      payout += Math.max(0, other.strike - stat.strike) * other.putOpenInterest;
    }
    stat.painPayout = payout * board.contractSize;
  }

  const totals = strikes.reduce(
    (acc, s) => ({
      callOpenInterest: acc.callOpenInterest + s.callOpenInterest,
      putOpenInterest: acc.putOpenInterest + s.putOpenInterest,
      callVolume: acc.callVolume + s.callVolume,
      putVolume: acc.putVolume + s.putVolume,
    }),
    { callOpenInterest: 0, putOpenInterest: 0, callVolume: 0, putVolume: 0 },
  );

  const pain = maxPain(strikes);

  const positioning: OptionPositioning = {
    symbol: board.symbol,
    name: board.name,
    assetClass: board.assetClass,
    spot: board.spot,
    expiry,
    strikes,
    maxPain: pain?.strike ?? null,
    totalCallOpenInterest: totals.callOpenInterest,
    totalPutOpenInterest: totals.putOpenInterest,
    totalCallVolume: totals.callVolume,
    totalPutVolume: totals.putVolume,
    putCallOpenInterest:
      totals.callOpenInterest > 0 ? totals.putOpenInterest / totals.callOpenInterest : null,
    putCallVolume: totals.callVolume > 0 ? totals.putVolume / totals.callVolume : null,
    venue: board.venue,
    source: board.source,
  };
  if (board.note) positioning.note = board.note;
  return positioning;
}
