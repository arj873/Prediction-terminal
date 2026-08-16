/**
 * Option source normalisation and board derivation.
 *
 * The parsers here have the same failure mode as the entertainment ones: an
 * upstream answers `200 OK`, the shape parses, and the numbers are quietly
 * wrong. Three cases in particular produce a chain that renders perfectly and
 * means something else entirely, and each has a test below:
 *
 *   * **A Deribit inverse mark is quoted in BTC.** `0.0119` is not a price in
 *     dollars, it is 0.0119 BTC — about $757. Forgetting the conversion puts a
 *     board of cent-priced contracts on screen with plausible Greeks.
 *   * **Deribit quotes volatility in percentage points.** `35.34` is 35.34%,
 *     and feeding it to the model as 3,534% prices every contract at its
 *     ceiling.
 *   * **Nasdaq's table carries the year only on its group-header rows.** The
 *     data rows say `"Sep 18"`. Parsing them independently of the header is how
 *     a chain ends up dated to the wrong year, which shifts every time to
 *     expiry and therefore every Greek.
 */

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, it } from 'node:test';
import {
  buildBoard,
  normaliseChart,
  parseInstrument,
  resolveSymbol,
  supports,
  symbolOfContract,
} from '../src/server/sources/deribit.js';
import {
  occSymbol,
  parseExpiryGroup,
  parseLastTrade,
  parseNasdaqChain,
  parseOcc,
  parseYahooOptions,
} from '../src/server/sources/equityoptions.js';
import {
  atmVol,
  carryFor,
  chainFor,
  enrich,
  expiriesOf,
  midOf,
  positioningFor,
  resolveExpiry,
  riskReversal,
  surfaceFor,
  type BoardQuote,
  type OptionBoard,
} from '../src/server/sources/optionboard.js';

const deribitFixture = JSON.parse(
  readFileSync(new URL('./fixtures/deribit-greeks.json', import.meta.url), 'utf8'),
);
const nasdaqFixture = JSON.parse(
  readFileSync(new URL('./fixtures/nasdaq-optionchain.json', import.meta.url), 'utf8'),
);
/** A page spanning several expiry groups — how the expiry strip gets built. */
const nasdaqExpiriesFixture = JSON.parse(
  readFileSync(new URL('./fixtures/nasdaq-expiries.json', import.meta.url), 'utf8'),
);

/* ------------------------------------------------------------------ deribit */

describe('deribit symbol resolution', () => {
  it('accepts the spellings a person actually types', () => {
    assert.equal(resolveSymbol('btc'), 'BTC');
    assert.equal(resolveSymbol('BTC-USD'), 'BTC');
    assert.equal(resolveSymbol('bitcoin'), 'BTC');
    assert.equal(resolveSymbol('XBT'), 'BTC');
    assert.equal(resolveSymbol('solana'), 'SOL');
  });

  it('declines underlyings Deribit does not list options on', () => {
    assert.equal(resolveSymbol('AAPL'), null);
    assert.equal(resolveSymbol('DOGE'), null);
    assert.equal(supports('AAPL'), false);
    assert.equal(supports('ETH'), true);
  });
});

describe('deribit instrument names', () => {
  it('splits an inverse contract', () => {
    assert.deepEqual(parseInstrument('BTC-25DEC26-104000-P'), {
      base: 'BTC',
      currency: 'BTC',
      expiryLabel: '25DEC26',
      strike: 104000,
      type: 'put',
    });
  });

  it('splits a linear USDC contract, where the head carries the quote currency', () => {
    assert.deepEqual(parseInstrument('SOL_USDC-17AUG26-66-C'), {
      base: 'SOL',
      currency: 'USDC',
      expiryLabel: '17AUG26',
      strike: 66,
      type: 'call',
    });
  });

  it('rejects anything that is not one', () => {
    assert.equal(parseInstrument('BTC-PERPETUAL'), null);
    assert.equal(parseInstrument('AAPL260918C00300000'), null);
    assert.equal(parseInstrument('BTC-25DEC26-104000-X'), null);
  });

  it('routes a contract back to its underlying', () => {
    assert.equal(symbolOfContract('BTC-25DEC26-104000-P'), 'BTC');
    assert.equal(symbolOfContract('SOL_USDC-17AUG26-66-C'), 'SOL');
    assert.equal(symbolOfContract('AAPL260918C00300000'), null);
  });
});

