/**
 * ForecastEx normalisation tests.
 *
 * Every fixture below is a trimmed copy of a real response: the
 * double-wrapped `body.data` string exactly as the Lambda sends it, catalogue
 * rows with their `null` prices intact, and lines lifted verbatim out of
 * `pairs_20260814.csv` and `daily_prices_20260814.csv` — including the rows
 * that trap a careless parser, where an untraded session writes `0.00` into
 * every price column and leaves `settlement_price` empty.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  distinguishingClauses,
  applySession,
  bucketSamples,
  centralIso,
  eventTickerOf,
  getCandles,
  getOrderBook,
  normaliseEvent,
  normaliseMarket,
  normaliseSeries,
  normaliseTrades,
  num,
  parseArchiveCsv,
  parsePairsCsv,
  sampleEnd,
  sessionDate,
  strikeOf,
  unwrap,
  type RawFexContract,
  type RawFexEnvelope,
  type RawFexPricePoint,
  type RawFexProduct,
} from '../src/server/sources/forecastex.js';

/* --------------------------------------------------------------- fixtures */

const HORC: RawFexContract = {
  contract_id: 'HORC_1126_Republican',
  product_id: 'HORC',
  category: 'Elections',
  event_display_name: 'US House of Representatives Control November 2026',
  question:
    'Will the Republican Party win a majority in the United States House of Representatives in the 2026 general election?',
  expiration_date: '2027-01-04T15:00:00',
  last_trade_date: '2027-01-04T15:00:00',
  payout_date: '2027-01-04T17:00:00',
  open_interest: 1032777,
  exchange_spec_url: 'https://data.forecastex.com/regulatory/HORCOTermsandConditions.pdf',
  last_yes_price: 0.21,
  last_no_price: 0.79,
};

const HORC_DEM: RawFexContract = {
  ...HORC,
  contract_id: 'HORC_1126_Democratic',
  question:
    'Will the Democratic Party win a majority in the United States House of Representatives in the 2026 general election?',
  open_interest: 821649,
  last_yes_price: 0.79,
  last_no_price: 0.21,
};

/** Never traded, and holding no position: 58% of the universe looks like this. */
const DENVER_83: RawFexContract = {
  contract_id: 'UHBKF_081726_83',
  product_id: 'UHBKF',
  category: 'Environmental',
  event_display_name: 'Denver Daily Temperature High August 17 2026',
  question: 'Will the highest temperature in Denver (DEN)(KBKF) exceed 83 F on August 17, 2026?',
  expiration_date: '2026-08-18T02:00:00',
  last_trade_date: '2026-08-18T00:59:00',
  open_interest: 0,
  exchange_spec_url: 'https://data.forecastex.com/regulatory/DailyTemperatureTermsandConditions.pdf',
  last_yes_price: null,
  last_no_price: null,
};

const DENVER_91: RawFexContract = {
  ...DENVER_83,
  contract_id: 'UHBKF_081726_91',
  question: 'Will the highest temperature in Denver (DEN)(KBKF) exceed 91 F on August 17, 2026?',
  open_interest: 2606,
  last_yes_price: 0.98,
  last_no_price: 0.02,
};

/** Four segments, and the reason `strikeOf` reads the last one. */
const ZFFCP: RawFexContract = {
  contract_id: 'ZFFCP_091626_E0X0926_3.4',
  product_id: 'ZFFCP',
  category: 'Conditional',
  event_display_name: 'Fed Decision Impact on CPI September 16 2026',
  question:
    'If the Fed leaves the rate unchanged on September 16, 2026, will the year-over-year change in the US Consumer Price Index exceed 3.4% in September2026?',
  expiration_date: '2026-10-14T07:30:00',
  last_trade_date: '2026-10-14T07:30:00',
  open_interest: 0,
  exchange_spec_url: 'https://data.forecastex.com/regulatory/FFCPITermsandConditions.pdf',
  last_yes_price: null,
  last_no_price: null,
};

/** A ladder that runs the other way — 370 contracts do, and the id cannot tell. */
const ATLANTA_73: RawFexContract = {
  contract_id: 'ULATL_081826_73',
  product_id: 'ULATL',
  category: 'Environmental',
  event_display_name: 'Atlanta Daily Temperature Low August 18 2026',
  question: 'Will the lowest temperature in Atlanta (ATL)(KATL) be below 73 F on August 18, 2026?',
  expiration_date: '2026-08-19T00:00:00',
  last_trade_date: '2026-08-18T22:59:00',
  open_interest: 8,
  exchange_spec_url: 'https://data.forecastex.com/regulatory/DailyTemperatureTermsandConditions.pdf',
  last_yes_price: 0.02,
  last_no_price: 0.98,
};

/** The seven contracts whose ladder includes its own rung. */
const HURRICANES_11: RawFexContract = {
  contract_id: 'HCAB_1226_11',
  product_id: 'HCAB',
  category: 'Environmental',
  event_display_name: 'Atlantic Basin Hurricanes 2026',
  question: 'Will at least 11 hurricanes form in the Atlantic Basin in 2026?',
  expiration_date: '2027-01-04T08:00:00',
  last_trade_date: '2027-01-04T08:00:00',
  open_interest: 155,
  exchange_spec_url: 'https://data.forecastex.com/regulatory/HCABTermsandConditions.pdf',
  last_yes_price: 0.11,
  last_no_price: 0.89,
};

