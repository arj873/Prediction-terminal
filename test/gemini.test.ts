/**
 * Gemini normalisation tests.
 *
 * The fixtures are trimmed copies of real responses from both hosts, so the
 * decimal-string prices, the Contentful rules documents and the strike objects
 * with label debris in them are exactly what the exchange sends — including the
 * `prices.buy`/`prices.sell` pair that collapses to an indicative mark when
 * nothing rests, which the normaliser deliberately ignores.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  applyBook,
  applyTicker,
  flattenRules,
  normaliseCandles,
  normaliseEvent,
  normaliseMarket,
  normaliseOrderBook,
  normaliseTrades,
  num,
  previousFrom,
  seriesFromEvent,
  statusOf,
  strikeOf,
  type GeminiParent,
  type RawGeminiContract,
  type RawGeminiEvent,
  type RawGeminiTrade,
} from '../src/server/sources/gemini.js';

/* ---------------------------------------------------------------- coercion */

describe('num', () => {
  it('reads the decimal strings every figure arrives as', () => {
    assert.equal(num('0.19'), 0.19);
    assert.equal(num('7804'), 7804);
    assert.equal(num('3801.44'), 3801.44);
  });

  it('reads a plain number, as the v2 candle rows send', () => {
    assert.equal(num(5467), 5467);
  });

  it('returns null for absent or unparseable values', () => {
    assert.equal(num(undefined), null);
    assert.equal(num(null), null);
    assert.equal(num(''), null);
    assert.equal(num('CKHEE.'), null);
  });

  it('keeps a real zero — a bar with nothing printed in it says so', () => {
    assert.equal(num(0), 0);
    assert.equal(num('0'), 0);
  });
});

/* ----------------------------------------------------------------- markets */

const PARENT: GeminiParent = {
  eventTicker: 'DEMNOM2028',
  seriesTicker: 'DEMNOM',
  title: '2028 Democratic Nominee for President?',
  category: 'Politics',
  marketType: 'categorical',
  openTime: '2026-02-19T22:02:11.412Z',
  closeTime: '2028-11-10T00:00:00.000Z',
  volume: null,
  volume24h: null,
};

// The AOC leg of DEMNOM2028, verbatim but for a shortened rules document.
const AOC: RawGeminiContract = {
  id: '1889-10934',
  label: 'Alexandria Ocasio-Cortez',
  abbreviatedName: 'AOC',
  ticker: 'DEMNOM28AOC',
  instrumentSymbol: 'GEMI-DEMNOM2028-DEMNOM28AOC',
  description: {
    content: [
      { value: 'This market will resolve to “Yes” if Alexandria Ocasio-Cortez wins ' },
      { value: 'and formally accepts the 2028 Democratic Party nomination.' },
    ],
  },
  prices: {
    buy: { yes: '0.2', no: '0.81' },
    sell: { yes: '0.19', no: '0.8' },
    bestBid: '0.19',
    bestAsk: '0.2',
    lastTradePrice: '0.22',
  },
  status: 'active',
  marketState: 'open',
  effectiveDate: '2026-02-20T14:00:00.000Z',
  expiryDate: '2028-11-10T00:00:00.000Z',
  priceDelta24hPct: '10',
  priceDelta1hPct: '0',
};