describe('deribit board', () => {
  const index = deribitFixture.tickers[0].index_price as number;
  const board = buildBoard('BTC', {
    instruments: deribitFixture.instruments,
    summaries: deribitFixture.summaries,
    index,
  });

  it('normalises every listed contract', () => {
    assert.equal(board.quotes.length, deribitFixture.instruments.length);
    assert.equal(board.assetClass, 'crypto');
    assert.equal(board.currency, 'USD');
    assert.equal(board.venue, 'Deribit');
    assert.equal(board.spot, index);
  });

  it('converts an inverse mark from BTC into dollars', () => {
    // The fixture's first contract marks at 0.01197855 BTC. In dollars that is
    // ~$757; read as a dollar price it would be about one cent.
    const quote = board.quotes.find((q) => q.contract === 'BTC-30OCT26-76000-C')!;
    const expected = 0.01197855 * index;
    assert.ok(Math.abs(quote.mark! - expected) < 1e-6, `mark ${quote.mark} != ${expected}`);
    assert.ok(quote.mark! > 500 && quote.mark! < 1200, 'a converted mark should be in dollars');
    assert.ok(Math.abs(quote.bid! - 0.0115 * index) < 1e-6);
    assert.ok(Math.abs(quote.ask! - 0.0125 * index) < 1e-6);
  });

  it('reads volatility as a decimal, not as percentage points', () => {
    const quote = board.quotes.find((q) => q.contract === 'BTC-30OCT26-76000-C')!;
    assert.ok(Math.abs(quote.venueIv! - 0.3534) < 1e-9, `iv was ${quote.venueIv}`);
  });

  it("takes the venue's per-expiry forward", () => {
    const quote = board.quotes.find((q) => q.contract === 'BTC-30OCT26-76000-C')!;
    assert.equal(quote.venueForward, 63762.6);
  });

  it('derives the discount factor from the basis, not from `interest_rate`', () => {
    // Every `interest_rate` in the payload is 0.0, while the forward sits above
    // the index. Believing the field would leave the discount at 1 and every
    // long-dated delta disagreeing with Deribit's own.
    const quote = board.quotes.find((q) => q.contract === 'BTC-30OCT26-76000-C')!;
    assert.equal(deribitFixture.summaries[0].interest_rate, 0);
    assert.ok(Math.abs(quote.venueDiscount! - index / 63762.6) < 1e-12);
    assert.ok(quote.venueDiscount! < 1, 'a forward above spot must discount below 1');
  });

  it('labels the coin-margined board so the conversion is not invisible', () => {
    assert.match(board.note ?? '', /coin-margined/);
    assert.match(board.note ?? '', /converted to USD/);
  });

  it('drops contracts with no quote rather than inventing one', () => {
    const partial = buildBoard('BTC', {
      instruments: deribitFixture.instruments,
      summaries: [deribitFixture.summaries[0]],
      index,
    });
    assert.equal(partial.quotes.length, 1);
  });

  it('produces nothing at all without an index to convert against', () => {
    const blind = buildBoard('BTC', {
      instruments: deribitFixture.instruments,
      summaries: deribitFixture.summaries,
      index: null,
    });
    // Better an empty board the route reports than a board of BTC-denominated
    // numbers presented as dollars.
    assert.equal(blind.quotes.length, 0);
  });
});