/** Neither a bound nor a direction: "exactly one", not "more than one". */
const DISSENTERS_1: RawFexContract = {
  contract_id: 'DISSN_091626_1',
  product_id: 'DISSN',
  category: 'Government',
  event_display_name: 'Number of FOMC Dissenters September 16 2026',
  question: 'Will exactly one FOMC member dissent at the September 16, 2026 Fed meeting?',
  expiration_date: '2026-09-16T18:00:00',
  last_trade_date: '2026-09-16T18:00:00',
  open_interest: 0,
  exchange_spec_url: 'https://data.forecastex.com/regulatory/DISSTermsandConditions.pdf',
  last_yes_price: null,
  last_no_price: null,
};

const FF: RawFexContract = {
  contract_id: 'FF_091726_3.625',
  product_id: 'FF',
  category: 'Economic Indicators',
  event_display_name: 'US Fed Funds Target Rate September 17 2026',
  question:
    'Will the US Fed Funds Target Rate be set above 3.625% at the FOMC meeting ending September 16, 2026?',
  expiration_date: '2026-09-16T13:00:00',
  last_trade_date: '2026-09-16T13:00:00',
  open_interest: 1325,
  exchange_spec_url: 'https://data.forecastex.com/regulatory/FFTermsandConditions.pdf',
  last_yes_price: 0.29,
  last_no_price: 0.71,
};

const PRODUCTS: Record<string, RawFexProduct> = {
  HORC: {
    product_id: 'HORC',
    name: 'US House of Representatives Control',
    category: 'Elections',
    frequency_type: 'M',
    external_symbol: 'HORC=MAN',
    source_agency: 'United States Congress',
    strike_unit: 'String',
    open_interest: 1854426,
  },
  UHBKF: {
    product_id: 'UHBKF',
    name: 'Denver Daily Temperature High',
    category: 'Environmental',
    frequency_type: 'D',
    external_symbol: 'UHBKF=MAN',
    source_agency: 'Weather Underground',
    strike_unit: 'Decimal',
  },
  ZFFCP: {
    product_id: 'ZFFCP',
    name: 'Fed Decision Impact on CPI',
    category: 'Conditional',
    frequency_type: 'D',
    strike_unit: 'String',
  },
  ULATL: {
    product_id: 'ULATL',
    name: 'Atlanta Daily Temperature Low',
    category: 'Environmental',
    frequency_type: 'D',
    external_symbol: 'ULATL=MAN',
    source_agency: 'Weather Underground',
    strike_unit: 'Decimal',
  },
  HCAB: {
    product_id: 'HCAB',
    name: 'Atlantic Basin Hurricanes',
    category: 'Environmental',
    frequency_type: 'I',
    strike_unit: 'Integer',
  },
  DISSN: {
    product_id: 'DISSN',
    name: 'Number of FOMC Dissenters',
    category: 'Government',
    frequency_type: 'I',
    strike_unit: 'Integer',
  },
  FF: {
    product_id: 'FF',
    name: 'US Fed Funds Target Rate',
    category: 'Economic Indicators',
    frequency_type: 'I',
    external_symbol: 'USFOMC=ECI',
    source_agency: 'U.S. Federal Reserve',
    strike_unit: 'Percentage',
  },
};

const ARCHIVE_CSV = [
  'event_contract,subtype,expiration_date,date,start_price,high_price,low_price,end_price,settlement_price,pair_quantity,open_interest,vwap',
  'HORC_1126_Republican,YES,2027-01-04T15:00:00-06:00,2026-08-14,0.18,0.21,0.18,0.21,0.21,70800,1032677,0.21',
  'HORC_1126_Republican,NO,2027-01-04T15:00:00-06:00,2026-08-14,0.82,0.82,0.79,0.79,0.79,70800,1032677,0.79',
  'FF_091726_3.625,YES,2026-09-16T13:00:00-05:00,2026-08-14,0.29,0.29,0.27,0.27,0.27,30,1325,0.28',
  'ACD_1226_432.0,YES,2027-01-05T08:00:00-06:00,2026-08-14,0.04,0.00,0.00,0.04,0.04,0,105,0.00',
  'UST_1230_54.41,YES,2031-01-10T10:00:00-06:00,2026-08-14,0.00,0.00,0.00,0.00,,0,0,0.00',
].join('\n');

const PAIRS_CSV = [
  'pair_id,event_contract,expiration_date,quantity,yes_price,no_price,pair_time',
  'BVT0EZB1M07Q,HORC_1126_Republican,2027-01-04T15:00:00-06:00,1000,0.18,0.82,2026-08-13T20:39:12.313020214-05:00',
  'BW2WQQPDW07Q,HORC_1126_Republican,2027-01-04T15:00:00-06:00,2000,0.21,0.79,2026-08-14T07:16:41.334884912-05:00',
  'BVNZ2MSM407Q,UHPHX_081326_99,2026-08-14T03:00:00-05:00,58,0.52,0.48,2026-08-13T16:15:30.13715089-05:00',
].join('\n');

const archive = parseArchiveCsv(ARCHIVE_CSV);