describe('normaliseMarket', () => {
  it('quotes from bestBid/bestAsk and coerces the dollar strings to numbers', () => {
    const market = normaliseMarket(AOC, PARENT);
    assert.equal(market.venue, 'gemini');
    assert.equal(market.yesBid, 0.19);
    assert.equal(market.yesAsk, 0.2);
    assert.equal(market.mid, 0.195);
    assert.equal(market.lastPrice, 0.22);
  });

  it('carries the instrument symbol as the ticker, so getMarket can round-trip it', () => {
    assert.equal(normaliseMarket(AOC, PARENT).ticker, 'GEMI-DEMNOM2028-DEMNOM28AOC');
    assert.equal(normaliseMarket(AOC, PARENT).eventTicker, 'DEMNOM2028');
  });

  it('reads the symbol the exchange states rather than rebuilding it from the tickers', () => {
    // Four of 2,995 symbols do not contain their own contract ticker.
    const odd = normaliseMarket(
      { ...AOC, ticker: 'TUCKERFAVREAU', instrumentSymbol: 'GEMI-SEN26ME-TROYJACKSON' },
      PARENT,
    );
    assert.equal(odd.ticker, 'GEMI-SEN26ME-TROYJACKSON');
  });

  it('derives the NO side from the YES book', () => {
    const market = normaliseMarket(AOC, PARENT);
    assert.equal(market.noBid, 0.8);
    assert.equal(market.noAsk, 0.81);
  });

  it('derives a NO side matching the venue’s own buy.no and sell.no exactly', () => {
    // The exchange publishes both; they agreed to the cent on every contract
    // that carries the pair, which is why the mirror is safe to use.
    const market = normaliseMarket(AOC, PARENT);
    assert.equal(market.noBid, Number(AOC.prices?.sell?.no));
    assert.equal(market.noAsk, Number(AOC.prices?.buy?.no));
  });

  it('ignores prices.buy/prices.sell, which hold a mark when nothing rests', () => {
    // A real row from NOBELPEACE26: no bestBid/bestAsk at all, and buy.yes ===
    // sell.yes === lastTradePrice. Reading them would print a two-sided quote
    // at a zero spread on a contract nobody is quoting.
    const unquoted = normaliseMarket(
      {
        ...AOC,
        instrumentSymbol: 'GEMI-NOBELPEACE26-AMODEI',
        prices: {
          buy: { yes: '0.95', no: '0.05' },
          sell: { yes: '0.95', no: '0.05' },
          lastTradePrice: '0.95',
        },
        priceDelta24hPct: undefined,
      },
      PARENT,
    );
    assert.equal(unquoted.yesBid, null);
    assert.equal(unquoted.yesAsk, null);
    assert.equal(unquoted.noBid, null);
    assert.equal(unquoted.noAsk, null);
    // The print is still a fact, and the mid falls back to it.
    assert.equal(unquoted.lastPrice, 0.95);
    assert.equal(unquoted.mid, 0.95);
  });

  it('reports a settled contract’s emptied prices as absent, not as zero quotes', () => {
    // A settled leg's prices collapse to {"buy":{},"sell":{}} — the bestBid,
    // bestAsk and lastTradePrice keys disappear entirely.
    const settled = normaliseMarket(
      {
        label: 'Alexandra Eala',
        instrumentSymbol: 'GEMI-TENNIS-WTACIN-20260817-EAL-ANI-EAL',
        prices: { buy: {}, sell: {} },
        status: 'settled',
        marketState: 'closed',
        resolutionSide: 'no',
      },
      PARENT,
    );
    assert.equal(settled.yesBid, null);
    assert.equal(settled.yesAsk, null);
    assert.equal(settled.lastPrice, null);
    assert.equal(settled.mid, null);
    assert.equal(settled.change, null);
  });

  it('states only the side that exists when the book is one-sided', () => {
    // The mirror is per side: an offer nobody has made cannot become a NO bid.
    const oneSided = normaliseMarket(
      { ...AOC, prices: { buy: {}, sell: {}, bestBid: '0.19', lastTradePrice: '0.22' } },
      PARENT,
    );
    assert.equal(oneSided.yesBid, 0.19);
    assert.equal(oneSided.yesAsk, null);
    assert.equal(oneSided.noBid, null);
    assert.equal(oneSided.noAsk, 0.81);
    // No two-sided quote to take a midpoint of, so the last print stands in.
    assert.equal(oneSided.mid, 0.22);
  });

  it('leaves turnover, open interest and depth unstated on a leg of a multi-leg event', () => {
    // Gemini states turnover per event only, and never publishes open interest
    // or a liquidity figure — a 0 here would read as a dead market.
    const market = normaliseMarket(AOC, PARENT);
    assert.equal(market.volume, null);
    assert.equal(market.volume24h, null);
    assert.equal(market.openInterest, null);
    assert.equal(market.liquidity, null);
  });

  it('derives the move in dollars from the percentage the venue states', () => {
    // last 0.22 at +10% over 24h → 0.20 a day ago, a two-cent move. Assigning
    // the percentage to `change` would print a ten-dollar one.
    const market = normaliseMarket(AOC, PARENT);
    assert.equal(market.previousPrice, 0.2);
    assert.equal(market.change, 0.02);
  });

  it('states no move on the contracts that carry no percentage', () => {
    const market = normaliseMarket({ ...AOC, priceDelta24hPct: undefined }, PARENT);
    assert.equal(market.previousPrice, null);
    assert.equal(market.change, null);
  });

  it('maps the settlement side onto the result, per leg', () => {
    assert.equal(normaliseMarket({ ...AOC, resolutionSide: 'yes' }, PARENT).result, 'yes');
    assert.equal(normaliseMarket({ ...AOC, resolutionSide: 'no' }, PARENT).result, 'no');
    assert.equal(normaliseMarket(AOC, PARENT).result, '');
  });

  it('flattens the rich-text rules document', () => {
    const market = normaliseMarket(AOC, PARENT);
    assert.match(market.rulesPrimary, /^This market will resolve to “Yes”/);
    assert.match(market.rulesPrimary, /formally accepts the 2028 Democratic Party nomination\.$/);
  });

  it('labels the YES leg from the contract and the NO leg from nothing', () => {
    const market = normaliseMarket(AOC, PARENT);
    assert.equal(market.yesSubTitle, 'Alexandria Ocasio-Cortez');
    assert.equal(market.noSubTitle, 'No');
    assert.equal(market.title, '2028 Democratic Nominee for President?');
  });

  it('opens at the leg’s own effective date, not the event’s start time', () => {
    // An event's startTime is the kickoff — when trading stops.
    const market = normaliseMarket(AOC, PARENT);
    assert.equal(market.openTime, '2026-02-20T14:00:00.000Z');
    assert.equal(market.closeTime, '2028-11-10T00:00:00.000Z');
    assert.equal(market.expirationTime, '2028-11-10T00:00:00.000Z');
  });
});