describe('deribit chart data', () => {
  it('zips the positional arrays into candles', () => {
    const candles = normaliseChart({
      ticks: [3000, 1000, 2000],
      open: [3, 1, 2],
      high: [3.5, 1.5, 2.5],
      low: [2.5, 0.5, 1.5],
      close: [3.2, 1.2, 2.2],
      volume: [30, 10, 20],
    });
    assert.equal(candles.length, 3);
    // Sorted ascending, and milliseconds become seconds.
    assert.deepEqual(candles.map((c) => c.time), [1, 2, 3]);
    assert.equal(candles[0]!.close, 1.2);
  });

  it('skips buckets with no close', () => {
    const candles = normaliseChart({ ticks: [1000, 2000], close: [1, Number.NaN] });
    assert.equal(candles.length, 1);
  });

  it('survives an empty response', () => {
    assert.deepEqual(normaliseChart({}), []);
  });
});

/* ------------------------------------------------------------------- equity */

describe('OCC contract symbols', () => {
  it('builds the symbol both providers key on', () => {
    assert.equal(occSymbol('AAPL', '2026-09-18', 'call', 300), 'AAPL260918C00300000');
    assert.equal(occSymbol('AAPL', '2026-09-18', 'put', 12.5), 'AAPL260918P00012500');
    // Strikes in thousandths, zero-padded to eight, so half-dollar strikes and
    // four-figure ones sort and compare correctly as text.
    assert.equal(occSymbol('SPX', '2026-01-16', 'call', 6000), 'SPX260116C06000000');
  });

  it('round-trips', () => {
    for (const [symbol, date, type, strike] of [
      ['AAPL', '2026-09-18', 'call', 300],
      ['SPY', '2026-12-31', 'put', 0.5],
      ['NVDA', '2027-06-17', 'call', 1234.5],
    ] as const) {
      const built = occSymbol(symbol, date, type, strike)!;
      assert.deepEqual(parseOcc(built), { symbol, date, type, strike });
    }
  });

  it('rejects what is not an OCC symbol', () => {
    assert.equal(parseOcc('BTC-25DEC26-104000-C'), null);
    assert.equal(parseOcc('AAPL'), null);
    assert.equal(occSymbol('AAPL', 'Sep 18', 'call', 300), null);
  });
});

