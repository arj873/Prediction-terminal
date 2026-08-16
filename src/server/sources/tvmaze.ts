/**
 * TV schedules — from the TVmaze public API.
 *
 * Kalshi's TV book is episodic and date-driven: `KXBIGBROTHER` and
 * `KXBIGBROTHERELIMINATION` (the second-busiest entertainment series by volume)
 * resolve week by week as episodes air, and `KXDWTS`, `KXSNL` and
 * `KXTVSHOWSCANCELLED` all turn on what is actually broadcast and when.
 *
 * TVmaze is the rare entertainment source that is a real API: JSON, no key, no
 * bot protection, documented rate limits. So this module is a normaliser rather
 * than a parser — the only work is flattening TVmaze's nested show/network
 * objects into flat rows and being careful about the fields it leaves null.
 */

import type { TvEpisode, TvSchedule } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const BASE = 'https://api.tvmaze.com';
const DATE = /^\d{4}-\d{2}-\d{2}$/;
const COUNTRY = /^[A-Za-z]{2}$/;

export function assertDate(raw: string): string {
  const date = raw.trim();
  if (!DATE.test(date) || Number.isNaN(Date.parse(date))) {
    throw new UpstreamError(`"${raw}" is not a valid schedule date`, {
      code: 'bad_request',
      hint: 'Dates are YYYY-MM-DD, e.g. `TV 2026-08-20`.',
    });
  }
  return date;
}

export function assertCountry(raw: string): string {
  const country = raw.trim().toUpperCase();
  if (!COUNTRY.test(country)) {
    throw new UpstreamError(`"${raw}" is not a country code`, {
      code: 'bad_request',
      hint: 'Use a two-letter code, e.g. `TV GB` or `TV 2026-08-20 US`.',
    });
  }
  return country;
}

interface RawEpisode {
  airtime?: string;
  name?: string;
  season?: number | null;
  number?: number | null;
  runtime?: number | null;
  type?: string;
  show?: {
    name?: string;
    network?: { name?: string } | null;
    webChannel?: { name?: string } | null;
  } | null;
}

function intOrNull(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

export function normaliseSchedule(
  raw: RawEpisode[],
  date: string,
  country: string,
  sourceUrl: string,
): TvSchedule {
  const episodes: TvEpisode[] = raw
    .map((entry) => {
      const show = entry.show ?? {};
      return {
        airtime: entry.airtime ?? '',
        show: show.name ?? '',
        // Broadcast shows carry a `network`; streaming ones carry a
        // `webChannel` instead, and a row with neither is not worth a blank.
        network: show.network?.name ?? show.webChannel?.name ?? '',
        season: intOrNull(entry.season),
        episode: intOrNull(entry.number),
        name: entry.name ?? '',
        runtime: intOrNull(entry.runtime),
        type: entry.type ?? '',
      };
    })
    .filter((episode) => episode.show !== '');

  // TVmaze returns the day roughly in airtime order, but not reliably, and an
  // untimed entry should sort to the end rather than to midnight.
  episodes.sort((a, b) => {
    if (a.airtime === b.airtime) return a.show.localeCompare(b.show);
    if (!a.airtime) return 1;
    if (!b.airtime) return -1;
    return a.airtime.localeCompare(b.airtime);
  });

  return { date, country, episodes, sourceUrl };
}

function todayIso(): string {
  return new Date().toISOString().slice(0, 10);
}

export async function getSchedule(rawDate?: string, rawCountry?: string): Promise<TvSchedule> {
  const date = rawDate ? assertDate(rawDate) : todayIso();
  const country = rawCountry ? assertCountry(rawCountry) : 'US';
  const sourceUrl = `${BASE}/schedule?country=${country}&date=${date}`;

  return cache.cached(`tvmaze:${country}:${date}`, TTL.tvSchedule, async () => {
    const raw = await fetchJson<RawEpisode[]>(sourceUrl, { timeoutMs: 25_000, retries: 2 });
    if (!Array.isArray(raw)) {
      throw new UpstreamError('TVmaze returned an unexpected schedule payload', {
        code: 'bad_upstream_body',
      });
    }
    return normaliseSchedule(raw, date, country, sourceUrl);
  });
}