describe('statusOf', () => {
  it('maps the venue’s two live states onto the terminal’s', () => {
    assert.equal(statusOf({ status: 'active', marketState: 'open' }), 'open');
    assert.equal(statusOf({ status: 'settled', marketState: 'closed' }), 'settled');
  });

  it('falls through to marketState when the contract states no status', () => {
    assert.equal(statusOf({ marketState: 'closed' }), 'settled');
  });

  it('folds the case the exchange happens to send', () => {
    assert.equal(statusOf({ status: 'Active' }), 'open');
    assert.equal(statusOf({ marketState: 'CLOSED' }), 'settled');
  });

  it('passes an unseen state through in the venue’s own word', () => {
    // The exchange's validator names `under_review`; nothing in the snapshot
    // used it, and guessing which of ours it resembles would be worse.
    assert.equal(statusOf({ status: 'under_review' }), 'under_review');
  });
});

describe('previousFrom', () => {
  it('inverts the percentage into the older price', () => {
    assert.equal(previousFrom(0.22, '10'), 0.2);
    assert.equal(previousFrom(0.71, '-4.05'), 0.74);
  });

  it('states nothing when either half is missing', () => {
    assert.equal(previousFrom(null, '10'), null);
    assert.equal(previousFrom(0.22, undefined), null);
  });

  it('keeps a flat market flat rather than dropping it', () => {
    assert.equal(previousFrom(0.98, '0'), 0.98);
  });

  it('reads a percentage the exchange sends as a string, sign and all', () => {
    assert.equal(previousFrom(0.98, '38.03'), 0.71);
    assert.equal(previousFrom(0.2, '-80'), 1);
  });

  it('states no older price for a contract that lost all of it', () => {
    // -100% inverts to a division by zero; an Infinity would chart as a spike.
    assert.equal(previousFrom(0.01, '-100'), null);
  });
});

/* ----------------------------------------------------------------- strikes */

describe('strikeOf', () => {
  it('reads `above` as inclusive, because the rung pays at its own strike', () => {
    // Every numeric `above` strike is labelled "$61,000 or above", and its terms
    // read "if the price of Bitcoin is $61,000 or above … resolve to Yes".
    assert.deepEqual(strikeOf({ type: 'above', value: '61000' }), {
      strikeType: 'greater_or_equal',
      floorStrike: 61000,
      capStrike: null,
    });
  });

  it('reads `below` and `under_or_equal` as the one inclusive upper bound', () => {
    // The venue spells the same relation two ways: every numeric `below` is a
    // "74°F or below" weather bucket, and `under_or_equal` is a top-10 finish
    // "including ties". Strict bounds would leave a gap at every tick.
    assert.deepEqual(strikeOf({ type: 'below', value: '74' }), {
      strikeType: 'less_or_equal',
      floorStrike: null,
      capStrike: 74,
    });
    assert.deepEqual(strikeOf({ type: 'under_or_equal', value: '10' }), {
      strikeType: 'less_or_equal',
      floorStrike: null,
      capStrike: 10,
    });
  });

  it('reads `reference` as no strike — it is a level quoted against, not a bound', () => {
    assert.deepEqual(strikeOf({ type: 'reference', value: '64182.97' }), {
      strikeType: null,
      floorStrike: null,
      capStrike: null,
    });
  });

  it('rejects the 27 strikes carrying label debris instead of a figure', () => {
    // "Lockheed Martin" and "Hike 25bps" arrive as strikes. A NaN bound would
    // sort to one end of every ladder it appeared in.
    assert.equal(strikeOf({ type: 'below', value: 'CKHEE.' }).strikeType, null);
    assert.equal(strikeOf({ type: 'above', value: 'KE25' }).floorStrike, null);
    assert.equal(strikeOf(undefined).strikeType, null);
  });

  it('puts the strike onto the market it belongs to', () => {
    const ladder = normaliseMarket(
      {
        label: '$61,000 or above',
        instrumentSymbol: 'GEMI-BTC2608212100-HI61000',
        prices: { bestBid: '0.97', bestAsk: '0.98', lastTradePrice: '0.83' },
        strike: { type: 'above', value: '61000' },
      },
      PARENT,
    );
    assert.equal(ladder.strikeType, 'greater_or_equal');
    assert.equal(ladder.floorStrike, 61000);
    assert.equal(ladder.capStrike, null);
  });
});