describe('nasdaq chain parsing', () => {
  const parsed = parseNasdaqChain(nasdaqFixture, 'AAPL');

  it('takes the year from the group header, which is the only place it exists', () => {
    // Data rows say `"Sep 18"` and nothing more. Reading them alone would put
    // the chain in whatever year the parser assumed.
    assert.deepEqual(parsed.dates, ['2026-09-18']);
    assert.ok(parsed.quotes.length > 0);
    for (const quote of parsed.quotes) {
      assert.equal(new Date(quote.expiry * 1000).toISOString().slice(0, 10), '2026-09-18');
    }
  });

  it('expires the board at 16:00 New York, not at UTC midnight', () => {
    // September is EDT, so 16:00 local is 20:00Z.
    assert.equal(
      new Date(parsed.quotes[0]!.expiry * 1000).toISOString(),
      '2026-09-18T20:00:00.000Z',
    );
  });

  it('emits both legs per row, keyed by OCC symbol', () => {
    const call = parsed.quotes.find((q) => q.contract === 'AAPL260918C00240000');
    const put = parsed.quotes.find((q) => q.contract === 'AAPL260918P00240000');
    assert.ok(call, 'missing the 240 call');
    assert.ok(put, 'missing the 240 put');
    assert.equal(call!.bid, 65.1);
    assert.equal(call!.ask, 67.65);
    assert.equal(call!.last, 66.92);
    assert.equal(call!.openInterest, 3261);
    assert.equal(put!.bid, 0.11);
    assert.equal(put!.openInterest, 11355);
  });

  it('reads a change of -0.07 as a number, not as a missing value', () => {
    const put = parsed.quotes.find((q) => q.contract === 'AAPL260918P00240000')!;
    assert.equal(put.change, -0.07);
  });

  it("turns Nasdaq's `--` into null rather than zero", () => {
    // A strike with no bid is unquoted, not bid at nothing.
    const dashed = parseNasdaqChain(
      {
        data: {
          table: {
            rows: [
              { expirygroup: 'September 18, 2026' },
              {
                expirygroup: '',
                strike: '250.00',
                c_Bid: '--',
                c_Ask: '--',
                c_Last: '--',
                c_Volume: '--',
                c_Openinterest: '5',
                p_Bid: '1.00',
                p_Ask: '1.10',
                p_Last: '--',
                p_Volume: '--',
                p_Openinterest: '--',
              },
            ],
          },
        },
      },
      'AAPL',
    );
    const call = dashed.quotes.find((q) => q.type === 'call')!;
    const put = dashed.quotes.find((q) => q.type === 'put')!;
    assert.equal(call.bid, null);
    assert.equal(call.ask, null);
    assert.equal(call.volume, null);
    assert.equal(call.openInterest, 5);
    assert.equal(put.bid, 1);
    assert.equal(put.openInterest, null);
  });

  it('drops a data row that arrives before any header', () => {
    // Without a header there is no year, and a guessed year moves every expiry.
    const orphan = parseNasdaqChain(
      { data: { table: { rows: [{ expirygroup: '', strike: '100.00', c_Bid: '1.00' }] } } },
      'AAPL',
    );
    assert.equal(orphan.quotes.length, 0);
  });

  it('walks several expiry groups in one page, in order', () => {
    // Nasdaq has no endpoint that lists expiries, so the strip is built by
    // crawling the chain and reading the group headers as they go past. This
    // is the case the single-expiry fixture cannot exercise: the parser has to
    // carry the current date across rows and switch on each new header.
    const parsed = parseNasdaqChain(nasdaqExpiriesFixture, 'AAPL');
    assert.deepEqual(parsed.dates, ['2026-08-17', '2026-08-19', '2026-08-21']);

    // Only the first group had data rows behind it; the trailing headers
    // contribute an expiry each and no contracts.
    assert.deepEqual(
      [...new Set(parsed.quotes.map((q) => new Date(q.expiry * 1000).toISOString().slice(0, 10)))],
      ['2026-08-17'],
    );
    assert.equal(parsed.quotes.length, 4);
  });

  it('reads the underlying price out of the last-trade banner', () => {
    assert.equal(parsed.spot, 305.93);
    assert.equal(parseLastTrade('LAST TRADE: $1,234.56 (AS OF AUG 13, 2026)'), 1234.56);
    assert.equal(parseLastTrade('LAST TRADE: N/A'), null);
    assert.equal(parseLastTrade(null), null);
  });

  it('parses the group header date', () => {
    assert.equal(parseExpiryGroup('September 18, 2026'), '2026-09-18');
    assert.equal(parseExpiryGroup('January 2, 2027'), '2027-01-02');
    assert.equal(parseExpiryGroup('Septembr 18, 2026'), null);
    assert.equal(parseExpiryGroup(''), null);
  });
});

describe('yahoo options parsing', () => {
  const body = {
    optionChain: {
      result: [
        {
          underlyingSymbol: 'AAPL',
          expirationDates: [1789689600, 1790294400],
          quote: { regularMarketPrice: 305.93, longName: 'Apple Inc.', currency: 'USD' },
          options: [
            {
              expirationDate: 1789689600,
              calls: [
                {
                  contractSymbol: 'AAPL260918C00300000',
                  strike: 300,
                  bid: 12.1,
                  ask: 12.4,
                  lastPrice: 12.25,
                  change: 0.4,
                  volume: 1200,
                  openInterest: 5400,
                  expiration: 1789689600,
                },
              ],
              puts: [
                {
                  contractSymbol: 'AAPL260918P00300000',
                  strike: 300,
                  bid: 6.1,
                  ask: 6.3,
                  lastPrice: 6.2,
                  volume: 800,
                  openInterest: 3100,
                  expiration: 1789689600,
                },
              ],
            },
          ],
        },
      ],
    },
  };

  it('normalises both legs and the underlying', () => {
    const { board, expiries } = parseYahooOptions(body, 'AAPL');
    assert.equal(board.symbol, 'AAPL');
    assert.equal(board.name, 'Apple Inc.');
    assert.equal(board.spot, 305.93);
    assert.equal(board.contractSize, 100);
    assert.equal(board.quotes.length, 2);
    assert.deepEqual(expiries, [1789689600, 1790294400]);
  });

  it('moves the expiry from UTC midnight to the New York close', () => {
    // Yahoo dates an expiry at 00:00Z. The contract stops trading ~16 hours
    // later, which on a same-day chain is its entire remaining life.
    const { board } = parseYahooOptions(body, 'AAPL');
    const iso = new Date(board.quotes[0]!.expiry * 1000).toISOString();
    assert.ok(iso.endsWith('T20:00:00.000Z') || iso.endsWith('T21:00:00.000Z'), iso);
    assert.ok(board.quotes[0]!.expiry > 1789689600);
  });

  it("does not adopt Yahoo's implied volatility", () => {
    // Yahoo publishes one, against its own undisclosed carry. Mixing it with a
    // parity-fitted forward would put two disagreeing vols on one screen.
    const { board } = parseYahooOptions(body, 'AAPL');
    for (const quote of board.quotes) assert.equal(quote.venueIv, null);
  });

  it('reports an empty result as a not-found rather than a crash', () => {
    assert.throws(() => parseYahooOptions({ optionChain: { result: [] } }, 'NOPE'), /no option board/);
  });
});

