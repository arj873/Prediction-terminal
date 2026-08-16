/**
 * Steam player counts — the live half of Kalshi's video-game book.
 *
 * `KXSTEAM*` (Game of the Year, Labor of Love and the rest of the Steam Awards)
 * and `GAMERANK` settle on Steam itself, and concurrent-player counts are the
 * public number those markets move on. Release-date markets — `KXGTA6`,
 * `KXESVI`, `KXPS6` — trade on the same catalogue.
 *
 * Two upstreams, each used for what it is actually good at:
 *
 *   api.steampowered.com   Valve's own API. Authoritative live player count for
 *                          a given appid, no key required for this endpoint.
 *   steamcharts.com        The most-played leaderboard with *names* attached.
 *
 * Valve publishes a most-played endpoint too, but it returns bare appids, so
 * rendering a leaderboard from it would cost one store lookup per row. The
 * scraped table carries rank, name, current and peak together in one request.
 */

import * as cheerio from 'cheerio';
import type { SteamChart, SteamGame } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson, fetchText } from '../lib/http.js';

const API = 'https://api.steampowered.com';
const STORE = 'https://store.steampowered.com';
const CHARTS = 'https://steamcharts.com';

function intOrNull(text: string | undefined): number | null {
  if (!text) return null;
  const cleaned = text.replace(/[,\s]/g, '');
  if (!cleaned || cleaned === '-') return null;
  const n = Number.parseInt(cleaned, 10);
  return Number.isFinite(n) ? n : null;
}

export function assertAppId(raw: string | number): number {
  const n = typeof raw === 'number' ? raw : Number.parseInt(String(raw).trim(), 10);
  if (!Number.isInteger(n) || n <= 0 || n > 100_000_000) {
    throw new UpstreamError(`"${raw}" is not a valid Steam app id`, {
      code: 'bad_request',
      hint: 'Pass a numeric app id, or a game name: `STEAM counter-strike`.',
    });
  }
  return n;
}

function headerKey(text: string): string {
  return text.toLowerCase().replace(/[^a-z0-9]/g, '');
}

/**
 * Parse the most-played table.
 *
 * The leading rank column has a blank header, so it is taken positionally;
 * everything else is resolved by header text. The app id comes from the row's
 * own link (`/app/730`), which is what makes each row clickable through to a
 * live count.
 */
export function parseTopPage(html: string, sourceUrl: string): SteamChart {
  const $ = cheerio.load(html);
  const $table = $('table').first();

  if ($table.length === 0) {
    throw new UpstreamError(`No games table found on ${sourceUrl}`, {
      code: 'parse_failed',
      hint: 'steamcharts.com answered but served no leaderboard table.',
    });
  }

  const columns = new Map<string, number>();
  $table.find('th').each((i, node) => {
    const key = headerKey($(node).text());
    if (key && !columns.has(key)) columns.set(key, i);
    else if (!key && i === 0) columns.set('rank', 0);
  });

  const at = (cells: string[], ...names: string[]): string | undefined => {
    for (const name of names) {
      const index = columns.get(name);
      if (index !== undefined && cells[index] !== undefined) return cells[index];
    }
    return undefined;
  };

  const games: SteamGame[] = [];

  $table.find('tbody tr').each((_, node) => {
    const $row = $(node);
    const cells: string[] = [];
    $row.find('td').each((__, td) => {
      cells.push($(td).text().replace(/\s+/g, ' ').trim());
    });
    if (cells.length === 0) return;

    const name = at(cells, 'name', 'game') ?? '';
    if (!name) return;

    const href = $row.find('a[href*="/app/"]').attr('href') ?? '';
    const appId = intOrNull(/\/app\/(\d+)/.exec(href)?.[1]);

    games.push({
      appId: appId ?? 0,
      name,
      // Ranks render as `1.`; the trailing dot is not part of the number.
      rank: intOrNull((at(cells, 'rank') ?? '').replace(/\./g, '')) ?? games.length + 1,
      currentPlayers: intOrNull(at(cells, 'currentplayers')),
      peakPlayers: intOrNull(at(cells, 'peakplayers')),
    });
  });

  if (games.length === 0) {
    throw new UpstreamError(`No games found on ${sourceUrl}`, {
      code: 'parse_failed',
      hint:
        'steamcharts.com returned a page but its rows did not match the expected ' +
        'table shape. The layout may have changed.',
    });
  }

  return { view: 'top', games, sourceUrl };
}

