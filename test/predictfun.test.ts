/**
 * predict.fun normalisation tests.
 *
 * The fixtures are trimmed copies of real `graphql.predict.fun` responses, so
 * the things that make this venue awkward are all present as they arrive: the
 * `chancePercentage` that looks like a midpoint and is not one, the
 * `percentageChanceChange24h` that looks like the daily move and is a range,
 * the outcome pair that is only sometimes called Yes/No, the 1e18-scaled
 * strings on the tape, the last print that names an outcome but is quoted in
 * YES either way, and the null bid and ask that a live market with real money
 * behind it can carry.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  bucketSamples,
  complement,
  decimal,
  figure,
  fromWei,
  makeTicker,
  normaliseEvent,
  normaliseMarket,
  normaliseOrderBook,
  normaliseSeries,
  normaliseTrades,
  parseTicker,
  seriesFromSlug,
  statusOf,
  type RawPfCategory,
  type RawPfConnection,
  type RawPfMarket,
  type RawPfTrade,
} from '../src/server/sources/predictfun.js';

/** Wrap rows the way every list on this API arrives. */
function connection<T>(...items: T[]): RawPfConnection<T> {
  return { edges: items.map((node) => ({ node })) };
}

/* ---------------------------------------------------------------- coercion */

describe('figure', () => {
  it('keeps a stated zero, which is a price the venue actually quotes', () => {
    assert.equal(figure(0), 0);
  });

  it('reports an unpublished figure as absent rather than as zero', () => {
    assert.equal(figure(null), null);
    assert.equal(figure(undefined), null);
    assert.equal(figure(Number.NaN), null);
  });
});

describe('decimal', () => {
  it('reads the last print, which the venue sends as text', () => {
    assert.equal(decimal('0.14'), 0.14);
    assert.equal(decimal(0.14), 0.14);
  });

  it('returns null for an absent or unreadable price', () => {
    assert.equal(decimal(null), null);
    assert.equal(decimal(undefined), null);
    assert.equal(decimal(''), null);
    assert.equal(decimal('   '), null);
    assert.equal(decimal('n/a'), null);
  });

  it('keeps a zero written as text, which the venue does send', () => {
    // `Number('')` is 0 and `Number(null)` is 0, so the empty cases above are
    // the ones that must not reach the coercion. A written "0.00" must.
    assert.equal(decimal('0.00'), 0);
    assert.equal(decimal('0'), 0);
  });
});

describe('fromWei', () => {
  it('unscales the tape, where size and price are 1e18 integer strings', () => {
    assert.equal(fromWei('207230000000000000000'), 207.23);
    assert.equal(fromWei('70000000000000000'), 0.07);
    assert.equal(fromWei('951000000000000000'), 0.951);
    // 0.01 shares — the smallest fill seen on the live tape.
    assert.equal(fromWei('10000000000000000'), 0.01);
  });

  it('refuses a shape it does not understand instead of reporting a free trade', () => {
    assert.equal(fromWei('0.07'), null);
    assert.equal(fromWei(null), null);
    assert.equal(fromWei(undefined), null);
    assert.equal(fromWei(''), null);
    assert.equal(fromWei('n/a'), null);
  });

  it('keeps a scaled zero apart from an absent one', () => {
    assert.equal(fromWei('0'), 0);
  });
});

describe('complement', () => {
  it('mirrors YES to NO at the leg’s own precision', () => {
    // Leg 26952 is quoted to three places: ask 0.14 → NO bid 0.86.
    assert.equal(complement(0.14, 3), 0.86);
    assert.equal(complement(0.139, 3), 0.861);
  });

  it('rounds at the stated precision rather than at the category’s', () => {
    // The same leg's category is quoted to two places; using it would move the
    // NO ladder a tenth of a cent away from the one the exchange shows.
    assert.equal(complement(0.139, 2), 0.86);
    assert.equal(complement(0.139, 3), 0.861);
  });

  it('falls back to two places when the venue states no precision', () => {
    assert.equal(complement(0.25, null), 0.75);
  });
});

/* -------------------------------------------------------------- identifiers */

describe('makeTicker / parseTicker', () => {
  it('round-trips a slug that itself ends in a hyphenated number', () => {
    const ticker = makeTicker('btc-updown-15m-1787032800', '1436748');
    assert.equal(ticker, 'btc-updown-15m-1787032800~1436748');
    assert.deepEqual(parseTicker(ticker), {
      slug: 'btc-updown-15m-1787032800',
      marketId: '1436748',
    });
  });

  it('splits on the last separator, so nothing in the slug can shadow it', () => {
    assert.deepEqual(parseTicker('big-game-champion-2027~26952'), {
      slug: 'big-game-champion-2027',
      marketId: '26952',
    });
  });

  it('reports a bare identifier as unsplittable rather than guessing halves', () => {
    assert.equal(parseTicker('26952'), null);
    assert.equal(parseTicker('big-game-champion-2027'), null);
  });
});

