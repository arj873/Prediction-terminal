/**
 * Parsers for the eight publishers added alongside FRED.
 *
 * These matter for the same reason the FRED parser tests do: most of these
 * hosts throttle, block or rate-limit a datacentre IP, so CI cannot exercise
 * the live path. The fixtures below reproduce the shapes the upstreams actually
 * serve — copied from real responses — so a regression in the parsing is caught
 * even where the network is not available.
 *
 * The recurring failure they guard against is the quiet one. Every upstream
 * here answers a malformed request with HTTP 200 and a *plausible* body: the
 * Federal Reserve returns headers with no rows, SDMX returns several series
 * interleaved, the IMF returns a dataflow description. None of those look like
 * errors until someone reads a chart drawn from them.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { cellAt, headerIndex, numberCell, parseCsv } from '../src/server/lib/csv.js';
import { periodToDate } from '../src/server/lib/period.js';
import { blsPeriodToDate, windows } from '../src/server/sources/bls.js';
import { billLabel, currentCongress, stripHtml } from '../src/server/sources/congress.js';
import { readPeriodicity, toDataset } from '../src/server/sources/datagov.js';
import { parseChooserPage, parseFedPackage, splitFedId, toDdpDate } from '../src/server/sources/feddata.js';
import { polygonSymbol } from '../src/server/sources/polygon.js';
import { padCik, zipFilings } from '../src/server/sources/secedgar.js';
import { assertSingleSeries, encodeSdmxKey, parseSdmxCsv, searchCatalogue } from '../src/server/sources/sdmx.js';

/* -------------------------------------------------------------------- csv */

describe('parseCsv', () => {
  it('keeps commas that live inside quoted fields', () => {
    // The failure this prevents is silent: splitting on every comma shifts each
    // later column by one, pairing a date with the wrong series' value.
    const rows = parseCsv('"Series Description","Yield, 10-year","Yield, 2-year"\n2026-01-01,4.2,4.0\n');
    assert.deepEqual(rows[0], ['Series Description', 'Yield, 10-year', 'Yield, 2-year']);
    assert.deepEqual(rows[1], ['2026-01-01', '4.2', '4.0']);
  });

  it('reads a doubled quote as one literal quote', () => {
    assert.deepEqual(parseCsv('a,"say ""hi""",c\n')[0], ['a', 'say "hi"', 'c']);
  });

  it('handles embedded newlines inside a quoted field', () => {
    const rows = parseCsv('a,"line one\nline two"\nb,c\n');
    assert.equal(rows.length, 2);
    assert.deepEqual(rows[0], ['a', 'line one\nline two']);
  });

  it('handles CRLF and a trailing newline without emitting a blank row', () => {
    assert.deepEqual(parseCsv('a,b\r\nc,d\r\n'), [['a', 'b'], ['c', 'd']]);
  });

  it('returns nothing for an empty document', () => {
    assert.deepEqual(parseCsv(''), []);
  });
});

describe('headerIndex and cellAt', () => {
  it('matches case-insensitively and strips a BOM and stray spaces', () => {
    const index = headerIndex(['﻿TIME_PERIOD', '  OBS_VALUE  ', 'Unique Identifier: ']);
    assert.equal(index.get('time_period'), 0);
    assert.equal(index.get('obs_value'), 1);
    assert.equal(index.get('unique identifier:'), 2);
  });

  it('keeps the first of a repeated column name', () => {
    // The OECD's labelled CSV emits each dimension twice — a code column then a
    // label column — and the code is the one worth indexing.
    const index = headerIndex(['FREQ', 'FREQ']);
    assert.equal(index.get('freq'), 0);
  });

  it('reads an absent column as empty rather than throwing', () => {
    const index = headerIndex(['A']);
    assert.equal(cellAt(['x'], index, 'NOPE'), '');
  });
});