/* ------------------------------------------------------------------ series */

describe('seriesFromEvent', () => {
  it('prefers the series the exchange states, because the ticker fuses products', () => {
    // The hourly bitcoin ladder and the daily one strip to the same key.
    assert.equal(seriesFromEvent({ ticker: 'BTC2608212100', series: 'BTC1H' }), 'BTC1H');
    assert.equal(seriesFromEvent({ ticker: 'BTC2608180600', series: 'BTC' }), 'BTC');
  });

  it('keeps the city when the stated series drops it, because that is a question each', () => {
    // The venue files all seventeen daily-high weather books under one WXHIGH
    // product. A series here is a recurring question, and the high in Miami is
    // not the high in Los Angeles — grouped, sixteen of the seventeen become
    // unreachable from `XV`, since one event has to stand for the family.
    assert.equal(
      seriesFromEvent({ ticker: 'WXHIGH-LA-2608190359', series: 'WXHIGH' }),
      'WXHIGH-LA',
    );
    assert.equal(
      seriesFromEvent({ ticker: 'WXHIGH-MIA-2608190359', series: 'WXHIGH' }),
      'WXHIGH-MIA',
    );
  });

  it('keeps the stated series where it says more than the ticker does', () => {
    // An hourly bitcoin ladder and a daily one strip to the same `BTC`, and
    // pairing the merged series against one market at the next broker is
    // exactly the mistake the stated name exists to prevent.
    assert.equal(seriesFromEvent({ ticker: 'BTC2608212100', series: 'BTC1H' }), 'BTC1H');
    assert.equal(seriesFromEvent({ ticker: 'BTC2608180600', series: 'BTC' }), 'BTC');
  });

  it('strips the settlement date where no series is stated', () => {
    assert.equal(seriesFromEvent({ ticker: 'FED260917' }), 'FED');
    assert.equal(seriesFromEvent({ ticker: 'FED260729' }), 'FED');
    assert.equal(seriesFromEvent({ ticker: 'WXHIGH-LA-2608190359' }), 'WXHIGH-LA');
    assert.equal(seriesFromEvent({ ticker: 'NFL-2608212330-CAR-JAX-M' }), 'NFL-CAR-JAX-M');
    assert.equal(
      seriesFromEvent({ ticker: 'TENNIS-ATPCIN-20260818-FAR-WAL' }),
      'TENNIS-ATPCIN-FAR-WAL',
    );
  });

  it('keeps a two-digit year, so the 36 Senate races stay 36 series', () => {
    // A \d{2,} threshold would strip the year and leave every race as `SEN`.
    assert.equal(seriesFromEvent({ ticker: 'SEN26MI' }), 'SEN26MI');
    assert.equal(seriesFromEvent({ ticker: 'SEN26AK' }), 'SEN26AK');
    assert.equal(seriesFromEvent({ ticker: 'HOUSE26AZ-06' }), 'HOUSE26AZ-06');
  });

  it('ignores an empty series field rather than filing the event under nothing', () => {
    assert.equal(seriesFromEvent({ ticker: 'FED260917', series: '' }), 'FED');
  });

  it('falls back to the ticker when stripping would leave nothing', () => {
    assert.equal(seriesFromEvent({ ticker: '2608212100' }), '2608212100');
    assert.equal(seriesFromEvent({}), '');
  });
});

/* ------------------------------------------------------------------ events */