function session(contract: string) {
  const row = archive.get(contract);
  assert.ok(row, `fixture is missing ${contract}`);
  return row;
}

/* --------------------------------------------------------------- envelope */

describe('unwrap', () => {
  // The single-contract lookup, byte for byte.
  const real: RawFexEnvelope = {
    statusCode: 200,
    body: {
      data: '[{"contract_id": "HORC_1126_Republican", "product_id": "HORC", "open_interest": 1032777, "last_yes_price": 0.21, "last_no_price": 0.79}]',
      next_page: null,
      total_pages: 1,
    },
  };

  it('parses the payload a second time, because body.data is a JSON string', () => {
    const { rows } = unwrap<RawFexContract>(real);
    assert.equal(rows.length, 1);
    assert.equal(rows[0]?.contract_id, 'HORC_1126_Republican');
    assert.equal(rows[0]?.last_yes_price, 0.21);
  });

  it('reports the next page so a crawl knows when the catalogue ends', () => {
    assert.equal(unwrap(real).nextPage, null);
    assert.equal(unwrap({ statusCode: 200, body: { data: '[]', next_page: 2 } }).nextPage, 2);
  });

  it('accepts a plain array, in case the double wrapping is ever fixed', () => {
    const { rows } = unwrap<RawFexContract>({
      statusCode: 200,
      body: { data: [{ contract_id: 'HORC_1126_Republican' }], next_page: null },
    });
    assert.equal(rows[0]?.contract_id, 'HORC_1126_Republican');
  });

  it('reads the envelope status, not the HTTP one, when a request is rejected', () => {
    // The real shape of a 400: HTTP 200 outside, and `body` collapsed to a string.
    assert.throws(
      () =>
        unwrap({
          statusCode: 400,
          body: '{"error": "sortBy must be one of: category, contract_id, event_display_name"}',
        }),
      /sortBy must be one of/,
    );
  });

  it('names the payload cap when the Lambda itself fails', () => {
    assert.throws(
      () =>
        unwrap({
          errorMessage: 'Response payload size exceeded maximum allowed payload size (6291556 bytes).',
          errorType: 'Function.ResponseSizeTooLarge',
        }),
      /payload size/,
    );
  });

  it('refuses a data string that is not JSON rather than returning nothing', () => {
    assert.throws(() => unwrap({ statusCode: 200, body: { data: '<html>' } }), /not JSON/);
  });
});

describe('num', () => {
  it('reads a JSON number and a CSV string alike', () => {
    assert.equal(num(0.21), 0.21);
    assert.equal(num('70800'), 70800);
  });

  it('keeps a stated zero and drops an unstated figure', () => {
    // The difference the whole terminal rests on: 0 is the exchange saying
    // zero, null is the exchange saying nothing.
    assert.equal(num(0), 0);
    assert.equal(num('0'), 0);
    assert.equal(num(null), null);
    assert.equal(num(undefined), null);
    assert.equal(num(''), null);
  });

  it('refuses every other thing Number() would call zero', () => {
    // `Number(' ')` and `Number(false)` are both 0, and a blank archive cell is
    // the venue declining to state a figure, not stating that it is nothing.
    assert.equal(num(' '), null);
    assert.equal(num('\t'), null);
    assert.equal(num(false), null);
    assert.equal(num([]), null);
    assert.equal(num('n/a'), null);
    assert.equal(num(Number.NaN), null);
  });
});

/* ------------------------------------------------------------ identifiers */

describe('eventTickerOf', () => {
  it('takes the product and its period, which is what an event is here', () => {
    assert.equal(eventTickerOf('HORC_1126_Republican'), 'HORC_1126');
    assert.equal(eventTickerOf('UHBKF_081726_91'), 'UHBKF_081726');
  });

  it('is unchanged by the nine four-segment Conditional ids', () => {
    assert.equal(eventTickerOf('ZFFCP_091626_E0X0926_3.4'), 'ZFFCP_091626');
    assert.equal(eventTickerOf('ZFFCP_091626_E-25X0926_4.2'), 'ZFFCP_091626');
  });

  it('answers in the case the registry folds ids to', () => {
    assert.equal(eventTickerOf('horc_1126_republican'), 'HORC_1126');
  });
});

describe('strikeOf', () => {
  it('reads the last segment, not the third', () => {
    // Index 2 would yield the scenario code `E0X0926`, which renders as a
    // strike label and parses as NaN.
    assert.equal(strikeOf('ZFFCP_091626_E0X0926_3.4'), '3.4');
    assert.equal(strikeOf('ZFFCP_091626_E-25X0926_4.2'), '4.2');
  });

  it('is identical to the third segment on a three-segment id', () => {
    assert.equal(strikeOf('HORC_1126_Republican'), 'Republican');
    assert.equal(strikeOf('YXLBT_123126_40000'), '40000');
    assert.equal(strikeOf('ACD_1226_432.0'), '432.0');
  });

  it('states no strike when the id carries none', () => {
    assert.equal(strikeOf('HORC_1126'), '');
  });
});

/* ----------------------------------------------------------------- market */