describe('numberCell', () => {
  it('reads a missing reading as null rather than zero', () => {
    // A zero would assert the reading was taken and came out at nought.
    for (const empty of ['', '.', 'NA', 'ND', 'NC', '   ']) {
      assert.equal(numberCell(empty), null, `"${empty}" should be null`);
    }
    assert.equal(numberCell(undefined), null);
  });

  it('strips thousands separators and surrounding quotes', () => {
    assert.equal(numberCell('"28,624.069"'), 28624.069);
    assert.equal(numberCell('-2.5'), -2.5);
  });
});

/* ----------------------------------------------------------------- periods */

describe('periodToDate', () => {
  it('dates every period to the day it begins', () => {
    assert.equal(periodToDate('2026-08-12'), '2026-08-12');
    assert.equal(periodToDate('2026-08'), '2026-08-01');
    assert.equal(periodToDate('2026-M08'), '2026-08-01');
    assert.equal(periodToDate('2026-Q3'), '2026-07-01');
    assert.equal(periodToDate('2026Q1'), '2026-01-01');
    assert.equal(periodToDate('2026-S2'), '2026-07-01');
    assert.equal(periodToDate('2026-T2'), '2026-05-01');
    assert.equal(periodToDate('2026'), '2026-01-01');
  });

  it('dates an hourly reading to its day', () => {
    assert.equal(periodToDate('2026-08-12T14'), '2026-08-12');
  });

  it('reads an ISO week as its Monday', () => {
    // 2026-01-01 is a Thursday, so ISO week 1 begins Monday 2025-12-29.
    assert.equal(periodToDate('2026-W01'), '2025-12-29');
    assert.equal(periodToDate('2026-W02'), '2026-01-05');
  });

  it('returns null for anything it does not recognise, rather than guessing', () => {
    for (const bad of ['', 'Series Description', 'Unit:', '2026-M13', '2026-Q5', 'later']) {
      assert.equal(periodToDate(bad), null, `"${bad}" should not parse`);
    }
  });
});

/* --------------------------------------------------------------------- bls */

describe('blsPeriodToDate', () => {
  it('reads monthly, quarterly, semiannual and annual codes', () => {
    assert.equal(blsPeriodToDate('2026', 'M07'), '2026-07-01');
    assert.equal(blsPeriodToDate('2026', 'Q02'), '2026-04-01');
    assert.equal(blsPeriodToDate('2026', 'S02'), '2026-07-01');
    assert.equal(blsPeriodToDate('2026', 'A01'), '2026-01-01');
  });

  it('drops the aggregate codes the API interleaves with real readings', () => {
    // M13 is the annual *average*, sitting in the same array as M01–M12.
    // Charting it would draw thirteen points a year, one of them the mean of
    // the other twelve.
    assert.equal(blsPeriodToDate('2026', 'M13'), null);
    assert.equal(blsPeriodToDate('2026', 'Q05'), null);
    assert.equal(blsPeriodToDate('2026', 'S03'), null);
  });

  it('rejects a malformed year', () => {
    assert.equal(blsPeriodToDate('20xx', 'M01'), null);
  });
});

describe('windows', () => {
  it('splits a range into spans the API will actually accept', () => {
    // Without this, "unemployment since 1948" silently returns the last decade
    // of it — which looks like data rather than like a truncation.
    assert.deepEqual(windows(2000, 2029, 10), [
      { start: 2000, end: 2009 },
      { start: 2010, end: 2019 },
      { start: 2020, end: 2029 },
    ]);
  });

  it('does not extend the last window past the requested end', () => {
    assert.deepEqual(windows(2020, 2026, 10), [{ start: 2020, end: 2026 }]);
  });

  it('returns one window for a single year', () => {
    assert.deepEqual(windows(2026, 2026, 20), [{ start: 2026, end: 2026 }]);
  });
});

/* -------------------------------------------------------------------- sdmx */