describe('seriesFromSlug', () => {
  it('strips the unix stamp the machine-made crypto series regenerates', () => {
    assert.equal(seriesFromSlug('btc-updown-15m-1787032800'), 'btc-updown-15m');
    assert.equal(seriesFromSlug('btc-updown-15m-1787033700'), 'btc-updown-15m');
    assert.equal(seriesFromSlug('eth-updown-5m-1787033400'), 'eth-updown-5m');
  });

  it('strips the fixture date, so a league’s games group together', () => {
    assert.equal(seriesFromSlug('mlb-oak-hou-2026-08-23'), 'mlb-oak-hou');
    assert.equal(seriesFromSlug('lol-t1-dnf-2026-08-17'), 'lol-t1-dnf');
  });

  it('strips a season year, so consecutive seasons are one question', () => {
    assert.equal(seriesFromSlug('big-game-champion-2027'), 'big-game-champion');
    assert.equal(seriesFromSlug('nba-2027-champion'), 'nba-champion');
    assert.equal(
      seriesFromSlug('what-will-fed-rate-hit-before-2027'),
      'what-will-fed-rate-hit-before',
    );
  });

  it('strips a date spelled in words, which is where the Fed books live', () => {
    // The whole reason this is not left alone: September's and October's FOMC
    // books are one question asked twice, and keeping the month makes them two
    // one-event series that pair with nothing at Kalshi.
    assert.equal(seriesFromSlug('fed-decision-in-september-762'), 'fed-decision-in');
    assert.equal(seriesFromSlug('fed-decision-in-october-20260617190323537'), 'fed-decision-in');
    assert.equal(seriesFromSlug('bitcoin-up-or-down-on-august-18-2026'), 'bitcoin-up-or-down-on');
  });

  it('takes the day off a named month but leaves a district number alone', () => {
    // The day is only claimed next to a month name. Nothing anchors the `15`
    // in a district slug, so all 38 Texas districts stay 38 series.
    assert.equal(
      seriesFromSlug('ethereum-up-or-down-august-18-2026-2am-et'),
      'ethereum-up-or-down-2am-et',
    );
    assert.equal(seriesFromSlug('ushr-tx-15'), 'ushr-tx-15');
  });

  it('strips the creation stamp the season books carry, which is not a year', () => {
    // 28 of the 1,024 open categories are minted with a seventeen-digit
    // creation time. Left on, every league's title race is its own one-event
    // series and pairs with nothing at another broker.
    assert.equal(seriesFromSlug('laliga-2027-champion-20260701200737375'), 'laliga-champion');
  });

  it('leaves the second half of a split season on, and that is the known limit', () => {
    // `2026-27` is a season, not a date, and the stripper takes numbers only in
    // whole date groups — the rule that keeps all 38 Texas districts apart. So
    // the `27` survives and the book re-keys at each season rollover. Within a
    // season it is stable, which is what `XV` needs; loosening the rule to
    // catch it would cost far more than it saves.
    assert.equal(
      seriesFromSlug('nhl-2026-27-calder-trophy-20260625172243021'),
      'nhl-27-calder-trophy',
    );
  });

  it('keeps a number in the middle of a slug, which is part of the question', () => {
    // The short-run rule fires only at the end, where this venue puts its
    // disambiguator. Anywhere else the digits are the market.
    assert.equal(seriesFromSlug('nasdaq-100-above'), 'nasdaq-100-above');
    assert.equal(seriesFromSlug('btc-updown-15m-1787032800'), 'btc-updown-15m');
  });
});

/* ------------------------------------------------------------------ status */

describe('statusOf', () => {
  it('reads REGISTERED as live, because that is the ordinary trading state', () => {
    assert.equal(statusOf({ status: 'REGISTERED', isTradingEnabled: true }), 'open');
    assert.equal(statusOf({ status: 'UNPAUSED' }), 'open');
  });

  it('honours the venue’s halt switch over the registration state', () => {
    assert.equal(statusOf({ status: 'REGISTERED', isTradingEnabled: false }), 'closed');
    assert.equal(statusOf({ status: 'PAUSED' }), 'closed');
  });

  it('separates a decided market from a settled one', () => {
    assert.equal(statusOf({ status: 'PRICE_PROPOSED' }), 'determined');
    assert.equal(statusOf({ status: 'PRICE_DISPUTED' }), 'determined');
    assert.equal(statusOf({ status: 'RESOLVED' }), 'settled');
  });

  it('reads the pre-open states as unopened', () => {
    assert.equal(statusOf({ status: 'INITIALIZING' }), 'unopened');
    assert.equal(statusOf({ status: 'CREATING' }), 'unopened');
  });
});