describe('normaliseEvent', () => {
  const btc: RawGeminiEvent = {
    ticker: 'BTC2608212100',
    title: 'BTC price on August 21',
    type: 'categorical',
    series: 'BTC1H',
    category: 'Crypto',
    status: 'active',
    volume: '3801.44',
    volume24h: '522.93',
    effectiveDate: '2026-08-14T09:27:07.707Z',
    expiryDate: '2026-08-21T21:00:00.000Z',
    startTime: '2026-08-14T09:30:00.000Z',
    contracts: [
      {
        label: '$61,000 or above',
        instrumentSymbol: 'GEMI-BTC2608212100-HI61000',
        prices: { bestBid: '0.97', bestAsk: '0.98', lastTradePrice: '0.83' },
        strike: { type: 'above', value: '61000' },
      },
      {
        label: '$61,500 or above',
        instrumentSymbol: 'GEMI-BTC2608212100-HI61500',
        prices: { bestBid: '0.94', bestAsk: '0.96', lastTradePrice: '0.74' },
        strike: { type: 'above', value: '61500' },
      },
    ],
  };

  it('stamps its event and series onto every leg', () => {
    const event = normaliseEvent(btc);
    assert.equal(event.venue, 'gemini');
    assert.equal(event.eventTicker, 'BTC2608212100');
    assert.equal(event.seriesTicker, 'BTC1H');
    assert.equal(event.markets.length, 2);
    assert.equal(event.markets[0]!.eventTicker, 'BTC2608212100');
    assert.equal(event.markets[1]!.seriesTicker, 'BTC1H');
    assert.equal(event.markets[0]!.title, 'BTC price on August 21');
    assert.equal(event.markets[0]!.marketType, 'categorical');
    assert.equal(event.markets[0]!.category, 'Crypto');
  });

  it('claims no exclusivity, because `categorical` includes cumulative ladders', () => {
    // These two legs are "$61,000 or above" and "$61,500 or above": the full
    // ladder's asks sum to $6.80, and every leg below the settlement pays.
    assert.equal(normaliseEvent(btc).mutuallyExclusive, false);
  });

  it('claims no exclusivity on a two-leg head-to-head either', () => {
    // Obviously exclusive, and the exchange still says nothing that says so.
    const game = normaliseEvent({
      ticker: 'NFL-2608212330-CAR-JAX-M',
      type: 'categorical',
      contracts: [{ instrumentSymbol: 'A' }, { instrumentSymbol: 'B' }],
    });
    assert.equal(game.mutuallyExclusive, false);
  });

  it('withholds the event’s turnover from the legs of a multi-leg event', () => {
    // Copying it onto both legs would have sumOrNull report double the true
    // turnover in the search panel.
    const event = normaliseEvent(btc);
    assert.equal(event.markets[0]!.volume, null);
    assert.equal(event.markets[0]!.volume24h, null);
    assert.equal(event.markets[1]!.volume24h, null);
  });

  it('gives a one-leg event’s turnover to its only leg, where the two are one figure', () => {
    const recession = normaliseEvent({
      ticker: 'RECESSION26',
      title: 'Recession this year?',
      type: 'binary',
      volume: '46211',
      volume24h: '1470',
      contracts: [
        {
          label: 'Yes',
          instrumentSymbol: 'GEMI-RECESSION26-2026',
          prices: { bestBid: '0.07', bestAsk: '0.08', lastTradePrice: '0.08' },
        },
      ],
    });
    assert.equal(recession.markets[0]!.volume, 46211);
    assert.equal(recession.markets[0]!.volume24h, 1470);
    // Still nothing anywhere for these two.
    assert.equal(recession.markets[0]!.openInterest, null);
    assert.equal(recession.markets[0]!.liquidity, null);
  });

  it('reads the legs in the order the venue numbers them, not the array’s', () => {
    // WXHIGH-LA arrives with its temperature buckets shuffled; sortOrder reads
    // the ladder cold to hot, and a strike ladder out of sequence is unreadable.
    const wx = normaliseEvent({
      ticker: 'WXHIGH-LA-2608190359',
      series: 'WXHIGH',
      contracts: [
        { label: '79°F to 80°F', instrumentSymbol: 'A', sortOrder: 4 },
        { label: '74°F or below', instrumentSymbol: 'B', sortOrder: 1 },
        { label: '83°F or above', instrumentSymbol: 'C', sortOrder: 6 },
      ],
    });
    assert.deepEqual(
      wx.markets.map((m) => m.yesSubTitle),
      ['74°F or below', '79°F to 80°F', '83°F or above'],
    );
  });

  it('leaves a partly numbered event in the order it arrived', () => {
    // Sorting around the gaps would move the numbered legs past the rest for
    // no stated reason; the payload order is at least the exchange's own.
    const mixed = normaliseEvent({
      ticker: 'FED260917',
      contracts: [
        { label: 'Fed maintains rates', instrumentSymbol: 'A' },
        { label: 'Hike 25bps', instrumentSymbol: 'B', sortOrder: 0 },
      ],
    });
    assert.deepEqual(
      mixed.markets.map((m) => m.yesSubTitle),
      ['Fed maintains rates', 'Hike 25bps'],
    );
  });

  it('keeps a one-leg event’s stated zero turnover as a zero', () => {
    // The exchange answered "nothing traded". That is not the same silence as
    // the multi-leg legs above, and must not become one.
    const quiet = normaliseEvent({
      ticker: 'ZECEXPLOIT26',
      type: 'binary',
      volume: '0',
      volume24h: '0',
      contracts: [{ label: 'Proof of non-exploitation published', instrumentSymbol: 'Z' }],
    });
    assert.equal(quiet.markets[0]!.volume, 0);
    assert.equal(quiet.markets[0]!.volume24h, 0);
  });

  it('leaves the subtitle empty rather than filling it with hourly commentary', () => {
    assert.equal(normaliseEvent(btc).subTitle, '');
  });
});