describe('normaliseMarket', () => {
  it('carries prices through as dollars, and the ticker in the registry case', () => {
    const market = normaliseMarket(HORC, PRODUCTS.HORC);
    assert.equal(market.venue, 'forecastex');
    // Upper, because that is what `normaliseId` hands `getMarket` back.
    assert.equal(market.ticker, 'HORC_1126_REPUBLICAN');
    assert.equal(market.eventTicker, 'HORC_1126');
    assert.equal(market.seriesTicker, 'HORC');
    assert.equal(market.lastPrice, 0.21);
    assert.equal(market.title, HORC.question);
    assert.equal(market.category, 'Elections');
    assert.equal(market.marketType, 'binary');
  });

  it('states no bid, ask or depth, because the exchange has no book', () => {
    const market = normaliseMarket(HORC, PRODUCTS.HORC);
    assert.equal(market.yesBid, null);
    assert.equal(market.yesAsk, null);
    assert.equal(market.noBid, null);
    assert.equal(market.noAsk, null);
    assert.equal(market.liquidity, null);
  });

  it('carries the last print into mid rather than deriving one from the NO print', () => {
    // last_yes_price and last_no_price are independent prints and sum to 1.01
    // on 832 contracts, so neither is the other's mirror.
    const market = normaliseMarket({ ...HORC, last_no_price: 0.8 }, PRODUCTS.HORC);
    assert.equal(market.mid, 0.21);
    assert.equal(market.mid, market.lastPrice);
  });

  it('reports a never-traded contract as unpriced, not as worth nothing', () => {
    const market = normaliseMarket(DENVER_83, PRODUCTS.UHBKF);
    assert.equal(market.lastPrice, null);
    assert.equal(market.mid, null);
  });

  it('keeps an open interest of zero, which the venue does state', () => {
    assert.equal(normaliseMarket(DENVER_83, PRODUCTS.UHBKF).openInterest, 0);
    assert.equal(normaliseMarket(HORC, PRODUCTS.HORC).openInterest, 1032777);
  });

  it('leaves lifetime volume and turnover unstated until the archive answers', () => {
    const market = normaliseMarket(HORC, PRODUCTS.HORC);
    assert.equal(market.volume, null);
    assert.equal(market.volume24h, null);
    assert.equal(market.previousPrice, null);
    assert.equal(market.change, null);
  });

  it('claims a greater strike only where the question proves the direction', () => {
    const denver = normaliseMarket(DENVER_91, PRODUCTS.UHBKF);
    assert.equal(denver.strikeType, 'greater');
    assert.equal(denver.floorStrike, 91);
    assert.equal(denver.capStrike, null);
  });

  it('reads the four-segment strike, so the Conditional book is not NaN', () => {
    const market = normaliseMarket(ZFFCP, PRODUCTS.ZFFCP);
    assert.equal(market.floorStrike, 3.4);
    assert.equal(market.strikeType, 'greater');
    assert.equal(market.yesSubTitle, 'Above 3.4');
  });

  it('states no strike where the outcome is a name rather than a number', () => {
    const market = normaliseMarket(HORC, PRODUCTS.HORC);
    assert.equal(market.strikeType, null);
    assert.equal(market.floorStrike, null);
    // The canonical spelling survives in the label even though the ticker folds.
    assert.equal(market.yesSubTitle, 'Republican');
    assert.equal(market.noSubTitle, 'No');
  });

  it('reads "above" as the same upward ladder that "exceed" is', () => {
    // 3,121 numeric contracts word it this way. Refusing them costs the strike
    // level as well as the direction, and the level is not in doubt.
    const market = normaliseMarket(FF, PRODUCTS.FF);
    assert.equal(market.strikeType, 'greater');
    assert.equal(market.floorStrike, 3.625);
    assert.equal(market.capStrike, null);
    assert.equal(market.yesSubTitle, 'Above 3.625%');
  });

  it('puts a "below" ladder’s strike on the cap, not the floor', () => {
    // The id cannot tell these apart from the ones above — `ULATL_081826_73`
    // and `UHBKF_081726_91` are the same grammar — and 370 contracts run this
    // way. Reading the strike as a floor would put the YES region on the wrong
    // side of the line.
    const market = normaliseMarket(ATLANTA_73, PRODUCTS.ULATL);
    assert.equal(market.strikeType, 'less');
    assert.equal(market.capStrike, 73);
    assert.equal(market.floorStrike, null);
    assert.equal(market.yesSubTitle, 'Below 73');
    assert.equal(market.noSubTitle, '73 or above');
  });

  it('keeps "at least" inclusive, so the rung itself settles YES', () => {
    const market = normaliseMarket(HURRICANES_11, PRODUCTS.HCAB);
    assert.equal(market.strikeType, 'greater_or_equal');
    assert.equal(market.floorStrike, 11);
    assert.equal(market.yesSubTitle, '11 or above');
  });

  it('states no bound for a question that names an exact value, not a ladder', () => {
    // "Exactly one dissenter" has a number in its id and no direction at all.
    const market = normaliseMarket(DISSENTERS_1, PRODUCTS.DISSN);
    assert.equal(market.strikeType, null);
    assert.equal(market.floorStrike, null);
    assert.equal(market.capStrike, null);
  });

  it('stamps the Central offset onto the zoneless close and expiry', () => {
    const market = normaliseMarket(HORC, PRODUCTS.HORC);
    // January in Chicago is CST; the exchange's own CSV agrees on -06:00.
    assert.equal(market.expirationTime, '2027-01-04T15:00:00-06:00');
    assert.equal(market.closeTime, '2027-01-04T15:00:00-06:00');
    // Summer is CDT.
    assert.equal(normaliseMarket(DENVER_91, PRODUCTS.UHBKF).closeTime, '2026-08-18T00:59:00-05:00');
  });

  it('reports an open contract and no settlement, which is all the catalogue holds', () => {
    const market = normaliseMarket(HORC, PRODUCTS.HORC);
    assert.equal(market.status, 'open');
    // Settled contracts leave the catalogue, so nothing reachable has a result.
    assert.equal(market.result, '');
    assert.equal(market.openTime, '');
    assert.equal(market.rulesPrimary, HORC.exchange_spec_url);
  });
});

