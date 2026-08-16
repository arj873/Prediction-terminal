/**
 * Polymarket US normalisation tests.
 *
 * The fixtures are trimmed copies of real `gateway.polymarket.us` responses,
 * so the `{value, currency}` money objects and the decimal-string quantities
 * are exactly what the upstream sends — including the inconsistent
 * `outcomes`/`outcomePrices` pair that the normaliser deliberately ignores.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  applyBbo,
  money,
  normaliseEvent,
  normaliseMarket,
  normaliseOrderBook,
  seriesFromEvent,
  stripDates,
  type RawUsBbo,
  type RawUsMarket,
} from '../src/server/sources/polymarketus.js';

/* ---------------------------------------------------------------- coercion */

describe('money', () => {
  it('unwraps a {value, currency} object', () => {
    assert.equal(money({ value: '0.4600', currency: 'USD' }), 0.46);
  });

  it('reads a bare decimal string, as quantities arrive', () => {
    assert.equal(money('64154.0000'), 64154);
  });

  it('returns null for absent or unparseable values', () => {
    assert.equal(money(undefined), null);
    assert.equal(money(null), null);
    assert.equal(money(''), null);
    assert.equal(money({ value: '' }), null);
    assert.equal(money('n/a'), null);
  });

  it('keeps a real zero — a settled contract can be worth nothing', () => {
    assert.equal(money('0'), 0);
  });
});

/* ----------------------------------------------------------------- markets */

describe('normaliseMarket', () => {
  // Real shape, trimmed. Note `outcomePrices`: on this endpoint it holds
  // [askToBuyYes, askToBuyNo], and in a nested event listing the *same market*
  // holds [bestBid, bestAsk]. Neither is read.
  const raw: RawUsMarket = {
    slug: 'tec-mlb-nlchamp-2026-09-27-lad',
    question: 'National League Champion',
    title: 'Los Angeles Dodgers',
    subtitle: '',
    description: 'Will Los Angeles Dodgers win the 2026 National League pennant…',
    category: 'sports',
    marketType: 'futures',
    status: 'MARKET_STATUS_OPEN',
    active: true,
    closed: false,
    startDate: '2026-03-19T14:32:23Z',
    endDate: '2026-11-06T16:20:09Z',
    bestBidQuote: { value: '0.4600' },
    bestAskQuote: { value: '0.4780' },
    outcomes: '["Yes","No"]',
    outcomePrices: '["0.4780","0.54"]',
    marketSides: [
      { description: 'Yes', long: true, price: '0.4780', quote: { value: '0.4780' } },
      { description: 'No', long: false, price: '0.54', quote: { value: '0.54' } },
    ],
  };

  it('quotes from bestBidQuote/bestAskQuote and ignores outcomePrices', () => {
    const market = normaliseMarket(raw);
    assert.equal(market.venue, 'polymarket-us');
    assert.equal(market.yesBid, 0.46);
    assert.equal(market.yesAsk, 0.478);
    assert.equal(market.mid, 0.469);
  });

  it('is unmoved by outcomePrices meaning something else', () => {
    // Same market, the event-listing spelling of the same fields. The quote
    // must not change with the endpoint it was read from.
    const listing = normaliseMarket({ ...raw, outcomePrices: '["0.4600","0.4780"]' });
    assert.equal(listing.yesBid, 0.46);
    assert.equal(listing.yesAsk, 0.478);
  });

  it('is unmoved by the outcomes array being ordered either way', () => {
    // The gateway labels one market ["Yes","No"] and its neighbour ["No","Yes"]
    // with both price arrays still in the same order.
    const flipped = normaliseMarket({ ...raw, outcomes: '["No","Yes"]' });
    assert.equal(flipped.yesBid, 0.46);
    assert.equal(flipped.yesAsk, 0.478);
  });

  it('derives the NO side from the YES book', () => {
    const market = normaliseMarket(raw);
    assert.equal(market.noBid, 0.522);
    assert.equal(market.noAsk, 0.54);
  });

  it('leaves the figures the public catalogue does not publish unstated', () => {
    const market = normaliseMarket(raw);
    // Not zero: the exchange publishes none of these without an API key, and a
    // zero in a volume column reads as a dead market.
    assert.equal(market.volume, null);
    assert.equal(market.volume24h, null);
    assert.equal(market.openInterest, null);
    assert.equal(market.liquidity, null);
    assert.equal(market.lastPrice, null);
    assert.equal(market.change, null);
  });

  it('unwraps the status enum', () => {
    assert.equal(normaliseMarket(raw).status, 'open');
    assert.equal(normaliseMarket({ ...raw, status: 'MARKET_STATUS_CLOSED' }).status, 'closed');
    assert.equal(normaliseMarket({ ...raw, status: undefined, closed: true }).status, 'closed');
  });

  it('labels the legs from the market title and its short side', () => {
    const market = normaliseMarket(raw);
    assert.equal(market.yesSubTitle, 'Los Angeles Dodgers');
    assert.equal(market.noSubTitle, 'No');
    assert.equal(
      normaliseMarket({ ...raw, subtitle: 'to win' }).yesSubTitle,
      'Los Angeles Dodgers · to win',
    );
  });

  it('reports an empty book side as absent, not as a zero quote', () => {
    const market = normaliseMarket({
      ...raw,
      bestBidQuote: { value: '0' },
      bestAskQuote: { value: '' },
    });
    assert.equal(market.yesBid, null);
    assert.equal(market.yesAsk, null);
    assert.equal(market.mid, null);
  });
});