describe('flattenRules', () => {
  it('joins a Contentful document, which is the only spelling rules ever have', () => {
    assert.equal(flattenRules({ content: [{ value: 'a' }, { value: 'b' }] }), 'ab');
    assert.equal(flattenRules(undefined), '');
  });
});

/* -------------------------------------------------------------------- book */

describe('normaliseOrderBook', () => {
  // /v1/book/GEMI-DEMNOM2028-DEMNOM28AOC, verbatim.
  const raw = {
    bids: [
      { price: '0.19', amount: '1530.0', timestamp: '1787032777' },
      { price: '0.18', amount: '25.0', timestamp: '1787032777' },
      { price: '0.17', amount: '788.0', timestamp: '1787032777' },
      { price: '0.16', amount: '100.0', timestamp: '1787032777' },
      { price: '0.01', amount: '250.0', timestamp: '1787032777' },
    ],
    asks: [
      { price: '0.2', amount: '1250.0', timestamp: '1787032777' },
      { price: '0.21', amount: '110.0', timestamp: '1787032777' },
    ],
  };

  it('coerces the string ladders and orders them best-first', () => {
    const book = normaliseOrderBook(raw, 'GEMI-DEMNOM2028-DEMNOM28AOC');
    assert.equal(book.venue, 'gemini');
    assert.deepEqual(
      book.yes.map((l) => l.price),
      [0.19, 0.18, 0.17, 0.16, 0.01],
    );
    assert.deepEqual(
      book.yesAsks.map((l) => l.price),
      [0.2, 0.21],
    );
    assert.equal(book.yes[0]!.size, 1530);
  });

  it('derives the NO ladder by inverting the offers', () => {
    const book = normaliseOrderBook(raw, 'x');
    assert.deepEqual(
      book.no.map((l) => l.price),
      [0.8, 0.79],
    );
    assert.equal(book.no[0]!.size, 1250);
  });

  it('states the top of book and the spread', () => {
    const book = normaliseOrderBook(raw, 'x');
    assert.equal(book.bestYesBid, 0.19);
    assert.equal(book.bestYesAsk, 0.2);
    assert.equal(book.spread, 0.01);
    assert.equal(book.mid, 0.195);
  });

  it('reports a settled instrument’s empty book as absent, not as a zero quote', () => {
    // A settled instrument answers {"bids":[],"asks":[]}.
    const book = normaliseOrderBook({ bids: [], asks: [] }, 'x');
    assert.deepEqual(book.yes, []);
    assert.deepEqual(book.no, []);
    assert.equal(book.bestYesBid, null);
    assert.equal(book.bestYesAsk, null);
    assert.equal(book.spread, null);
    assert.equal(book.mid, null);
  });

  it('caps each side at the requested depth', () => {
    const book = normaliseOrderBook(raw, 'x', 2);
    assert.equal(book.yes.length, 2);
    assert.equal(book.yesAsks.length, 2);
    assert.equal(book.bestYesBid, 0.19);
  });

  it('drops rows with no price or no size', () => {
    const book = normaliseOrderBook(
      { bids: [{ price: '0', amount: '100' }, { price: '0.4', amount: '0' }, { price: '0.3', amount: '5' }] },
      'x',
    );
    assert.equal(book.yes.length, 1);
    assert.equal(book.yes[0]!.price, 0.3);
  });
});

