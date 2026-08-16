/**
 * Domestic daily box office — scraped from boxofficemojo.com.
 *
 * The film side of Kalshi's entertainment book trades on how a release performs
 * — `KXGGBOXOFFICE` settles on box office achievement outright, and the opening
 * weekend is the strongest public read on the award and Rotten Tomatoes markets
 * that sit around a title.
 *
 * Mojo serves a plain server-rendered table, which makes this the least fragile
 * of the scraped sources: one table, a labelled header row, one row per release.
 * Columns are resolved by header text so that Mojo adding a column does not
 * silently shift "theaters" into "days in release".
 */

import * as cheerio from 'cheerio';
import type { BoxOfficeDay, BoxOfficeEntry } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchText } from '../lib/http.js';

const BASE = 'https://www.boxofficemojo.com';
const DATE = /^\d{4}-\d{2}-\d{2}$/;

export function assertDate(raw: string): string {
  const date = raw.trim();
  if (!DATE.test(date) || Number.isNaN(Date.parse(date))) {
    throw new UpstreamError(`"${raw}" is not a valid box office date`, {
      code: 'bad_request',
      hint: 'Dates are YYYY-MM-DD, e.g. `BO 2026-08-14`.',
    });
  }
  return date;
}

/** `"$19,000,000"` → 19000000; `"-"`, `""` → null. */
function moneyOrNull(text: string | undefined): number | null {
  if (!text) return null;
  const cleaned = text.replace(/[$,\s]/g, '');
  if (!cleaned || cleaned === '-' || cleaned === '--') return null;
  const n = Number.parseInt(cleaned, 10);
  return Number.isFinite(n) ? n : null;
}

function intOrNull(text: string | undefined): number | null {
  if (!text) return null;
  const cleaned = text.replace(/[,\s]/g, '');
  if (!cleaned || cleaned === '-' || cleaned === '--') return null;
  const n = Number.parseInt(cleaned, 10);
  return Number.isFinite(n) ? n : null;
}

/** `"+68.9%"` → 68.9; `"-55.6%"` → -55.6; a bare `"-"` (no comparison) → null. */
function percentOrNull(text: string | undefined): number | null {
  if (!text) return null;
  const cleaned = text.replace(/[%,\s]/g, '');
  if (!cleaned || cleaned === '-' || cleaned === '--') return null;
  const n = Number.parseFloat(cleaned);
  return Number.isFinite(n) ? n : null;
}

/**
 * Normalise a header cell to a lookup key.
 *
 * `%` becomes `pct` rather than being stripped, because Mojo ships two pairs of
 * columns that would otherwise collide: `YD` (yesterday's rank) next to `%± YD`
 * (the day-over-day change). Dropping the symbol maps both to `yd` and silently
 * reads a rank where a percentage belongs.
 */
function headerKey(text: string): string {
  return text
    .toLowerCase()
    .replace(/%/g, 'pct')
    .replace(/±/g, '')
    .replace(/[^a-z0-9]/g, '');
}