/* ----------------------------------------------------------------- markets */

// Leg 26952 of the NFL champion category, verbatim but trimmed. Note
// `chancePercentage: 14.0` against a book of 0.139/0.14 — a mid of 0.1395.
const RAMS: RawPfMarket = {
  id: '26952',
  title: 'Los Angeles Rams',
  question: 'Will the Los Angeles Rams win the 2027 NFL league championship?',
  description: 'This market will resolve according to the team that wins the 2027 NFL league championship.',
  status: 'REGISTERED',
  marketType: null,
  isTradingEnabled: true,
  decimalPrecision: 3,
  chancePercentage: 14.0,
  resolution: null,
  statistics: {
    totalLiquidityUsd: 107890435.51,
    liquidity3CAskUsd: 5574.42,
    volumeTotalUsd: 5896.0,
    volume24hUsd: 67.38,
    volume24hChangeUsd: 213.38,
    percentageChanceChange24h: 1.0,
  },
  orderbook: {
    marketId: 26952,
    asks: [
      [0.14, 1838.8899999999999],
      [0.141, 6131.34],
      [0.142, 2439.4965],
    ],
    bids: [
      [0.139, 6302.299999999999],
      [0.138, 1782.75],
      [0.137, 7412.0],
    ],
    lastOrderSettled: { id: '2265739738', price: '0.14', kind: 'LIMIT', side: 'Ask', outcome: 'Yes' },
  },
  outcomes: connection(
    {
      id: '51626',
      index: 1,
      name: 'Yes',
      status: null,
      chancePercentage: 13.9,
      bidPriceInCurrency: 0.139,
      askPriceInCurrency: 0.14,
      statistics: { sharesCount: 5425.598222287107, positionsValueUsd: 754.1581528979079 },
    },
    {
      id: '51627',
      index: 2,
      name: 'No',
      status: null,
      chancePercentage: 86.0,
      bidPriceInCurrency: 0.86,
      askPriceInCurrency: 0.861,
      statistics: { sharesCount: 5425.611265690695, positionsValueUsd: 4666.025688493998 },
    },
  ),
};

const NFL_CHAMPION: RawPfCategory = {
  id: 'big-game-champion-2027',
  slug: 'big-game-champion-2027',
  title: 'NFL Champion 2027',
  status: 'OPEN',
  marketVariant: 'DEFAULT',
  isNegRisk: true,
  decimalPrecision: 2,
  startsAt: '2026-06-01T22:30:00.000Z',
  endsAt: '2027-02-14T23:55:00.000Z',
  statistics: {
    liquidityValueUsd: 1460060205.98,
    liquidity3CAskUsd: 67651.43,
    volumeTotalUsd: 1455756.12,
    volume24hUsd: 157362.69,
  },
  tags: connection({ id: '4', name: 'Sports' }, { id: '45', name: 'NFL' }),
};