describe('applyBook', () => {
  const base = normaliseMarket(AOC, PARENT);
  const raw = {
    bids: [
      { price: '0.19', amount: '1530.0' },
      { price: '0.18', amount: '25.0' },
    ],
    asks: [{ price: '0.2', amount: '1250.0' }],
  };

  it('adds up the resting depth the exchange never states', () => {
    // 0.19×1530 + 0.18×25 on the bid, and 0.80×1250 behind the offer.
    const market = applyBook(base, raw);
    assert.equal(market.liquidity, 1295.2);
  });

  it('refreshes the top of book from the live ladder', () => {
    const stale = normaliseMarket(
      { ...AOC, prices: { bestBid: '0.10', bestAsk: '0.30', lastTradePrice: '0.22' } },
      PARENT,
    );
    const market = applyBook(stale, raw);
    assert.equal(market.yesBid, 0.19);
    assert.equal(market.yesAsk, 0.2);
    assert.equal(market.noBid, 0.8);
    assert.equal(market.mid, 0.195);
  });

  it('calls an empty book zero depth — the exchange was asked and answered', () => {
    const market = applyBook(base, { bids: [], asks: [] });
    assert.equal(market.liquidity, 0);
    // Turnover is the other case: never asked, never answered.
    assert.equal(market.volume24h, null);
  });

  it('leaves the market untouched when the book could not be read', () => {
    assert.deepEqual(applyBook(base, null), base);
  });
});

describe('applyTicker', () => {
  // /v2/ticker/GEMI-DEMNOM2028-DEMNOM28AOC, verbatim but for a trimmed changes array.
  const raw = {
    symbol: 'GEMI-DEMNOM2028-DEMNOM28AOC',
    open: '0.2',
    high: '0.22',
    low: '0.2',
    close: '0.22',
    changes: ['0.2', '0.21', '0.22'],
    bid: '0.1900',
    ask: '0.2000',
  };

  it('coerces the four-decimal quote strings', () => {
    const market = applyTicker(normaliseMarket(AOC, PARENT), raw);
    assert.equal(market.yesBid, 0.19);
    assert.equal(market.yesAsk, 0.2);
    assert.equal(market.lastPrice, 0.22);
  });

  it('gives a move to a contract whose catalogue row carries no percentage', () => {
    // 262 contracts state no priceDelta24hPct; the 24h open is the only other
    // place a previous price exists.
    const bare = normaliseMarket({ ...AOC, priceDelta24hPct: undefined }, PARENT);
    assert.equal(bare.change, null);
    const market = applyTicker(bare, raw);
    assert.equal(market.previousPrice, 0.2);
    assert.equal(market.change, 0.02);
  });

  it('keeps the exchange’s own percentage where it stated one', () => {
    const market = applyTicker(normaliseMarket(AOC, PARENT), { ...raw, open: '0.05' });
    assert.equal(market.previousPrice, 0.2);
  });

  it('leaves the market untouched when the instrument has never traded', () => {
    // /v2/ticker 404s on those, which is a fact about the contract.
    const base = normaliseMarket(AOC, PARENT);
    assert.deepEqual(applyTicker(base, null), base);
  });
});

/* -------------------------------------------------------------------- tape */