const ECB_CSV = `KEY,FREQ,CURRENCY,TIME_PERIOD,OBS_VALUE,TITLE,UNIT
EXR.D.USD.EUR.SP00.A,D,USD,2026-08-12,1.1545,US dollar/Euro,USD
EXR.D.USD.EUR.SP00.A,D,USD,2026-08-13,1.1602,US dollar/Euro,USD
EXR.D.USD.EUR.SP00.A,D,USD,2026-08-14,,US dollar/Euro,USD
`;

describe('parseSdmxCsv', () => {
  it('reads observations and the descriptive columns', () => {
    const series = parseSdmxCsv(ECB_CSV, 'the ECB');
    assert.deepEqual(series.observations, [
      { date: '2026-08-12', value: 1.1545 },
      { date: '2026-08-13', value: 1.1602 },
      // An empty OBS_VALUE is a gap in the series, not a zero and not a row to
      // drop — dropping it would silently compress the time axis.
      { date: '2026-08-14', value: null },
    ]);
    assert.equal(series.title, 'US dollar/Euro');
    assert.equal(series.units, 'USD');
    // `D` is spelled the way FRED spells the same thing, so a euro-area series
    // and a US one read alike in the panel's FREQ field.
    assert.equal(series.frequency, 'Daily');
    assert.equal(series.seriesCount, 1);
  });

  it('counts interleaved series from repeated periods', () => {
    // The OECD spreads its key over a dozen columns and the IMF's first column
    // is the dataflow, identical on every row — so multiplicity has to be read
    // from the periods rather than from a key column.
    const two = `TIME_PERIOD,OBS_VALUE,REF_AREA
2026-01,1,USA
2026-01,2,GBR
2026-02,3,USA
2026-02,4,GBR
`;
    const series = parseSdmxCsv(two, 'the OECD');
    assert.equal(series.seriesCount, 2);
    assert.throws(
      () => assertSingleSeries(series, 'DF/…', 'the OECD'),
      /matches 2 the OECD series, not one/,
    );
  });

  it('accepts a single series without complaint', () => {
    assert.doesNotThrow(() => assertSingleSeries(parseSdmxCsv(ECB_CSV, 'the ECB'), 'x', 'the ECB'));
  });

  it('rejects a body with no observation columns', () => {
    assert.throws(
      () => parseSdmxCsv('A,B\n1,2\n', 'the IMF'),
      /without TIME_PERIOD\/OBS_VALUE/,
    );
  });

  it('rejects a body that carries only a dataflow description', () => {
    // Exactly what the IMF returns for a key using the wrong country code: 200,
    // well-formed, and containing no observations at all.
    assert.throws(
      () => parseSdmxCsv('DATAFLOW,TIME_PERIOD,OBS_VALUE\nIMF.STA:CPI(5.0.0),,\n', 'the IMF'),
      /no readable observations/,
    );
  });
});

describe('encodeSdmxKey', () => {
  it('leaves the grammar alone and escapes only what is inside a code', () => {
    assert.equal(encodeSdmxKey('D.USD.EUR.SP00.A'), 'D.USD.EUR.SP00.A');
    // `+` is "or" between codes; escaping it to %2B matches nothing.
    assert.equal(encodeSdmxKey('D.USD+GBP.EUR'), 'D.USD+GBP.EUR');
    assert.equal(encodeSdmxKey('A.a b'), 'A.a%20b');
  });
});

describe('searchCatalogue', () => {
  const catalogue = [
    { id: 'A/1', title: 'Euro area unemployment rate', keywords: 'jobless labour' },
    { id: 'B/2', title: 'US dollar / euro reference exchange rate', keywords: 'fx forex' },
  ];

  it('requires every term to match, so a second word narrows rather than widens', () => {
    assert.deepEqual(searchCatalogue('ecb', catalogue, 'unemployment', 10).map((r) => r.id), ['A/1']);
    assert.deepEqual(searchCatalogue('ecb', catalogue, 'euro rate', 10).map((r) => r.id).sort(), ['A/1', 'B/2']);
    assert.deepEqual(searchCatalogue('ecb', catalogue, 'unemployment forex', 10), []);
  });

  it('matches keywords that are not in the title', () => {
    assert.deepEqual(searchCatalogue('ecb', catalogue, 'jobless', 10).map((r) => r.id), ['A/1']);
  });

  it('stamps every result with its publisher', () => {
    assert.ok(searchCatalogue('ecb', catalogue, 'euro', 10).every((r) => r.provider === 'ecb'));
  });
});