describe('normaliseMarket', () => {
  it('names the leg by its category and id, both of which round-trip', () => {
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.equal(market.venue, 'predictfun');
    assert.equal(market.ticker, 'big-game-champion-2027~26952');
    assert.equal(market.eventTicker, 'big-game-champion-2027');
    assert.equal(market.seriesTicker, 'big-game-champion');
  });

  it('quotes both sides from the outcome pair', () => {
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.equal(market.yesBid, 0.139);
    assert.equal(market.yesAsk, 0.14);
    assert.equal(market.noBid, 0.86);
    assert.equal(market.noAsk, 0.861);
  });

  it('computes the mid from the book instead of trusting chancePercentage', () => {
    // The venue reports a chance of 14.0 on a 0.139/0.14 book. It agrees with
    // the mid on well under half of all legs and is not a midpoint at all.
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.equal(market.mid, 0.1395);
  });

  it('does not take chancePercentage even when it disagrees wildly', () => {
    // Leg 1459048, live: bid 0.10, ask 0.85, chancePercentage 70.
    const wide = normaliseMarket({
      ...RAMS,
      chancePercentage: 70.0,
      outcomes: connection(
        { index: 1, name: 'Yes', bidPriceInCurrency: 0.1, askPriceInCurrency: 0.85 },
        { index: 2, name: 'No', bidPriceInCurrency: 0.15, askPriceInCurrency: 0.9 },
      ),
    });
    assert.equal(wide.mid, 0.475);
  });

  it('derives the NO side from the YES book when only one side is stated', () => {
    const oneSided = normaliseMarket({
      ...RAMS,
      outcomes: connection({ index: 1, name: 'Yes', bidPriceInCurrency: 0.139, askPriceInCurrency: 0.14 }),
    });
    assert.equal(oneSided.noBid, 0.86);
    assert.equal(oneSided.noAsk, 0.861);
  });

  it('reports an unquoted leg as absent, not as a zero bid', () => {
    // Leg 935921 has $119k of lifetime turnover, is REGISTERED inside an OPEN
    // category, and carries no bid or ask at all — 189 of 1,780 outcomes do.
    const unquoted = normaliseMarket(
      {
        ...RAMS,
        orderbook: null,
        outcomes: connection(
          { index: 1, name: 'Yes', bidPriceInCurrency: null, askPriceInCurrency: null },
          { index: 2, name: 'No', bidPriceInCurrency: null, askPriceInCurrency: null },
        ),
      },
      NFL_CHAMPION,
    );
    assert.equal(unquoted.yesBid, null);
    assert.equal(unquoted.yesAsk, null);
    assert.equal(unquoted.noBid, null);
    assert.equal(unquoted.noAsk, null);
    assert.equal(unquoted.mid, null);
  });

  it('reads open interest off sharesCount, which is what the venue calls it', () => {
    // The YES and NO counts agree to five figures — minted pairs, not turnover.
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.equal(market.openInterest, 5425.598222287107);
  });

  it('carries turnover as the dollars the venue meters, and the depth it trusts', () => {
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.equal(market.volume, 5896);
    assert.equal(market.volume24h, 67.38);
    // liquidity3CAskUsd, not totalLiquidityUsd — the latter reads 107,890,435
    // on this leg, whose whole book is about $5.5k.
    assert.equal(market.liquidity, 5574.42);
  });

  it('states no 24h move, because the venue publishes a range and no direction', () => {
    // `percentageChanceChange24h` reads like the daily move and is the daily
    // spread: never negative in 6,341 stated values, and on leg 935920 it
    // reported 29.0 across a 29-point fall. Signing it would paint a crash
    // green, and nothing else in the schema states an earlier price.
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.equal(market.change, null);
    assert.equal(market.previousPrice, null);

    const wild = normaliseMarket({
      ...RAMS,
      statistics: { ...RAMS.statistics, percentageChanceChange24h: 77.0 },
    });
    assert.equal(wild.change, null);
    // A zero range is a real statement about the day, but it is still a range
    // and not a move, so it is not reported as a flat one either.
    const flat = normaliseMarket({
      ...RAMS,
      statistics: { ...RAMS.statistics, percentageChanceChange24h: 0 },
    });
    assert.equal(flat.change, null);
    assert.equal(flat.previousPrice, null);
  });

  it('leaves the figures it is not given unstated rather than zero', () => {
    const quiet = normaliseMarket({ ...RAMS, statistics: { volumeTotalUsd: 5896.0 } });
    assert.equal(quiet.volume, 5896);
    assert.equal(quiet.volume24h, null);
    assert.equal(quiet.liquidity, null);
    // A stated zero is the venue's own answer and survives as one: leg 935921
    // really does rest $0 within three cents of its ask.
    const dead = normaliseMarket({
      ...RAMS,
      statistics: { volumeTotalUsd: 119464.85, volume24hUsd: 0, liquidity3CAskUsd: 0 },
    });
    assert.equal(dead.volume24h, 0);
    assert.equal(dead.liquidity, 0);
  });

  it('takes the last print as quoted, because it is already in YES terms', () => {
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.equal(market.lastPrice, 0.14);

    // Leg 1457355 is quoted 0.01 on YES and reports its last settled order as
    // {price: "0.01", outcome: "No"}. `outcome` names the side the resting
    // order sat on, not the space the price is in — inverting it would print
    // 99¢ on a penny market. 163 of 167 live NO-named prints agree.
    const noSide = normaliseMarket({
      ...RAMS,
      decimalPrecision: 2,
      orderbook: {
        asks: [[0.01, 5000]],
        bids: [],
        lastOrderSettled: { price: '0.01', side: 'Bid', outcome: 'No' },
      },
      outcomes: connection(
        { index: 1, name: 'Up', bidPriceInCurrency: null, askPriceInCurrency: 0.01 },
        { index: 2, name: 'Down', bidPriceInCurrency: 0.99, askPriceInCurrency: null },
      ),
    });
    assert.equal(noSide.lastPrice, 0.01);
    // One-sided book, so the mid falls back to the print rather than inventing
    // a midpoint out of the single quote that exists.
    assert.equal(noSide.mid, 0.01);
  });

  it('keeps a last print of zero, which is the venue rounding to the cent', () => {
    // Every print comes back at two decimals — 315 of 315 sampled — including
    // on legs quoted to three, so a leg that last traded at 0.003 reports 0.00.
    // That is a stated price, not a missing one, and the exact fill is on the
    // tape.
    const subCent = normaliseMarket({
      ...RAMS,
      orderbook: { asks: [[0.003, 447.14]], bids: [], lastOrderSettled: { price: '0.00', outcome: 'Yes' } },
      outcomes: connection(
        { index: 1, name: 'Yes', bidPriceInCurrency: null, askPriceInCurrency: 0.003 },
        { index: 2, name: 'No', bidPriceInCurrency: 0.997, askPriceInCurrency: null },
      ),
    });
    assert.equal(subCent.lastPrice, 0);
    assert.equal(subCent.yesAsk, 0.003);
  });

  it('leaves the last print unstated on a leg that has never traded', () => {
    const never = normaliseMarket({ ...RAMS, orderbook: { asks: [], bids: [], lastOrderSettled: null } });
    assert.equal(never.lastPrice, null);
  });

  it('labels the legs from the leg title when the outcomes are a plain pair', () => {
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.equal(market.title, 'Will the Los Angeles Rams win the 2027 NFL league championship?');
    assert.equal(market.yesSubTitle, 'Los Angeles Rams');
    assert.equal(market.noSubTitle, 'No');
  });

  it('labels them from the outcomes when the outcomes are the question', () => {
    // Every leg of an esports match is titled "Match Winner"; only the outcome
    // names say which team a row is about.
    const esports = normaliseMarket({
      ...RAMS,
      title: 'Match Winner',
      outcomes: connection(
        { index: 1, name: 'T1', bidPriceInCurrency: 0.6, askPriceInCurrency: 0.62 },
        { index: 2, name: 'DNS', bidPriceInCurrency: 0.38, askPriceInCurrency: 0.4 },
      ),
    });
    assert.equal(esports.yesSubTitle, 'T1');
    assert.equal(esports.noSubTitle, 'DNS');

    const crypto = normaliseMarket({
      ...RAMS,
      title: 'Bitcoin Up or Down on August 18?',
      outcomes: connection({ index: 1, name: 'Up' }, { index: 2, name: 'Down' }),
    });
    assert.equal(crypto.yesSubTitle, 'Up');
    assert.equal(crypto.noSubTitle, 'Down');
  });

  it('takes the leg’s own status, since a settled leg can sit in an open category', () => {
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.equal(market.status, 'open');
    assert.equal(market.result, '');

    const resolved = normaliseMarket(
      { ...RAMS, status: 'RESOLVED', resolution: { index: 2, name: 'DNS', status: 'WON' } },
      NFL_CHAMPION,
    );
    assert.equal(resolved.status, 'settled');
    assert.equal(resolved.result, 'no');
    assert.equal(
      normaliseMarket({ ...RAMS, resolution: { index: 1, name: 'T1', status: 'WON' } }).result,
      'yes',
    );
  });

  it('takes its dates, its type and its bucket from the category', () => {
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.equal(market.openTime, '2026-06-01T22:30:00.000Z');
    assert.equal(market.closeTime, '2027-02-14T23:55:00.000Z');
    assert.equal(market.expirationTime, '2027-02-14T23:55:00.000Z');
    // The leg states no type of its own; the category's variant is the fallback.
    assert.equal(market.marketType, 'DEFAULT');
    assert.equal(market.category, 'Sports');
  });

  it('states no strike, because the venue only ever writes one into a title', () => {
    const ladder = normaliseMarket({ ...RAMS, title: '>¥42B' });
    assert.equal(ladder.strikeType, null);
    assert.equal(ladder.floorStrike, null);
    assert.equal(ladder.capStrike, null);
  });

  it('falls back to the category’s terms for a leg read out of the catalogue', () => {
    // The crawl does not ask for the leg's own rules — they are two kilobytes
    // apiece and only `DES` shows them — so a catalogue leg carries the terms
    // its category states for the group.
    const market = normaliseMarket(RAMS, NFL_CHAMPION);
    assert.match(market.rulesPrimary, /^This market will resolve/);
    const catalogued = normaliseMarket({ ...RAMS, description: undefined }, {
      ...NFL_CHAMPION,
      description: 'Resolves to the team that wins the 2027 NFL league championship.',
    });
    assert.equal(
      catalogued.rulesPrimary,
      'Resolves to the team that wins the 2027 NFL league championship.',
    );
  });

  it('reads the category off the leg when it is not passed one', () => {
    const market = normaliseMarket({ ...RAMS, category: NFL_CHAMPION });
    assert.equal(market.eventTicker, 'big-game-champion-2027');
  });
});