export async function getTop(limit = 25): Promise<SteamChart> {
  const sourceUrl = `${CHARTS}/top`;
  const chart = await cache.cached(`steam:top`, TTL.steam, async () => {
    const html = await fetchText(sourceUrl, { timeoutMs: 30_000, retries: 2 });
    return parseTopPage(html, sourceUrl);
  });
  return { ...chart, games: chart.games.slice(0, limit) };
}

/** Valve's live concurrent-player count for one app. */
export async function getPlayerCount(appId: number): Promise<number | null> {
  const id = assertAppId(appId);
  const url = `${API}/ISteamUserStats/GetNumberOfCurrentPlayers/v1/?appid=${id}`;

  return cache.cached(`steam:players:${id}`, TTL.steam, async () => {
    const body = await fetchJson<{ response?: { player_count?: number; result?: number } }>(url, {
      timeoutMs: 15_000,
      retries: 2,
    });
    // Valve answers `result: 1` for a real app and omits the count for one that
    // reports no stats, which is not an error — it is "no live figure".
    const count = body.response?.player_count;
    return typeof count === 'number' && Number.isFinite(count) ? count : null;
  });
}

interface StoreSearchBody {
  items?: { type?: string; name?: string; id?: number }[];
}

/**
 * Resolve a game name to apps, most relevant first.
 *
 * The store's own search endpoint is used rather than the full app list, which
 * is a ~10 MB document listing every app Valve has ever shipped.
 */
export async function searchGames(query: string, limit = 10): Promise<SteamGame[]> {
  const q = query.trim();
  if (!q) {
    throw new UpstreamError('Missing game name', {
      code: 'bad_request',
      hint: 'Usage: `STEAM <game>`, e.g. `STEAM counter-strike`.',
    });
  }

  const url = `${STORE}/api/storesearch/?term=${encodeURIComponent(q)}&l=en&cc=US`;

  const games = await cache.cached(`steam:search:${q.toLowerCase()}`, TTL.catalogue, async () => {
    const body = await fetchJson<StoreSearchBody>(url, { timeoutMs: 20_000, retries: 2 });
    return (body.items ?? [])
      .filter((item) => typeof item.id === 'number' && item.name)
      .map<SteamGame>((item) => ({
        appId: item.id as number,
        name: item.name as string,
        rank: null,
        currentPlayers: null,
        peakPlayers: null,
      }));
  });

  return games.slice(0, limit);
}

/**
 * One game, with its live count attached.
 *
 * Accepts either an app id or a name. Peak is filled in from the leaderboard
 * when the game is currently on it — Valve's live endpoint reports only the
 * instantaneous figure, and a peak is what makes it readable.
 */
export async function getGame(input: string): Promise<SteamChart> {
  const raw = input.trim();
  if (!raw) {
    throw new UpstreamError('Missing game', {
      code: 'bad_request',
      hint: 'Usage: `STEAM <game|appid>`, e.g. `STEAM 730`.',
    });
  }

  let game: SteamGame;
  if (/^\d+$/.test(raw)) {
    game = { appId: assertAppId(raw), name: `App ${raw}`, rank: null, currentPlayers: null, peakPlayers: null };
  } else {
    const hits = await searchGames(raw, 1);
    const hit = hits[0];
    if (!hit) {
      throw new UpstreamError(`Steam has no game matching "${raw}"`, {
        code: 'not_found',
        hint: 'Check the spelling, or pass the numeric app id from the store URL.',
      });
    }
    game = hit;
  }

  const players = await getPlayerCount(game.appId);

  // Best-effort: a game outside the top table simply has no peak to show.
  let peak: number | null = null;
  let rank: number | null = null;
  try {
    const top = await getTop(100);
    const onChart = top.games.find((g) => g.appId === game.appId);
    peak = onChart?.peakPlayers ?? null;
    rank = onChart?.rank ?? null;
    if (game.name.startsWith('App ') && onChart?.name) game.name = onChart.name;
  } catch {
    // The leaderboard is decoration here; the live count is the answer.
  }

  return {
    view: 'game',
    games: [{ ...game, rank, currentPlayers: players, peakPlayers: peak }],
    sourceUrl: `${STORE}/app/${game.appId}/`,
  };
}