/* --------------------------------------------------------------------- fed */

const FED_CSV = `"Series Description","Market yield, 10-year, investment basis","Market yield, 2-year, investment basis"
"Unit:","Percent:_Per_Year","Percent:_Per_Year"
"Multiplier:","1","1"
"Currency:","NA","NA"
"Unique Identifier: ","H15/H15/RIFLGFCY10_N.B","H15/H15/RIFLGFCY02_N.B"
"Time Period","RIFLGFCY10_N.B","RIFLGFCY02_N.B"
2026-08-12,4.70,4.22
2026-08-13,4.72,4.25
2026-08-14,ND,4.26
`;

describe('parseFedPackage', () => {
  it('reads the labelled header block and the observations', () => {
    const pkg = parseFedPackage(FED_CSV);
    assert.equal(pkg.columns.length, 2);
    assert.equal(pkg.columns[0]!.name, 'RIFLGFCY10_N.B');
    assert.equal(pkg.columns[0]!.description, 'Market yield, 10-year, investment basis');
    // The colon separates the measure from its qualifier; both are the unit.
    assert.equal(pkg.columns[0]!.unit, 'Percent Per Year');
    assert.equal(pkg.columns[0]!.uniqueId, 'H15/H15/RIFLGFCY10_N.B');
    assert.deepEqual(pkg.rows[0], { date: '2026-08-12', values: [4.7, 4.22] });
    assert.deepEqual(pkg.rows[2], { date: '2026-08-14', values: [null, 4.26] });
  });

  it('reads a monthly package, whose rows are dated YYYY-MM', () => {
    // Matching only the daily form treated every monthly row as a header and
    // returned a package with no observations at all.
    const monthly = FED_CSV.replace('2026-08-12,4.70,4.22', '2026-08,4.70,4.22');
    const pkg = parseFedPackage(monthly);
    assert.equal(pkg.rows[0]!.date, '2026-08-01');
  });

  it('keys the header rows by label, not by position', () => {
    // H.4.1 omits the currency row; reading by position would shift the short
    // names into the unique-identifier slot and leave every series unnameable.
    const noCurrency = FED_CSV.split('\n').filter((l) => !l.startsWith('"Currency:"')).join('\n');
    const pkg = parseFedPackage(noCurrency);
    assert.equal(pkg.columns[0]!.name, 'RIFLGFCY10_N.B');
    assert.equal(pkg.columns[0]!.currency, '');
  });

  it('returns no columns and no rows for an empty body', () => {
    assert.deepEqual(parseFedPackage(''), { columns: [], rows: [] });
  });
});

describe('parseChooserPage', () => {
  it('reads packages rendered as radio inputs', () => {
    const html = `<div><input type="radio" value="rel=H15&amp;series=bf17364827e38702b42a58cf8eaa3f78&amp;type=package" /> Treasury Constant Maturities [csv, All Observations, 981.9 KB ]</div>`;
    assert.deepEqual(parseChooserPage(html), [
      { hash: 'bf17364827e38702b42a58cf8eaa3f78', label: 'Treasury Constant Maturities' },
    ]);
  });

  it('reads packages rendered as select options', () => {
    // G.17 uses a `<select>` where H.15 uses radios. Matching only one silently
    // found nothing for half the releases.
    const html = `<select><option value="rel=G17&amp;series=6752c709e85190bd7d0ad535100a4175&amp;type=package">All the Latest Monthly Data [csv, Last 7 Obs, 205.1 KB]</option></select>`;
    assert.deepEqual(parseChooserPage(html), [
      { hash: '6752c709e85190bd7d0ad535100a4175', label: 'All the Latest Monthly Data' },
    ]);
  });

  it('ignores the ASP.NET hidden fields that carry no package', () => {
    const html = `<input type="hidden" name="__VIEWSTATE" value="NBHoXfKIzWY" />`;
    assert.deepEqual(parseChooserPage(html), []);
  });
});