/* ------------------------------------------------------------------ events */

describe('normaliseEvent', () => {
  const event = normaliseEvent({ ...NFL_CHAMPION, markets: connection(RAMS) });

  it('groups the legs under the category and stamps its tickers on them', () => {
    assert.equal(event.venue, 'predictfun');
    assert.equal(event.eventTicker, 'big-game-champion-2027');
    assert.equal(event.title, 'NFL Champion 2027');
    assert.equal(event.category, 'Sports');
    assert.equal(event.markets.length, 1);
    assert.equal(event.markets[0]!.eventTicker, 'big-game-champion-2027');
    assert.equal(event.markets[0]!.ticker, 'big-game-champion-2027~26952');
  });

  it('claims exclusivity only where the venue states it', () => {
    // The site offers NO-to-opposing-YES conversion on this one and says so.
    assert.equal(event.mutuallyExclusive, true);
    assert.equal(normaliseEvent({ ...NFL_CHAMPION, isNegRisk: false }).mutuallyExclusive, false);
    // An unstated flag is not a quiet yes: a wrong one puts a false arbitrage
    // on screen when `EVT` sums the legs.
    assert.equal(normaliseEvent({ ...NFL_CHAMPION, isNegRisk: null }).mutuallyExclusive, false);
  });

  it('groups a fixture under the series its slug names', () => {
    const fixture = normaliseEvent({
      id: 'lol-t1-dnf-2026-08-17',
      slug: 'lol-t1-dnf-2026-08-17',
      title: 'LoL: T1 vs DN SOOPers (BO5) - KeSPA Cup Playoffs',
      status: 'RESOLVED',
      isNegRisk: false,
      markets: connection({
        id: '1370660',
        title: 'Match Winner',
        status: 'RESOLVED',
        chancePercentage: 1.0,
        resolution: { index: 2, name: 'DNS', status: 'WON' },
        outcomes: connection(
          { index: 1, name: 'T1', status: 'LOST', bidPriceInCurrency: null, askPriceInCurrency: null },
          { index: 2, name: 'DNS', status: 'WON', bidPriceInCurrency: null, askPriceInCurrency: null },
        ),
      }),
    });
    assert.equal(fixture.seriesTicker, 'lol-t1-dnf');
    assert.equal(fixture.markets[0]!.status, 'settled');
    assert.equal(fixture.markets[0]!.result, 'no');
    assert.equal(fixture.markets[0]!.yesSubTitle, 'T1');
    // A settled leg publishes no prices at all, and none are invented for it.
    assert.equal(fixture.markets[0]!.mid, null);
    assert.equal(fixture.markets[0]!.openInterest, null);
  });
});