export function parseDailyPage(html: string, date: string, sourceUrl: string): BoxOfficeDay {
  const $ = cheerio.load(html);
  const $table = $('table').first();

  if ($table.length === 0) {
    // Mojo serves a perfectly valid page for a date it has not posted grosses
    // for yet, carrying a "No data available" notice instead of a table. That is
    // an empty day, not a broken parser, and saying so is the difference between
    // "wait for tomorrow" and "go fix the scraper".
    if (/no data available/i.test($('body').text())) {
      throw new UpstreamError(`Box Office Mojo has no grosses for ${date} yet`, {
        code: 'not_found',
        hint:
          'Daily grosses are usually posted the following afternoon. Try an ' +
          'earlier date, e.g. `BO 2026-08-14`.',
      });
    }

    throw new UpstreamError(`No box office table found on ${sourceUrl}`, {
      code: 'parse_failed',
      hint: 'Box Office Mojo answered but served no results table for that date.',
    });
  }

  const columns = new Map<string, number>();
  $table.find('th').each((i, node) => {
    const key = headerKey($(node).text());
    if (key && !columns.has(key)) columns.set(key, i);
  });

  const at = (cells: string[], ...names: string[]): string | undefined => {
    for (const name of names) {
      const index = columns.get(name);
      if (index !== undefined && cells[index] !== undefined) return cells[index];
    }
    return undefined;
  };

  const entries: BoxOfficeEntry[] = [];

  $table.find('tr').each((_, node) => {
    const cells: string[] = [];
    $(node)
      .find('td')
      .each((__, td) => {
        cells.push($(td).text().replace(/\s+/g, ' ').trim());
      });
    if (cells.length === 0) return; // the header row

    const title = at(cells, 'release', 'title') ?? '';
    if (!title) return;

    const rank = intOrNull(at(cells, 'td', 'rank')) ?? entries.length + 1;
    const lastRank = intOrNull(at(cells, 'yd'));

    entries.push({
      rank,
      lastRank,
      title,
      gross: moneyOrNull(at(cells, 'daily', 'gross')),
      changeDay: percentOrNull(at(cells, 'pctyd')),
      changeWeek: percentOrNull(at(cells, 'pctlw')),
      theaters: intOrNull(at(cells, 'theaters')),
      average: moneyOrNull(at(cells, 'avg')),
      totalGross: moneyOrNull(at(cells, 'todate')),
      daysInRelease: intOrNull(at(cells, 'days')),
      distributor: at(cells, 'distributor') ?? '',
      // A release with no yesterday rank opened today.
      move: lastRank === null ? null : lastRank - rank,
      isNew: lastRank === null,
    });
  });

  if (entries.length === 0) {
    throw new UpstreamError(`No box office entries found on ${sourceUrl}`, {
      code: 'parse_failed',
      hint:
        'Box Office Mojo returned a page but its rows did not match the expected ' +
        'table shape. That date may predate their daily coverage.',
    });
  }

  entries.sort((a, b) => a.rank - b.rank);

  const heading = $('h1').first().text().replace(/\s+/g, ' ').trim();

  return {
    date,
    title: heading || `Domestic Box Office for ${date}`,
    entries,
    totalGross: entries.reduce((sum, e) => sum + (e.gross ?? 0), 0),
    sourceUrl,
  };
}

/** `n` days before today, as `YYYY-MM-DD`. */
function daysAgoIso(n: number): string {
  return new Date(Date.now() - n * 24 * 60 * 60 * 1000).toISOString().slice(0, 10);
}

/**
 * How far back `BO` will look for the most recent posted chart.
 *
 * Grosses land the afternoon after the day they cover, so "today" is always
 * empty and yesterday usually is too. Holidays and reporting gaps can stretch
 * that, so the walk-back covers a few days rather than assuming a fixed lag —
 * and stops rather than crawling indefinitely into a genuine outage.
 */
const MAX_LOOKBACK_DAYS = 5;

async function fetchDay(date: string): Promise<BoxOfficeDay> {
  const sourceUrl = `${BASE}/date/${date}/`;
  return cache.cached(`boxoffice:${date}`, TTL.boxOffice, async () => {
    const html = await fetchText(sourceUrl, { timeoutMs: 30_000, retries: 2 });
    return parseDailyPage(html, date, sourceUrl);
  });
}

export async function getDaily(rawDate?: string): Promise<BoxOfficeDay> {
  // An explicit date is answered exactly — including with "no grosses yet",
  // which is the honest answer and not something to paper over.
  if (rawDate) return fetchDay(assertDate(rawDate));

  let lastError: unknown;
  for (let back = 1; back <= MAX_LOOKBACK_DAYS; back++) {
    try {
      return await fetchDay(daysAgoIso(back));
    } catch (err) {
      lastError = err;
      // Only an unposted day is worth stepping back from; a block or a timeout
      // would repeat five times over and bury the real cause.
      if (!(err instanceof UpstreamError) || err.code !== 'not_found') throw err;
    }
  }

  throw lastError instanceof UpstreamError
    ? lastError
    : new UpstreamError('Box Office Mojo has posted no recent daily grosses', {
        code: 'not_found',
      });
}