describe('applyBbo', () => {
  const bbo: RawUsBbo = {
    marketData: {
      marketSlug: 'tec-mlb-nlchamp-2026-09-27-lad',
      currentPx: { value: '0.4690' },
      lastTradePx: { value: '0.4670' },
      sharesTraded: '342.0000',
      openInterest: '64154.0000',
      bestBid: { value: '0.4600' },
      bestAsk: { value: '0.4780' },
    },
  };

  const base = normaliseMarket({
    slug: 'tec-mlb-nlchamp-2026-09-27-lad',
    bestBidQuote: { value: '0.4000' },
    bestAskQuote: { value: '0.5000' },
  });

  it('prefers the live book over the catalogue snapshot', () => {
    const market = applyBbo(base, bbo);
    assert.equal(market.yesBid, 0.46);
    assert.equal(market.yesAsk, 0.478);
    assert.equal(market.noBid, 0.522);
  });

  it('fills in the last print, open interest and lifetime volume', () => {
    const market = applyBbo(base, bbo);
    assert.equal(market.lastPrice, 0.467);
    assert.equal(market.openInterest, 64154);
    assert.equal(market.volume, 342);
    // Nothing public breaks volume down by day, so this stays unstated.
    assert.equal(market.volume24h, null);
  });

  it('leaves the market untouched when the bbo is empty', () => {
    assert.deepEqual(applyBbo(base, {}), base);
  });
});

/* ------------------------------------------------------------------ events */

describe('seriesFromEvent', () => {
  it('prefers the series the exchange states', () => {
    assert.equal(seriesFromEvent({ slug: 'mlb-champ-2026-09-27', seriesSlug: 'mlb-2026' }), 'mlb-2026');
  });

  it('falls back to the slug stem, which is where the FOMC book lives', () => {
    // The exchange states no series on its rate or election books, and the two
    // October and December FOMC events have to group together regardless.
    assert.equal(seriesFromEvent({ slug: 'usfed-fomc-2026-10-28' }), 'usfed-fomc');
    assert.equal(seriesFromEvent({ slug: 'usfed-fomc-2026-12-09' }), 'usfed-fomc');
    assert.equal(seriesFromEvent({ slug: 'jerpowgov' }), 'jerpowgov');
  });
});