/* -------------------------------------------------------------------- book */

describe('normaliseOrderBook', () => {
  it('ladders the YES side and mirrors the NO side at the leg’s precision', () => {
    const book = normaliseOrderBook(RAMS, 'big-game-champion-2027~26952');
    assert.equal(book.venue, 'predictfun');
    assert.deepEqual(book.yes.map((l) => l.price), [0.139, 0.138, 0.137]);
    assert.deepEqual(book.yesAsks.map((l) => l.price), [0.14, 0.141, 0.142]);
    // 1 - 0.14 at three places, the exchange's own complement.
    assert.deepEqual(book.no.map((l) => l.price), [0.86, 0.859, 0.858]);
    assert.equal(book.no[0]!.size, 1838.8899999999999);
  });

  it('reports the top of book and the spread it implies', () => {
    const book = normaliseOrderBook(RAMS, 'x');
    assert.equal(book.bestYesBid, 0.139);
    assert.equal(book.bestYesAsk, 0.14);
    assert.equal(book.spread, 0.001);
    assert.equal(book.mid, 0.1395);
  });

  it('applies depth itself, because the venue returns the whole book', () => {
    const book = normaliseOrderBook(RAMS, 'x', 2);
    assert.equal(book.yes.length, 2);
    assert.equal(book.yesAsks.length, 2);
    assert.equal(book.no.length, 2);
  });

  it('sorts before slicing, so depth can never cut off the best price', () => {
    const scrambled = normaliseOrderBook(
      {
        ...RAMS,
        orderbook: {
          bids: [
            [0.12, 100],
            [0.139, 50],
          ],
          asks: [
            [0.2, 100],
            [0.14, 50],
          ],
        },
      },
      'x',
      1,
    );
    assert.equal(scrambled.bestYesBid, 0.139);
    assert.equal(scrambled.bestYesAsk, 0.14);
  });

  it('reports an empty book side as absent, not as a zero quote', () => {
    const book = normaliseOrderBook({ ...RAMS, orderbook: { asks: [], bids: [] } }, 'x');
    assert.deepEqual(book.yes, []);
    assert.deepEqual(book.no, []);
    assert.equal(book.bestYesBid, null);
    assert.equal(book.bestYesAsk, null);
    assert.equal(book.spread, null);
    assert.equal(book.mid, null);
  });

  it('drops rows that price or size nothing', () => {
    const book = normaliseOrderBook(
      {
        ...RAMS,
        orderbook: {
          bids: [
            [0, 100],
            [0.5, 0],
            [0.4, 10],
          ],
        },
      },
      'x',
    );
    assert.equal(book.yes.length, 1);
    assert.equal(book.yes[0]!.price, 0.4);
  });
});