describe('splitFedId and toDdpDate', () => {
  it('splits release from series and tolerates the dotted release form', () => {
    assert.deepEqual(splitFedId('H15/RIFLGFCY10_N.B'), { release: 'H15', series: 'RIFLGFCY10_N.B' });
    assert.deepEqual(splitFedId('h.15/RIFSPFF_N.M'), { release: 'H15', series: 'RIFSPFF_N.M' });
    assert.deepEqual(splitFedId('H41'), { release: 'H41', series: '' });
  });

  it('writes dates the only way the DDP accepts them', () => {
    assert.equal(toDdpDate('2015-06-30'), '06/30/2015');
    assert.equal(toDdpDate('not-a-date'), '');
  });
});

/* --------------------------------------------------------------------- sec */

describe('padCik', () => {
  it('pads to the ten digits EDGAR keys everything on', () => {
    assert.equal(padCik(320193), '0000320193');
    assert.equal(padCik('0000320193'), '0000320193');
    assert.equal(padCik('CIK320193'), '0000320193');
  });
});

describe('zipFilings', () => {
  const filings = {
    accessionNumber: ['0000320193-26-000081', '0001140361-26-032884'],
    filingDate: ['2026-08-01', '2026-08-13'],
    reportDate: ['2026-06-28', ''],
    form: ['10-Q', '4'],
    items: ['', ''],
    size: [1234, 5678],
    primaryDocument: ['aapl-20260628.htm', 'form4.xml'],
    primaryDocDescription: ['10-Q', 'FORM 4'],
  };

  it('zips the parallel arrays EDGAR ships into filings', () => {
    const rows = zipFilings(filings, '0000320193', 10);
    assert.equal(rows.length, 2);
    assert.equal(rows[0]!.form, '10-Q');
    assert.equal(rows[0]!.filed, '2026-08-01');
    assert.equal(
      rows[0]!.url,
      'https://www.sec.gov/Archives/edgar/data/320193/000032019326000081/aapl-20260628.htm',
    );
  });

  it('filters by form without shifting any other field', () => {
    const rows = zipFilings(filings, '0000320193', 10, '4');
    assert.equal(rows.length, 1);
    assert.equal(rows[0]!.accession, '0001140361-26-032884');
    assert.equal(rows[0]!.filed, '2026-08-13');
  });

  it('honours the limit', () => {
    assert.equal(zipFilings(filings, '0000320193', 1).length, 1);
  });

  it('reads a short array as empty rather than shifting later filings', () => {
    const short = { ...filings, reportDate: ['2026-06-28'] };
    const rows = zipFilings(short, '0000320193', 10);
    assert.equal(rows[1]!.reportDate, '');
    assert.equal(rows[1]!.form, '4');
  });
});

/* ---------------------------------------------------------------- congress */

describe('congress helpers', () => {
  it('computes the sitting Congress from the date', () => {
    assert.equal(currentCongress(new Date('2026-08-17T00:00:00Z')), 119);
    assert.equal(currentCongress(new Date('2025-06-01T00:00:00Z')), 119);
    assert.equal(currentCongress(new Date('2027-03-01T00:00:00Z')), 120);
    // A Congress convenes on 3 January; before then the previous one still sits.
    assert.equal(currentCongress(new Date('2027-01-01T00:00:00Z')), 119);
    assert.equal(currentCongress(new Date('2027-01-03T00:00:00Z')), 120);
  });

  it('writes a bill citation the way a reader would say it', () => {
    assert.equal(billLabel('hr', 1, 119), 'HR 1 (119th)');
    assert.equal(billLabel('s', '42', 118), 'S 42 (118th)');
    assert.equal(billLabel('hr', 1, 101), 'HR 1 (101st)');
    assert.equal(billLabel('hr', 1, 112), 'HR 1 (112th)');
    assert.equal(billLabel('hr', 1, 122), 'HR 1 (122nd)');
  });

  it('renders a summary as text, since the terminal never builds markup', () => {
    assert.equal(
      stripHtml('<p>This bill <b>appropriates</b> funds.</p><p>It takes effect &amp; expires.</p>'),
      'This bill appropriates funds.\n\nIt takes effect & expires.',
    );
    assert.equal(stripHtml('a<br/>b'), 'a\nb');
    assert.equal(stripHtml(''), '');
  });
});