/* -------------------------------------------------------------- derivation */

/** A small synthetic board with two expiries, so the derivations have shape. */
function syntheticBoard(): OptionBoard {
  const now = 1_800_000_000;
  const quotes: BoardQuote[] = [];

  for (const [expiry, vol] of [
    [now + 30 * 86400, 0.3],
    [now + 90 * 86400, 0.35],
  ] as const) {
    for (const strike of [80, 90, 100, 110, 120]) {
      for (const type of ['call', 'put'] as const) {
        // Parity-consistent prices around a forward of 101 and DF 0.99.
        const years = (expiry - now) / (365 * 86400);
        const forward = 100 * Math.exp(0.04 * years);
        const discount = Math.exp(-0.04 * years);
        const intrinsic = Math.max(0, type === 'call' ? forward - strike : strike - forward);
        const time = 6 * Math.exp(-(((strike - forward) / 40) ** 2)) * Math.sqrt(years);
        const mid = (intrinsic + time) * discount;
        quotes.push({
          contract: `X-${expiry}-${strike}-${type}`,
          type,
          strike,
          expiry,
          bid: mid * 0.98,
          ask: mid * 1.02,
          last: mid,
          mark: mid,
          change: null,
          volume: strike === 100 ? 500 : 100,
          openInterest: type === 'call' ? 1000 : 800,
          venueIv: vol,
          venueForward: forward,
          venueDiscount: discount,
        });
      }
    }
  }

  return {
    symbol: 'X',
    name: 'Test',
    assetClass: 'stock',
    currency: 'USD',
    venue: 'TEST',
    source: 'test',
    contractSize: 100,
    spot: 100,
    quotes,
  };
}

const NOW = 1_800_000_000;

describe('expiriesOf', () => {
  it('summarises each expiry and orders them soonest first', () => {
    const expiries = expiriesOf(syntheticBoard(), NOW);
    assert.equal(expiries.length, 2);
    assert.equal(expiries[0]!.daysToExpiry, 30);
    assert.equal(expiries[1]!.daysToExpiry, 90);
    assert.equal(expiries[0]!.contracts, 10);
    assert.equal(expiries[0]!.openInterest, 5 * 1000 + 5 * 800);
  });

  it('rounds days rather than flooring them', () => {
    // An expiry 23h50m away is tomorrow, not today.
    const board = syntheticBoard();
    board.quotes = [{ ...board.quotes[0]!, expiry: NOW + 86_400 - 600 }];
    assert.equal(expiriesOf(board, NOW)[0]!.daysToExpiry, 1);
  });
});