/* ---------------------------------------------------------------- archive */

describe('parseArchiveCsv', () => {
  it('keeps one row per contract, since the NO row is the YES row mirrored', () => {
    assert.equal(archive.size, 4);
    assert.equal(session('HORC_1126_REPUBLICAN').end_price, '0.21');
  });

  it('keys on the folded id, which is the case the terminal asks in', () => {
    assert.ok(archive.has('FF_091726_3.625'.toUpperCase()));
    assert.equal(archive.get('ff_091726_3.625'), undefined);
  });

  it('keeps an empty settlement price as empty rather than parsing it', () => {
    // It is blank on 52.6% of rows; a parse would throw or produce a zero.
    assert.equal(session('UST_1230_54.41').settlement_price, '');
    assert.equal(session('HORC_1126_REPUBLICAN').settlement_price, '0.21');
  });
});

describe('applySession', () => {
  it('states the session turnover and the previous close', () => {
    const market = applySession(
      normaliseMarket(HORC, PRODUCTS.HORC),
      session('HORC_1126_REPUBLICAN'),
    );
    assert.equal(market.volume24h, 70800);
    assert.equal(market.previousPrice, 0.21);
    assert.equal(market.change, 0);
  });

  it('takes the move from the live print against the archived close', () => {
    const market = applySession(normaliseMarket(FF, PRODUCTS.FF), session('FF_091726_3.625'));
    assert.equal(market.previousPrice, 0.27);
    assert.equal(market.change, 0.02);
  });

  it('keeps a traded quantity of zero, which is a session that really was quiet', () => {
    const market = applySession(normaliseMarket({ ...HORC, contract_id: 'ACD_1226_432.0' }), session('ACD_1226_432.0'));
    assert.equal(market.volume24h, 0);
    // The row still carries a mark on start/end even though nothing traded.
    assert.equal(market.previousPrice, 0.04);
  });

  it('states no previous close when the row has no price anywhere in it', () => {
    // 15,446 rows are untraded with start and end both 0.00 — the file having
    // no mark, not a contract worth nothing. A 0 here would print a full-dollar
    // move on the movers board.
    const market = applySession(normaliseMarket(HORC, PRODUCTS.HORC), session('UST_1230_54.41'));
    assert.equal(market.previousPrice, null);
    assert.equal(market.change, null);
    assert.equal(market.volume24h, 0);
  });

  it('leaves open interest as the live catalogue stated it', () => {
    const market = applySession(
      normaliseMarket(HORC, PRODUCTS.HORC),
      session('HORC_1126_REPUBLICAN'),
    );
    // The archive's column is that session's close; the catalogue's is now.
    assert.equal(market.openInterest, 1032777);
  });
});

/* ------------------------------------------------------------------ event */

describe('normaliseEvent', () => {
  it('groups the legs under the product and period, and titles them from the venue', () => {
    const event = normaliseEvent([HORC, HORC_DEM], PRODUCTS.HORC);
    assert.equal(event.venue, 'forecastex');
    assert.equal(event.eventTicker, 'HORC_1126');
    assert.equal(event.seriesTicker, 'HORC');
    assert.equal(event.title, 'US House of Representatives Control November 2026');
    assert.equal(event.subTitle, 'US House of Representatives Control');
    assert.equal(event.category, 'Elections');
    assert.equal(event.markets.length, 2);
    assert.equal(event.markets[0]?.eventTicker, 'HORC_1126');
  });

  it('marks a named-outcome field exclusive, because those legs really do exclude', () => {
    const event = normaliseEvent([HORC, HORC_DEM], PRODUCTS.HORC);
    assert.equal(event.mutuallyExclusive, true);
  });

  it('does not mark a numeric ladder exclusive — its rungs are all true at once', () => {
    const event = normaliseEvent([DENVER_83, DENVER_91], PRODUCTS.UHBKF);
    assert.equal(event.mutuallyExclusive, false);
  });

  it('does not mark the Conditional book exclusive, despite its String strikes', () => {
    // ZFFCP_091626 has a String strike unit and nine legs, and is three Fed
    // scenarios crossed with three cumulative CPI thresholds — a ladder inside
    // each scenario. Flagging it would put a fictional arbitrage on screen.
    const legs = ['3.4', '3.8', '4.2'].flatMap((strike) =>
      ['E0X0926', 'E-25X0926', 'E25X0926'].map((scenario) => ({
        ...ZFFCP,
        contract_id: `ZFFCP_091626_${scenario}_${strike}`,
      })),
    );
    const event = normaliseEvent(legs, PRODUCTS.ZFFCP);
    assert.equal(event.markets.length, 9);
    assert.equal(event.mutuallyExclusive, false);
  });

  it('claims no exclusivity for a single leg, which excludes nothing', () => {
    assert.equal(normaliseEvent([HORC], PRODUCTS.HORC).mutuallyExclusive, false);
  });

  it('merges the archive into the legs it covers and leaves the rest unstated', () => {
    const event = normaliseEvent([HORC, HORC_DEM], PRODUCTS.HORC, archive);
    assert.equal(event.markets[0]?.volume24h, 70800);
    // The Democratic leg is not in this trimmed file: unstated, not zero.
    assert.equal(event.markets[1]?.volume24h, null);
  });
});