/* ---------------------------------------------------------------- data.gov */

describe('data.gov', () => {
  it('reads ISO-8601 periodicity as words, passing unknown codes through', () => {
    assert.equal(readPeriodicity('R/P1Y'), 'Annual');
    assert.equal(readPeriodicity('R/P1M'), 'Monthly');
    assert.equal(readPeriodicity('irregular'), 'Irregular');
    assert.equal(readPeriodicity('R/P9Z'), 'R/P9Z');
    assert.equal(readPeriodicity(undefined), '');
  });

  it('reads a DCAT record, preferring the landing page for the link', () => {
    const dataset = toDataset({
      dcat: {
        identifier: 'abc',
        title: '  Consumer   Price Index ',
        description: 'Monthly CPI.',
        modified: '2026-08-01T12:00:00Z',
        accrualPeriodicity: 'R/P1M',
        landingPage: 'https://example.gov/cpi',
        theme: ['prices'],
        keyword: ['inflation', 'prices'],
        publisher: { name: 'BLS' },
        distribution: [{ format: 'text/csv', downloadURL: 'https://example.gov/cpi.csv' }],
      },
    });

    assert.equal(dataset?.title, 'Consumer Price Index');
    assert.equal(dataset?.publisher, 'BLS');
    assert.equal(dataset?.modified, '2026-08-01');
    assert.equal(dataset?.frequency, 'Monthly');
    assert.deepEqual(dataset?.formats, ['CSV']);
    assert.deepEqual(dataset?.themes, ['prices', 'inflation']);
    assert.equal(dataset?.url, 'https://example.gov/cpi');
  });

  it('falls back to a distribution URL when there is no landing page', () => {
    const dataset = toDataset({
      dcat: { title: 'X', distribution: [{ accessURL: 'https://example.gov/x' }] },
    });
    assert.equal(dataset?.url, 'https://example.gov/x');
  });

  it('drops a record with no title rather than rendering a blank row', () => {
    assert.equal(toDataset({ dcat: {} }), null);
    assert.equal(toDataset({}), null);
  });
});

/* ----------------------------------------------------------------- polygon */

describe('polygonSymbol', () => {
  it('maps this terminal\'s index names to Polygon\'s', () => {
    // A wrong guess here would quote an unrelated instrument rather than fail,
    // which is why the mapping is stated rather than inferred.
    assert.equal(polygonSymbol('^GSPC'), 'I:SPX');
    assert.equal(polygonSymbol('SPX'), 'I:SPX');
    assert.equal(polygonSymbol('^DJI'), 'I:DJI');
    assert.equal(polygonSymbol('^VIX'), 'I:VIX');
  });

  it('passes an ordinary equity through untouched', () => {
    assert.equal(polygonSymbol('aapl'), 'AAPL');
  });

  it('treats an unmapped caret symbol as an index', () => {
    assert.equal(polygonSymbol('^FTSE'), 'I:FTSE');
  });

  it('writes crypto pairs the way Polygon does', () => {
    assert.equal(polygonSymbol('BTC-USD', 'crypto'), 'X:BTCUSD');
    assert.equal(polygonSymbol('ETH', 'crypto'), 'X:ETHUSD');
    assert.equal(polygonSymbol('X:SOLUSD', 'crypto'), 'X:SOLUSD');
  });
});