describe('resolveExpiry', () => {
  const expiries = expiriesOf(syntheticBoard(), NOW);

  it('defaults to the front expiry', () => {
    assert.equal(resolveExpiry(expiries, undefined, NOW).daysToExpiry, 30);
  });

  it('skips an expiry that has already passed', () => {
    // A chain fetched at 16:05 on expiry day still lists this morning's board;
    // defaulting to it would show frozen quotes.
    const later = NOW + 31 * 86400;
    assert.equal(resolveExpiry(expiries, undefined, later).daysToExpiry, 90);
  });

  it('takes an exact date', () => {
    assert.equal(resolveExpiry(expiries, expiries[1]!.date, NOW).expiry, expiries[1]!.expiry);
  });

  it('takes a horizon and picks the nearest listed expiry', () => {
    assert.equal(resolveExpiry(expiries, '30d', NOW).daysToExpiry, 30);
    assert.equal(resolveExpiry(expiries, '6w', NOW).daysToExpiry, 30);
    assert.equal(resolveExpiry(expiries, '3m', NOW).daysToExpiry, 90);
    assert.equal(resolveExpiry(expiries, '1y', NOW).daysToExpiry, 90);
  });

  it('breaks a tie toward the nearer expiry', () => {
    // `2m` is 60 days, exactly between the 30- and 90-day expiries. The front
    // one wins, which is both the more liquid and the less surprising answer.
    assert.equal(resolveExpiry(expiries, '2m', NOW).daysToExpiry, 30);
  });

  it('takes a one-based index into the strip', () => {
    assert.equal(resolveExpiry(expiries, '1', NOW).daysToExpiry, 30);
    assert.equal(resolveExpiry(expiries, '2', NOW).daysToExpiry, 90);
  });

  it('names the listed expiries when nothing matches', () => {
    assert.throws(() => resolveExpiry(expiries, '2030-01-01', NOW), /No listed expiry matches/);
    assert.throws(() => resolveExpiry([], undefined, NOW), /No option expiries/);
  });
});

describe('midOf', () => {
  const base: BoardQuote = {
    contract: 'X',
    type: 'call',
    strike: 100,
    expiry: NOW,
    bid: null,
    ask: null,
    last: null,
    mark: null,
    change: null,
    volume: null,
    openInterest: null,
    venueIv: null,
    venueForward: null,
    venueDiscount: null,
  };

  it('takes the mid of a two-sided book', () => {
    assert.equal(midOf({ ...base, bid: 4, ask: 4.4 }), 4.2);
  });

  it('does NOT halve a one-sided book, unlike a binary contract', () => {
    // A Kalshi contract offered at 1c with no bid is worth somewhere in
    // [0, 1c], because its payoff is bounded. An option bid at 4.20 with no
    // offer is worth about 4.20 — there is no ceiling to split against.
    assert.equal(midOf({ ...base, bid: 4.2 }), 4.2);
    assert.equal(midOf({ ...base, ask: 4.2 }), 4.2);
  });

  it("prefers the venue's mark to a one-sided book", () => {
    assert.equal(midOf({ ...base, bid: 4, mark: 4.35 }), 4.35);
  });

  it('falls back to the last print, then gives up', () => {
    assert.equal(midOf({ ...base, last: 3.9 }), 3.9);
    assert.equal(midOf(base), null);
    // Zero is an empty side, not a price of nothing.
    assert.equal(midOf({ ...base, bid: 0, ask: 0 }), null);
  });
});