/* ------------------------------------------------------------------- tape */

describe('parsePairsCsv and normaliseTrades', () => {
  const pairs = parsePairsCsv(PAIRS_CSV);

  it('reads the whole-exchange file, which is every contract at once', () => {
    assert.equal(pairs.length, 3);
    assert.equal(pairs[0]?.pair_id, 'BVT0EZB1M07Q');
  });

  it('filters to the contract asked for, matching the folded ticker', () => {
    const { trades } = normaliseTrades(pairs, 'HORC_1126_REPUBLICAN', 50);
    assert.equal(trades.length, 2);
    assert.equal(trades[0]?.ticker, 'HORC_1126_REPUBLICAN');
  });

  it('sorts newest first, because the file is only roughly time-ordered', () => {
    const { trades } = normaliseTrades(pairs, 'HORC_1126_REPUBLICAN', 50);
    assert.equal(trades[0]?.tradeId, 'BW2WQQPDW07Q');
    assert.equal(trades[1]?.tradeId, 'BVT0EZB1M07Q');
  });

  it('truncates the nanosecond fraction, which Date.parse cannot read', () => {
    const { trades } = normaliseTrades(pairs, 'HORC_1126_REPUBLICAN', 50);
    // 2026-08-14T07:16:41.334884912-05:00
    assert.equal(trades[0]?.ts, 1786709801);
    assert.equal(trades[1]?.ts, 1786671552);
  });

  it('carries both prices and the pair count as the exchange states them', () => {
    const { trades } = normaliseTrades(pairs, 'HORC_1126_REPUBLICAN', 50);
    assert.equal(trades[0]?.yesPrice, 0.21);
    assert.equal(trades[0]?.noPrice, 0.79);
    assert.equal(trades[0]?.count, 2000);
    assert.equal(trades[0]?.isBlockTrade, false);
  });

  it('names no taker, because a paired match has no aggressor', () => {
    // Every print creates one YES and one NO holder at once. `yes` would assert
    // an initiative the matching model rules out.
    const { trades } = normaliseTrades(pairs, 'HORC_1126_REPUBLICAN', 50);
    assert.equal(trades[0]?.takerSide, '');
  });

  it('honours the limit and offers no cursor, since a page is a session file', () => {
    const one = normaliseTrades(pairs, 'HORC_1126_REPUBLICAN', 1);
    assert.equal(one.trades.length, 1);
    assert.equal(one.cursor, null);
  });

  it('returns nothing for a contract that did not print, rather than someone else’s prints', () => {
    assert.equal(normaliseTrades(pairs, 'ACD_1226_432.0', 50).trades.length, 0);
  });

  it('drops a row it cannot read rather than printing a $0.00 lot dated 1970', () => {
    // S3 rewrites the tape every ten minutes, so a reader can arrive mid-write
    // and take a truncated last line. Coercing the blanks to numbers would seat
    // a fabricated print at the top of the panel.
    const torn = parsePairsCsv(
      [
        'pair_id,event_contract,expiration_date,quantity,yes_price,no_price,pair_time',
        'BW2WQQPDW07Q,HORC_1126_Republican,2027-01-04T15:00:00-06:00,2000,0.21,0.79,2026-08-14T07:16:41.334884912-05:00',
        'BW2WQQPDX07Q,HORC_1126_Republican,2027-01-04T15:00:00-06:00,,,,',
      ].join('\n'),
    );
    assert.equal(torn.length, 2);
    const { trades } = normaliseTrades(torn, 'HORC_1126_REPUBLICAN', 50);
    assert.equal(trades.length, 1);
    assert.equal(trades[0]?.tradeId, 'BW2WQQPDW07Q');
  });
});

/* ------------------------------------------------- what the exchange lacks */

describe('capabilities the exchange does not serve', () => {
  it('refuses the book by naming the mechanism, not by returning an empty ladder', () => {
    // An empty book reads as a market with no resting interest. There is no
    // book here at all, and the hint has to say where the quotes actually live.
    assert.throws(
      () => getOrderBook('HORC_1126_REPUBLICAN'),
      (err: unknown) => {
        const e = err as { code?: string; hint?: string };
        assert.equal(e.code, 'unsupported');
        assert.match(e.hint ?? '', /\/api\/contracts/);
        assert.match(e.hint ?? '', /portal\.proxy\/v1\/ft/);
        return true;
      },
    );
  });

  it('refuses minute bars, because /api/prices takes only h and d', () => {
    return assert.rejects(
      () => getCandles('HORC_1126_REPUBLICAN', 1, 0, 0),
      (err: unknown) => {
        const e = err as { code?: string; hint?: string };
        assert.equal(e.code, 'unsupported');
        assert.match(e.hint ?? '', /\/api\/prices/);
        return true;
      },
    );
  });
});