describe('normaliseTrades', () => {
  // /v1/trades/GEMI-DEMNOM2028-DEMNOM28AOC, verbatim.
  const rows: RawGeminiTrade[] = [
    {
      timestamp: 1787003922,
      timestampms: 1787003922607,
      tid: 1893456011016044,
      price: '0.22',
      amount: '100',
      type: 'buy',
    },
    {
      timestamp: 1786998079,
      timestampms: 1786998079329,
      tid: 1893456011015174,
      price: '0.21',
      amount: '137',
      type: 'sell',
    },
  ];

  it('coerces the price and size strings and restates the NO price', () => {
    const { trades } = normaliseTrades(rows, 'GEMI-DEMNOM2028-DEMNOM28AOC', 50);
    assert.equal(trades[0]!.yesPrice, 0.22);
    assert.equal(trades[0]!.noPrice, 0.78);
    assert.equal(trades[0]!.count, 100);
    assert.equal(trades[0]!.venue, 'gemini');
    assert.equal(trades[0]!.ticker, 'GEMI-DEMNOM2028-DEMNOM28AOC');
  });

  it('carries the 16-digit trade id as text', () => {
    const { trades } = normaliseTrades(rows, 'x', 50);
    assert.equal(trades[0]!.tradeId, '1893456011016044');
    assert.equal(typeof trades[0]!.tradeId, 'string');
  });

  it('takes the seconds timestamp as it stands, without touching the ms twin', () => {
    const { trades } = normaliseTrades(rows, 'x', 50);
    assert.equal(trades[0]!.ts, 1787003922);
  });

  it('falls back to the millisecond stamp when only that is present', () => {
    const { trades } = normaliseTrades(
      [{ timestampms: 1787003922607, price: '0.2', amount: '5' }],
      'x',
      50,
    );
    assert.equal(trades[0]!.ts, 1787003922);
  });

  it('drops a print it cannot price, size or place, rather than inventing one', () => {
    // A defaulted price is a free trade, a defaulted size an empty one and a
    // defaulted clock a print stamped 1970 — all three read as outliers.
    const { trades } = normaliseTrades(
      [
        { timestamp: 1787003922, amount: '100', type: 'buy' },
        { timestamp: 1787003922, price: '0.2', type: 'buy' },
        { price: '0.2', amount: '100', type: 'buy' },
        ...rows,
      ],
      'x',
      50,
    );
    assert.equal(trades.length, 2);
    assert.equal(trades[0]!.yesPrice, 0.22);
  });

  it('reads buy and sell as the aggressor’s side, and flags no blocks', () => {
    const { trades } = normaliseTrades(rows, 'x', 50);
    assert.equal(trades[0]!.takerSide, 'yes');
    assert.equal(trades[1]!.takerSide, 'no');
    assert.equal(trades[0]!.isBlockTrade, false);
  });

  it('offers no cursor, because a tid walk over an inconsistent tape skips prints', () => {
    // Identical requests return 278 or 349 rows, and neither is a prefix of
    // the other.
    assert.equal(normaliseTrades(rows, 'x', 50).cursor, null);
  });

  it('honours the requested page size', () => {
    assert.equal(normaliseTrades(rows, 'x', 1).trades.length, 1);
    assert.equal(normaliseTrades([], 'x', 50).trades.length, 0);
  });
});

/* ----------------------------------------------------------------- candles */

describe('normaliseCandles', () => {
  // /v2/candles/GEMI-DEMNOM2028-DEMNOM28AOC/1day, verbatim, newest first.
  const daily = [
    [1786924800000, 0.2, 0.22, 0.2, 0.22, 5467],
    [1786838400000, 0.21, 0.21, 0.19, 0.2, 975],
    [1786752000000, 0.18, 0.21, 0.18, 0.21, 1819],
  ];

  it('turns the period start in milliseconds into a period end in seconds', () => {
    // 1786924800000 is 2026-08-17T00:00Z; the daily bar it opens closes a day later.
    const candles = normaliseCandles(daily, 1440);
    assert.equal(candles[2]!.time, 1786924800 + 86_400);
    assert.equal(new Date(candles[2]!.time * 1000).toISOString(), '2026-08-18T00:00:00.000Z');
  });

  it('reverses the feed, which arrives newest first', () => {
    const candles = normaliseCandles(daily, 1440);
    assert.deepEqual(
      candles.map((c) => c.close),
      [0.21, 0.2, 0.22],
    );
  });

  it('carries the OHLC through as dollars and the volume as contracts', () => {
    const candles = normaliseCandles(daily, 1440);
    const last = candles[2]!;
    assert.equal(last.open, 0.2);
    assert.equal(last.high, 0.22);
    assert.equal(last.low, 0.2);
    assert.equal(last.close, 0.22);
    assert.equal(last.volume, 5467);
    assert.equal(last.traded, true);
  });

  it('marks a bar with nothing printed untraded, and keeps its stated zero', () => {
    // A real 1hr row: the close is carried forward, and the exchange does state
    // the zero — unlike turnover, which it never states at all.
    const candles = normaliseCandles([[1787029200000, 0.22, 0.22, 0.22, 0.22, 0]], 60);
    assert.equal(candles[0]!.volume, 0);
    assert.equal(candles[0]!.traded, false);
  });

  it('states no open interest or quote, because the row carries none', () => {
    const candles = normaliseCandles(daily, 1440);
    assert.equal(candles[0]!.openInterest, null);
    assert.equal(candles[0]!.bid, null);
    assert.equal(candles[0]!.ask, null);
  });

  it('closes a one-minute bar a minute after it opened', () => {
    const candles = normaliseCandles([[1787003880000, 0.7, 0.71, 0.7, 0.71, 30]], 1);
    assert.equal(candles[0]!.time, 1787003880 + 60);
  });

  it('drops a malformed row rather than charting a NaN', () => {
    const candles = normaliseCandles([[1786924800000, 0.2, 0.22, 0.2, 0.22, 5467], [1, 2]], 1440);
    assert.equal(candles.length, 1);
  });
});