describe('carryFor', () => {
  const board = syntheticBoard();
  const expiries = expiriesOf(board, NOW);
  const front = expiries[0]!;
  const quotes = board.quotes.filter((q) => q.expiry === front.expiry);

  it("uses the venue's forward when it publishes one", () => {
    const carry = carryFor(quotes, board.spot, front.yearsToExpiry);
    assert.equal(carry.source, 'venue');
    assert.equal(carry.forward, quotes[0]!.venueForward);
  });

  it('falls back to a parity fit when the venue is silent', () => {
    const silent = quotes.map((q) => ({ ...q, venueForward: null, venueDiscount: null }));
    const carry = carryFor(silent, board.spot, front.yearsToExpiry);
    assert.equal(carry.source, 'parity');
    // The synthetic prices were built at a 4% rate, so the fit should find it.
    assert.ok(Math.abs(carry.rate! - 0.04) < 0.01, `rate came back as ${carry.rate}`);
    assert.ok(Math.abs(carry.forward! - quotes[0]!.venueForward!) < 0.05);
  });

  it('says so out loud when it has to assume', () => {
    // One strike, one leg: nothing to regress.
    const bare = [{ ...quotes[0]!, venueForward: null, venueDiscount: null }];
    const carry = carryFor(bare, board.spot, front.yearsToExpiry);
    assert.equal(carry.source, 'assumed');
    assert.ok(carry.forward! > board.spot!, 'an assumed forward still carries');
  });

  it('has nothing to say without a spot', () => {
    assert.equal(carryFor(quotes, null, front.yearsToExpiry).forward, null);
  });
});

describe('enrich', () => {
  const board = syntheticBoard();
  const expiries = expiriesOf(board, NOW);
  const front = expiries[0]!;
  const quotes = board.quotes.filter((q) => q.expiry === front.expiry);
  const carry = carryFor(quotes, board.spot, front.yearsToExpiry);

  it("prefers the venue's volatility and labels it", () => {
    const call = enrich(quotes.find((q) => q.type === 'call' && q.strike === 100)!, board.spot, carry, front.yearsToExpiry);
    assert.equal(call.ivSource, 'venue');
    assert.equal(call.iv, 0.3);
    assert.ok(call.greeks.delta! > 0 && call.greeks.delta! < 1);
    assert.ok(call.greeks.vega! > 0);
    assert.ok(call.greeks.theta! < 0, 'a long option decays');
  });

  it('solves the volatility when the venue publishes none', () => {
    const raw = quotes.find((q) => q.type === 'call' && q.strike === 100)!;
    const solved = enrich({ ...raw, venueIv: null }, board.spot, carry, front.yearsToExpiry);
    assert.equal(solved.ivSource, 'solved');
    assert.ok(solved.iv! > 0);
    // Solved from the mid, so the model reprices the mid it was solved from.
    assert.ok(Math.abs(solved.theo! - solved.mid!) < 1e-6);
  });

  it('splits the premium into intrinsic and extrinsic', () => {
    const itm = enrich(quotes.find((q) => q.type === 'call' && q.strike === 80)!, board.spot, carry, front.yearsToExpiry);
    assert.equal(itm.inTheMoney, true);
    assert.equal(itm.intrinsic, 20);
    assert.ok(Math.abs(itm.intrinsic! + itm.extrinsic! - itm.mid!) < 1e-9);
    assert.equal(itm.breakeven, 80 + itm.mid!);
  });

  it('returns null Greeks, never zeroes, when it cannot price', () => {
    const dead = enrich(
      { ...quotes[0]!, bid: null, ask: null, last: null, mark: null, venueIv: null },
      board.spot,
      carry,
      front.yearsToExpiry,
    );
    assert.equal(dead.iv, null);
    assert.equal(dead.ivSource, null);
    assert.deepEqual(dead.greeks, { delta: null, gamma: null, vega: null, theta: null, rho: null });
  });
});

describe('chainFor', () => {
  const board = syntheticBoard();
  const expiries = expiriesOf(board, NOW);
  const chain = chainFor(board, expiries[0]!, expiries);

  it('splits the board into sorted call and put ladders', () => {
    assert.equal(chain.calls.length, 5);
    assert.equal(chain.puts.length, 5);
    assert.deepEqual(chain.calls.map((c) => c.strike), [80, 90, 100, 110, 120]);
    assert.equal(chain.expiries.length, 2);
  });

  it('carries the forward and its provenance', () => {
    assert.equal(chain.forwardSource, 'venue');
    assert.ok(chain.forward! > 100);
    assert.ok(chain.discountFactor! < 1);
  });

  it('reports an at-the-money volatility', () => {
    assert.ok(Math.abs(chain.atmIv! - 0.3) < 1e-9);
  });
});