/* -------------------------------------------------------------------- tape */

describe('normaliseTrades', () => {
  // Two real rows: a YES fill on leg 26936 and a NO fill on leg 26939.
  const tape: RawPfConnection<RawPfTrade> = {
    edges: [
      {
        cursor: 'eyJvcmRlcklkIjoyMjgxNjM3NzA2LCJjcmVhdGVkQXQiOiIyMDI2LTA4LTE4VDA1OjM0OjQ5LjAwMFoifQ==',
        node: {
          transactionHash: '0x4d20a10735cc659e19c7121ef9a4fa550192c5792bb187b78b01908f2a66844d',
          amountFilled: '207230000000000000000',
          priceExecuted: '70000000000000000',
          quoteType: 'ASK',
          timestamp: '2026-08-18T05:34:49.000Z',
          market: { id: '26936', title: 'Buffalo Bills', category: { slug: 'big-game-champion-2027' } },
          outcome: { index: 1, name: 'Yes' },
          account: { address: '0xB9b438572B3707a54A3EBCa1132482B0B4780408', name: 'laozhang' },
        },
      },
      {
        cursor: 'eyJvcmRlcklkIjoyMjgzMzE3OTg5fQ==',
        node: {
          transactionHash: '0x5a2e0af0bed02c6a71cc9ce179065cdda9680249ab3e6562a6deb8d42071eb4b',
          amountFilled: '10000000000000000',
          priceExecuted: '951000000000000000',
          quoteType: 'ASK',
          timestamp: '2026-08-18T06:51:18.000Z',
          market: { id: '26939', title: 'Dallas Cowboys', category: { slug: 'big-game-champion-2027' } },
          outcome: { index: 2, name: 'No' },
          account: { address: '0xD542F3b61f2dB5ee5bEF170EAbB39c6C6c8C1163', name: 'ioup' },
        },
      },
    ],
  };

  const trades = normaliseTrades(tape);

  it('unscales size and price out of their 1e18 strings', () => {
    assert.equal(trades[0]!.count, 207.23);
    assert.equal(trades[0]!.yesPrice, 0.07);
    assert.equal(trades[0]!.noPrice, 0.93);
  });

  it('re-bases a fill named on the NO outcome into YES terms', () => {
    // 0.951 of NO is the same trade as 0.049 of YES; charting it as 0.951
    // would put a 90-cent error on a 5-cent market.
    assert.equal(trades[1]!.yesPrice, 0.049);
    assert.equal(trades[1]!.noPrice, 0.951);
    assert.equal(trades[1]!.count, 0.01);
  });

  it('reads the ISO timestamp as unix seconds', () => {
    assert.equal(trades[0]!.ts, 1787031289);
    assert.equal(new Date(trades[0]!.ts * 1000).toISOString(), '2026-08-18T05:34:49.000Z');
  });

  it('names each fill by its transaction hash and its leg', () => {
    assert.equal(trades[0]!.tradeId, '0x4d20a10735cc659e19c7121ef9a4fa550192c5792bb187b78b01908f2a66844d');
    assert.equal(trades[0]!.ticker, 'big-game-champion-2027~26936');
    assert.equal(trades[1]!.ticker, 'big-game-champion-2027~26939');
  });

  it('keeps two fills of one resting order apart, which the cursor would not', () => {
    // The cursor decodes to {orderId, createdAt} and repeats across fills: 50
    // live rows carried 44 distinct cursors and 50 distinct hashes.
    const shared = 'eyJvcmRlcklkIjoyMjIxNDgzNjUwLCJjcmVhdGVkQXQiOiIyMDI2LTA4LTE2VDE1OjMzOjExLjAwMFoifQ==';
    const both = normaliseTrades({
      edges: [
        {
          cursor: shared,
          node: {
            transactionHash: '0xaaa',
            amountFilled: '10000000000000000',
            priceExecuted: '70000000000000000',
            timestamp: '2026-08-16T15:33:11.000Z',
            outcome: { index: 1, name: 'Yes' },
          },
        },
        {
          cursor: shared,
          node: {
            transactionHash: '0xbbb',
            amountFilled: '107240000000000000000',
            priceExecuted: '70000000000000000',
            timestamp: '2026-08-16T15:33:11.000Z',
            outcome: { index: 1, name: 'Yes' },
          },
        },
      ],
    });
    assert.equal(both.length, 2);
    assert.equal(new Set(both.map((t) => t.tradeId)).size, 2);
  });

  it('states no aggressor, because the venue documents none', () => {
    // `quoteType` was read as the taker's direction until one leg was seen
    // printing Yes/ASK and Yes/BID at an identical price, which that reading
    // cannot produce. An unknown side prints as unknown.
    assert.equal(trades[0]!.takerSide, '');
    assert.equal(trades[1]!.takerSide, '');
    assert.equal(trades[0]!.isBlockTrade, false);
  });

  it('drops a row it cannot read rather than tape a zero-priced fill', () => {
    const broken = normaliseTrades({
      edges: [
        { node: { transactionHash: '0x1', amountFilled: '1.5', priceExecuted: '70000000000000000', timestamp: '2026-08-18T05:34:49.000Z' } },
        { node: { transactionHash: '0x2', amountFilled: '10000000000000000', priceExecuted: null, timestamp: '2026-08-18T05:34:49.000Z' } },
        { node: { transactionHash: '0x3', amountFilled: '10000000000000000', priceExecuted: '70000000000000000', timestamp: 'never' } },
      ],
    });
    assert.deepEqual(broken, []);
  });

  it('falls back to the leg it was asked about when a row omits its own', () => {
    const bare = normaliseTrades(
      {
        edges: [
          {
            node: {
              transactionHash: '0x9',
              amountFilled: '10000000000000000',
              priceExecuted: '70000000000000000',
              timestamp: '2026-08-18T05:34:49.000Z',
              outcome: { index: 1, name: 'Yes' },
            },
          },
        ],
      },
      'big-game-champion-2027~26936',
    );
    assert.equal(bare[0]!.ticker, 'big-game-champion-2027~26936');
  });
});