/* ---------------------------------------------------------------- candles */

describe('sampleEnd', () => {
  it('closes an hourly bar an hour after the stamp, which opens it', () => {
    // Reconciled against the tape: the bucket stamped 2026-08-17T22:00:00Z on
    // UHLAX_081826_78 carries volume 158, and the prints between 22:00Z and
    // 23:00Z are 21+10+30+94+3 = 158. The stamp is the bucket's start, and
    // `Candle.time` is a period end everywhere in the terminal.
    assert.equal(sampleEnd('2026-08-11T17:00:00Z', 60), 1786471200);
    assert.equal(sampleEnd('2026-08-11T17:00:00Z', 60), Date.parse('2026-08-11T18:00:00Z') / 1000);
  });

  it('closes a daily bar at the end of the UTC day, not the Central session', () => {
    // The daily point is the sum of that UTC date's hourly buckets — 992 on
    // 2026-08-17 for UHLAX_081826_78, exactly — while the exchange's own
    // archive files the 8/17 *session* (16:15 CT to 16:14 CT) as a different
    // figure entirely. Closing at Central midnight stamps every bar 5h late.
    assert.equal(sampleEnd('2026-08-14', 1440), Date.parse('2026-08-15T00:00:00Z') / 1000);
    // No seasonal offset applies: a UTC day is a UTC day in January too.
    assert.equal(sampleEnd('2026-01-14', 1440), Date.parse('2026-01-15T00:00:00Z') / 1000);
  });

  it('states nothing for a missing stamp', () => {
    assert.equal(sampleEnd(undefined, 60), null);
    assert.equal(sampleEnd('not a date', 60), null);
  });
});

describe('bucketSamples', () => {
  const hourly: RawFexPricePoint[] = [
    {
      instrument_id: 'HORC_1126_Republican',
      yes_price: 0.16,
      no_price: 0.84,
      interval_date: '2026-08-11T17:00:00Z',
      volume: 4000,
    },
    {
      instrument_id: 'HORC_1126_Republican',
      yes_price: 0.15,
      no_price: 0.85,
      interval_date: '2026-08-11T18:00:00Z',
      volume: 0,
    },
  ];

  it('draws one bar per sample, its high and low being its close', () => {
    const candles = bucketSamples(hourly, 60);
    assert.equal(candles.length, 2);
    assert.deepEqual(
      candles.map((c) => [c.open, c.high, c.low, c.close]),
      [
        [0.16, 0.16, 0.16, 0.16],
        [0.15, 0.15, 0.15, 0.15],
      ],
    );
  });

  it('reads a zero-volume bucket as the mark carried forward, and keeps the zero', () => {
    const candles = bucketSamples(hourly, 60);
    assert.equal(candles[0]?.volume, 4000);
    assert.equal(candles[0]?.traded, true);
    assert.equal(candles[1]?.volume, 0);
    assert.equal(candles[1]?.traded, false);
  });

  it('states no open interest per bucket, which the feed never carries', () => {
    const candles = bucketSamples(hourly, 60);
    assert.equal(candles[0]?.openInterest, null);
    assert.equal(candles[0]?.bid, null);
    assert.equal(candles[0]?.ask, null);
  });

  it('buckets the daily feed on Central session days, oldest first', () => {
    const candles = bucketSamples(
      [
        { yes_price: 0.21, interval_date: '2026-08-17', volume: 100 },
        { yes_price: 0.32, interval_date: '2025-04-09', volume: 10000 },
      ],
      1440,
    );
    assert.deepEqual(
      candles.map((c) => c.close),
      [0.32, 0.21],
    );
    assert.ok((candles[0]?.time ?? 0) < (candles[1]?.time ?? 0));
  });

  it('folds a repeated stamp into one bar instead of drawing two at one instant', () => {
    const candles = bucketSamples(
      [
        { yes_price: 0.2, interval_date: '2026-08-11T17:00:00Z', volume: 10 },
        { yes_price: 0.24, interval_date: '2026-08-11T17:00:00Z', volume: 5 },
      ],
      60,
    );
    assert.equal(candles.length, 1);
    assert.equal(candles[0]?.high, 0.24);
    assert.equal(candles[0]?.low, 0.2);
    assert.equal(candles[0]?.close, 0.24);
    assert.equal(candles[0]?.volume, 15);
  });

  it('drops a sample with no price rather than charting it as zero', () => {
    assert.equal(
      bucketSamples([{ yes_price: null, interval_date: '2026-08-14', volume: 0 }], 1440).length,
      0,
    );
  });
});

/* ----------------------------------------------------------------- series */