describe('stripDates', () => {
  it('removes a trailing ISO date', () => {
    assert.equal(stripDates('usse-nc-2026-11-03'), 'usse-nc');
    assert.equal(stripDates('mlb-nlchamp-2026-09-27'), 'mlb-nlchamp');
  });

  it('removes a date that is not at the end', () => {
    assert.equal(stripDates('oscars-03-14-2027-bestpic'), 'oscars-bestpic');
    assert.equal(stripDates('oscars-nom-2027-01-31-bestpic'), 'oscars-nom-bestpic');
    assert.equal(stripDates('usgubp-ok-2026-06-16-rep'), 'usgubp-ok-rep');
  });

  it('removes a month named in words, wherever it sits', () => {
    // Without this each month of CPI is its own one-event series, and none of
    // them can pair with the monthly CPI market at either other broker.
    assert.equal(stripDates('uscpi-august-yoy'), 'uscpi-yoy');
    assert.equal(stripDates('uscpi-september-yoy'), 'uscpi-yoy');
  });

  it('keeps a number that is not part of a date', () => {
    // The district is the question. Stripping loose numbers would file all 38
    // Texas House races under one series.
    assert.equal(stripDates('ushr-tx-15-2026-11-03'), 'ushr-tx-15');
    assert.equal(stripDates('ushr-tx-28-2026-11-03'), 'ushr-tx-28');
    assert.equal(stripDates('bbus-s28-winner'), 'bbus-s28-winner');
  });

  it('removes a bare trailing season year', () => {
    assert.equal(stripDates('nfl-2026'), 'nfl');
  });
});

describe('normaliseEvent', () => {
  it('stamps its event and series onto every nested market', () => {
    const event = normaliseEvent({
      slug: 'usfed-fomc-2026-10-28',
      title: 'Fed Decision in October',
      category: 'macro',
      markets: [{ slug: 'a', title: 'No Change', bestBidQuote: { value: '0.70' } }],
    });
    assert.equal(event.venue, 'polymarket-us');
    assert.equal(event.seriesTicker, 'usfed-fomc');
    assert.equal(event.markets[0]!.eventTicker, 'usfed-fomc-2026-10-28');
    assert.equal(event.markets[0]!.seriesTicker, 'usfed-fomc');
  });

  it('claims no exclusivity, because the exchange states none', () => {
    // `EVT` only draws its Σmid arbitrage check when told the legs exclude each
    // other; a guessed flag would put a false arbitrage on screen.
    const event = normaliseEvent({ slug: 'x', markets: [{ slug: 'a' }, { slug: 'b' }] });
    assert.equal(event.mutuallyExclusive, false);
  });
});

/* -------------------------------------------------------------------- book */

describe('normaliseOrderBook', () => {
  const raw = {
    marketData: {
      marketSlug: 'tec-mlb-nlchamp-2026-09-27-lad',
      bids: [
        { px: { value: '0.4400' }, qty: '505.0000' },
        { px: { value: '0.4600' }, qty: '150.0000' },
        { px: { value: '0.4360' }, qty: '3458.0000' },
      ],
      offers: [
        { px: { value: '0.4800' }, qty: '998.0000' },
        { px: { value: '0.4780' }, qty: '11000.0000' },
        { px: { value: '0.4790' }, qty: '60.0000' },
      ],
    },
  };

  it('sorts bids best-first and offers cheapest-first', () => {
    const book = normaliseOrderBook(raw, 'slug');
    assert.deepEqual(book.yes.map((l) => l.price), [0.46, 0.44, 0.436]);
    assert.deepEqual(book.yesAsks.map((l) => l.price), [0.478, 0.479, 0.48]);
  });

  it('derives the NO ladder and the top of book', () => {
    const book = normaliseOrderBook(raw, 'slug');
    assert.equal(book.no[0]!.price, 0.522);
    assert.equal(book.bestYesBid, 0.46);
    assert.equal(book.bestYesAsk, 0.478);
    assert.equal(book.spread, 0.018);
    assert.equal(book.mid, 0.469);
    assert.equal(book.venue, 'polymarket-us');
  });

  it('reports nulls rather than a fake quote for an empty book', () => {
    const book = normaliseOrderBook({}, 'slug');
    assert.deepEqual(book.yes, []);
    assert.equal(book.bestYesAsk, null);
    assert.equal(book.mid, null);
  });

  it('drops zero-price and zero-size rows', () => {
    const book = normaliseOrderBook(
      {
        marketData: {
          bids: [
            { px: { value: '0' }, qty: '100' },
            { px: { value: '0.5' }, qty: '0' },
            { px: { value: '0.4' }, qty: '10' },
          ],
        },
      },
      'slug',
    );
    assert.equal(book.yes.length, 1);
    assert.equal(book.yes[0]!.price, 0.4);
  });
});