/* ----------------------------------------------------------------- history */

describe('bucketSamples', () => {
  // Real 10-minute samples from the category's `_1D` series, in percent.
  const points = [
    { x: 1786947000, y: 2.0 },
    { x: 1786947600, y: 2.0 },
    { x: 1786948200, y: 3.0 },
    { x: 1786948800, y: 1.5 },
  ];

  it('reads the samples as percentages and reports dollars', () => {
    const [bar] = bucketSamples([{ x: 1786947000, y: 2.0 }], 60);
    assert.equal(bar!.close, 0.02);
  });

  it('takes open, high, low and close from the samples in the period', () => {
    const candles = bucketSamples(points, 60);
    assert.equal(candles.length, 1);
    assert.equal(candles[0]!.open, 0.02);
    assert.equal(candles[0]!.high, 0.03);
    assert.equal(candles[0]!.low, 0.015);
    assert.equal(candles[0]!.close, 0.015);
    assert.equal(candles[0]!.traded, true);
  });

  it('stamps a bar with the period it ends, so venues share an x-axis', () => {
    const candles = bucketSamples(points, 60);
    assert.equal(candles[0]!.time, 1786950000);
    assert.equal(candles[0]!.time % 3600, 0);
  });

  it('states no size, because a probability sample carries none', () => {
    const candles = bucketSamples(points, 60);
    assert.equal(candles[0]!.volume, null);
    assert.equal(candles[0]!.openInterest, null);
    assert.equal(candles[0]!.bid, null);
    assert.equal(candles[0]!.ask, null);
  });

  it('bridges a quiet stretch with bars marked as untraded', () => {
    const gapped = bucketSamples(
      [
        { x: 1786947000, y: 2.0 },
        { x: 1786958000, y: 4.0 },
      ],
      60,
    );
    assert.equal(gapped.length, 4);
    assert.deepEqual(gapped.map((c) => c.traded), [true, false, false, true]);
    // A carried-forward bar holds the last close rather than inventing a move.
    assert.equal(gapped[1]!.open, 0.02);
    assert.equal(gapped[1]!.close, 0.02);
  });

  it('ignores a sample the series left blank', () => {
    const candles = bucketSamples([{ x: 1786947000, y: null }, { x: null, y: 2.0 }], 60);
    assert.deepEqual(candles, []);
  });
});

/* ---------------------------------------------------------------- taxonomy */

describe('normaliseSeries', () => {
  it('names a tag by the id the venue’s own filter accepts', () => {
    const series = normaliseSeries({
      id: '4',
      name: 'Sports',
      open: 635,
      parent: null,
      children: [
        { id: '113', name: 'World Cup' },
        { id: '14', name: 'Soccer' },
      ],
    });
    assert.equal(series.venue, 'predictfun');
    assert.equal(series.ticker, '4');
    assert.equal(series.title, 'Sports');
    // A top-level tag files under itself, so filtering on it keeps its children.
    assert.equal(series.category, 'Sports');
    assert.deepEqual(series.tags, ['World Cup', 'Soccer']);
    assert.equal(series.frequency, '');
  });

  it('files a sub-tag under its parent', () => {
    const weather = normaliseSeries({
      id: '410',
      name: 'Weather',
      open: 38,
      parent: { id: '13', name: 'Culture' },
      children: [],
    });
    assert.equal(weather.category, 'Culture');
    assert.deepEqual(weather.tags, []);
  });
});