describe('atmVol', () => {
  it('interpolates between the strikes bracketing the forward', () => {
    // Reading the nearest strike alone makes the number jump every time spot
    // crosses a rung, which on a $5 board is several times a session.
    const contracts = [
      { strike: 100, type: 'call', iv: 0.2, volume: 0, openInterest: 0 },
      { strike: 110, type: 'call', iv: 0.3, volume: 0, openInterest: 0 },
    ] as never[];
    assert.ok(Math.abs(atmVol(contracts, 105)! - 0.25) < 1e-12);
    assert.ok(Math.abs(atmVol(contracts, 102)! - 0.22) < 1e-12);
  });

  it('has no answer without a forward', () => {
    assert.equal(atmVol([], null), null);
    assert.equal(atmVol([], 100), null);
  });
});

describe('surfaceFor', () => {
  const board = syntheticBoard();
  const expiries = expiriesOf(board, NOW);
  const surface = surfaceFor(board, expiries[0]!, expiries);

  it('gives one smile rung per strike, with both legs', () => {
    assert.equal(surface.smile.length, 5);
    for (const rung of surface.smile) {
      assert.ok(rung.callIv !== null && rung.putIv !== null);
      assert.equal(rung.openInterest, 1800);
    }
  });

  it('builds a term structure across every listed expiry', () => {
    assert.equal(surface.term.length, 2);
    assert.ok(Math.abs(surface.term[0]!.atmIv! - 0.3) < 1e-9);
    assert.ok(Math.abs(surface.term[1]!.atmIv! - 0.35) < 1e-9);
    assert.deepEqual(surface.term.map((t) => t.daysToExpiry), [30, 90]);
  });

  it('reads a flat board as having no skew', () => {
    // Every rung of the synthetic board carries the same vol, so the 25-delta
    // risk reversal is zero rather than absent.
    assert.ok(surface.skew === null || Math.abs(surface.skew) < 1e-9);
  });
});

describe('riskReversal', () => {
  it('measures the put wing against the call wing at 25 delta', () => {
    const contracts = [
      { type: 'put', iv: 0.4, greeks: { delta: -0.25 } },
      { type: 'call', iv: 0.3, greeks: { delta: 0.25 } },
    ] as never[];
    assert.ok(Math.abs(riskReversal(contracts)! - 0.1) < 1e-12);
  });

  it('declines to quote a wing that is not there', () => {
    // A board whose closest contract to 25-delta sits at 60-delta has no wing,
    // and answering anyway would be inventing a number.
    const contracts = [
      { type: 'put', iv: 0.4, greeks: { delta: -0.6 } },
      { type: 'call', iv: 0.3, greeks: { delta: 0.6 } },
    ] as never[];
    assert.equal(riskReversal(contracts), null);
  });
});

describe('positioningFor', () => {
  const board = syntheticBoard();
  const expiries = expiriesOf(board, NOW);
  const positioning = positioningFor(board, expiries[0]!);

  it('totals open interest and volume on both legs', () => {
    assert.equal(positioning.strikes.length, 5);
    assert.equal(positioning.totalCallOpenInterest, 5000);
    assert.equal(positioning.totalPutOpenInterest, 4000);
    assert.equal(positioning.putCallOpenInterest, 0.8);
  });

  it('scales the pain curve by the contract multiplier', () => {
    // Max pain in dollars, not in contracts: 100 shares to a contract.
    for (const rung of positioning.strikes) assert.ok(rung.painPayout! >= 0);
    const middle = positioning.strikes.find((s) => s.strike === 100)!;
    assert.ok(middle.painPayout! > 0);
  });

  it('finds the strike where the least value pays out', () => {
    assert.ok(positioning.maxPain !== null);
    assert.ok(positioning.strikes.some((s) => s.strike === positioning.maxPain));
    const pain = positioning.strikes.find((s) => s.strike === positioning.maxPain)!;
    const cheapest = Math.min(...positioning.strikes.map((s) => s.painPayout!));
    assert.equal(pain.painPayout, cheapest);
  });
});