describe('normaliseSeries', () => {
  it('spells out the frequency code the product registry states', () => {
    assert.equal(normaliseSeries(PRODUCTS.HORC ?? {}).frequency, 'monthly');
    assert.equal(normaliseSeries(PRODUCTS.UHBKF ?? {}).frequency, 'daily');
    // 688 of 1,004 products are one-offs rather than a recurring cadence.
    assert.equal(normaliseSeries(PRODUCTS.FF ?? {}).frequency, 'irregular');
  });

  it('carries the category the product states, which is what listSeries filters on', () => {
    const series = normaliseSeries(PRODUCTS.HORC ?? {});
    assert.equal(series.venue, 'forecastex');
    assert.equal(series.ticker, 'HORC');
    assert.equal(series.title, 'US House of Representatives Control');
    assert.equal(series.category, 'Elections');
  });

  it('tags the strike unit, which is what says whether a strike is a number', () => {
    assert.deepEqual(normaliseSeries(PRODUCTS.HORC ?? {}).tags, [
      'String',
      'HORC=MAN',
      'United States Congress',
    ]);
    // Missing fields are dropped rather than tagged as empty strings.
    assert.deepEqual(normaliseSeries(PRODUCTS.ZFFCP ?? {}).tags, ['String']);
  });
});

/* ---------------------------------------------------------------- session */

describe('sessionDate', () => {
  it('files a print made after the 16:15 CT roll under the next session', () => {
    // 2026-08-13T21:30Z is 16:30 in Chicago, and the tape agrees: that print is
    // in pairs_20260814.csv.
    assert.equal(sessionDate(Date.parse('2026-08-13T21:30:00Z')), '20260814');
  });

  it('keeps an afternoon print in the session it traded in', () => {
    assert.equal(sessionDate(Date.parse('2026-08-13T20:00:00Z')), '20260813');
  });

  it('rolls the month and the year with the session', () => {
    assert.equal(sessionDate(Date.parse('2026-12-31T23:00:00Z')), '20270101');
  });
});

describe('centralIso', () => {
  it('spells out the offset the exchange leaves off its timestamps', () => {
    assert.equal(centralIso('2026-08-18T02:00:00'), '2026-08-18T02:00:00-05:00');
    assert.equal(centralIso('2027-01-04T15:00:00'), '2027-01-04T15:00:00-06:00');
  });

  it('leaves a string it cannot read alone rather than inventing an instant', () => {
    assert.equal(centralIso(''), '');
  });
});

/* ------------------------------------------------------------ leg labelling */

describe('distinguishingClauses', () => {
  const fomc = [
    'Will the Fed leave the rate unchanged in September 2026?',
    'Will the Fed raise the rate 25bps in September 2026?',
    'Will the Fed lower the rate 25bps in September 2026?',
    'Will the Fed raise the rate 50bps or more in September 2026?',
    'Will the Fed lower the rate 50bps or more in September 2026?',
  ];

  it('leaves the clause the legs differ by, from both ends of the question', () => {
    // Printed as the exchange names them these rungs are `E0`, `E25`, `E-25`,
    // `H50` and `M-50`, which no reader and no matcher can pair with `No
    // change` at the other five brokers.
    assert.deepEqual(distinguishingClauses(fomc), [
      'leave the rate unchanged',
      'raise the rate 25bps',
      'lower the rate 25bps',
      'raise the rate 50bps or more',
      'lower the rate 50bps or more',
    ]);
  });

  it('strips a shared tail of boilerplate the exchange appends to one month', () => {
    assert.deepEqual(
      distinguishingClauses([
        'Will Ashley Hinson win the Iowa general election for US Senate in 2026?',
        'Will Josh Turek win the Iowa general election for US Senate in 2026?',
      ]),
      ['Ashley Hinson', 'Josh Turek'],
    );
  });

  it('gives up on a single leg, which has no sibling to differ from', () => {
    assert.equal(distinguishingClauses(['Will the Fed leave the rate unchanged?']), null);
    assert.equal(distinguishingClauses([]), null);
  });

  it('gives up when the exchange worded two legs identically', () => {
    // It lists a few races twice, once by party and once by candidate, with the
    // same question on both. Diffing those leaves two blanks, and the strike
    // code is a worse label than the question but a better one than nothing.
    const twice = ['Will Josh Turek win the Iowa general election?', 'Will Josh Turek win the Iowa general election?'];
    assert.equal(distinguishingClauses(twice), null);
  });

  it('gives up rather than blanking a leg whose question is a prefix of another', () => {
    // Every word of the first is shared, so diffing leaves it with nothing at
    // all — and a leg with no label is worse than a leg labelled `E0`.
    assert.equal(
      distinguishingClauses(['Will the Fed raise the rate', 'Will the Fed raise the rate 25bps']),
      null,
    );
  });

  it('treats punctuation as part of the word, which keeps a trailing rung labelled', () => {
    // `rate?` and `rate` are not the same word, so the shared run stops short
    // and both legs keep a clause. The trailing mark is trimmed afterwards.
    assert.deepEqual(
      distinguishingClauses(['Will the Fed raise the rate?', 'Will the Fed raise the rate 25bps?']),
      ['rate', 'rate 25bps'],
    );
  });

  it('ignores case when deciding which words are shared', () => {
    assert.deepEqual(
      distinguishingClauses(['Will inflation exceed 3%?', 'will inflation exceed 4%?']),
      ['3%', '4%'],
    );
  });
});
