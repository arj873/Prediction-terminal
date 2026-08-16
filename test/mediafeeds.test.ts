/**
 * Parser tests for the Netflix, Spotify/YouTube, box office, Steam and TV feeds.
 *
 * These lean on the cases where a plausible-looking parser reads the wrong
 * number without ever failing: two columns whose headers normalise to the same
 * key, a chart with no movement column at all, and an upstream that answers
 * `200 OK` with "no data" instead of an error.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { parseDailyPage } from '../src/server/sources/boxoffice.js';
import { parseTsv } from '../src/server/sources/netflix.js';
import { parseTopPage } from '../src/server/sources/steam.js';
import {
  parseChartTable,
  parseMove,
  splitArtistTitle,
  type ChartSpec,
} from '../src/server/sources/streamcharts.js';
import { normaliseSchedule } from '../src/server/sources/tvmaze.js';

/* ----------------------------------------------------------------- netflix */

const TSV = [
  'week\tcategory\tweekly_rank\tshow_title\tseason_title\tweekly_hours_viewed\truntime\tweekly_views\tcumulative_weeks_in_top_10',
  '2026-08-09\tFilms (English)\t1\tThe Last House\tN/A\t51400000\t1.8667\t27500000\t1',
  '2026-08-09\tTV (English)\t1\tWednesday\tWednesday: Season 2\t71000000\t0.9\t9000000\t3',
].join('\n');

describe('netflix parseTsv', () => {
  it('keys every row by the header line', () => {
    const rows = parseTsv(TSV);
    assert.equal(rows.length, 2);
    assert.equal(rows[0]!['show_title'], 'The Last House');
    assert.equal(rows[0]!['weekly_views'], '27500000');
    assert.equal(rows[1]!['category'], 'TV (English)');
  });

  it('returns nothing for an empty document rather than throwing', () => {
    assert.deepEqual(parseTsv(''), []);
    assert.deepEqual(parseTsv('week\tcategory'), []);
  });

  it('ignores blank trailing lines', () => {
    assert.equal(parseTsv(`${TSV}\n\n`).length, 2);
  });
});

/* -------------------------------------------------------- spotify / youtube */

const SPOTIFY_SPEC: ChartSpec = {
  slug: 'us-daily',
  name: 'Spotify Daily — US',
  source: 'spotify',
  path: '/spotify/country/us_daily.html',
};

const YOUTUBE_SPEC: ChartSpec = {
  slug: 'alltime',
  name: 'YouTube all time',
  source: 'youtube',
  path: '/youtube/topvideos.html',
};

/** Mirrors kworb's daily table, including the `Streams` / `Streams+` pair. */
const SPOTIFY_HTML = `<html><head><title>Spotify Daily Chart - United States</title></head><body>
<table>
  <thead><tr>
    <th>Pos</th><th>P+</th><th>Artist and Title</th><th>Days</th><th>Pk</th>
    <th>(x?)</th><th>Streams</th><th>Streams+</th><th>7Day</th><th>7Day+</th><th>Total</th>
  </tr></thead>
  <tbody>
    <tr><td>1</td><td>=</td><td>Ella Langley - Choosin' Texas</td><td>302</td><td>1</td><td>(x88)</td>
        <td>1,466,839</td><td>+84,270</td><td>10,022,760</td><td>-116,663</td><td>389,670,040</td></tr>
    <tr><td>2</td><td>+3</td><td>Shakira - Dai Dai (w/ Burna Boy)</td><td>12</td><td>2</td><td>(x5)</td>
        <td>1,145,809</td><td>-75,692</td><td>7,596,216</td><td>+231,164</td><td>12,679,364</td></tr>
    <tr><td>3</td><td>NEW</td><td>KATSEYE - Hootie Frutti</td><td>1</td><td>3</td><td>(x1)</td>
        <td>900,000</td><td>-</td><td>900,000</td><td>-</td><td>900,000</td></tr>
  </tbody>
</table></body></html>`;

/** The all-time table: no movement column, and `Views` is cumulative. */
const YOUTUBE_HTML = `<html><head><title>YouTube - Most Viewed Music Videos of All Time</title></head><body>
<table>
  <thead><tr><th>Video</th><th>Views</th><th>Yesterday</th></tr></thead>
  <tbody>
    <tr><td>Luis Fonsi - Despacito ft. Daddy Yankee</td><td>9,099,389,704</td><td>803,323</td></tr>
    <tr><td>Baby Shark Dance</td><td>15,000,000,000</td><td>1,200,000</td></tr>
  </tbody>
</table></body></html>`;

describe('parseChartTable (spotify)', () => {
  it('reads the value column and its delta separately', () => {
    // `Streams` and `Streams+` both reduce to `streams` unless `+` is preserved,
    // which would silently make the delta unreadable.
    const { entries } = parseChartTable(SPOTIFY_HTML, SPOTIFY_SPEC, 'u');
    assert.equal(entries[0]!.streams, 1466839);
    assert.equal(entries[0]!.streamsChange, 84270);
    assert.equal(entries[1]!.streamsChange, -75692);
  });

  it('splits artist from title on the first separator only', () => {
    const { entries } = parseChartTable(SPOTIFY_HTML, SPOTIFY_SPEC, 'u');
    assert.equal(entries[1]!.artist, 'Shakira');
    assert.equal(entries[1]!.title, 'Dai Dai (w/ Burna Boy)');
  });

  it('reads peak, days and cumulative total by header name', () => {
    const { entries } = parseChartTable(SPOTIFY_HTML, SPOTIFY_SPEC, 'u');
    assert.equal(entries[0]!.peak, 1);
    assert.equal(entries[0]!.days, 302);
    assert.equal(entries[0]!.total, 389670040);
  });

  it('derives last week’s rank from the movement cell', () => {
    const { entries } = parseChartTable(SPOTIFY_HTML, SPOTIFY_SPEC, 'u');
    assert.equal(entries[0]!.lastRank, 1); // held at 1
    assert.equal(entries[1]!.lastRank, 5); // climbed 3 into 2
  });

  it('marks a debut as new with no previous rank', () => {
    const { entries } = parseChartTable(SPOTIFY_HTML, SPOTIFY_SPEC, 'u');
    assert.equal(entries[2]!.isNew, true);
    assert.equal(entries[2]!.lastRank, null);
  });

  it('throws a diagnosable error when no rows match', () => {
    assert.throws(
      () => parseChartTable('<html><body><table></table></body></html>', SPOTIFY_SPEC, 'https://k/'),
      /No chart entries found/,
    );
  });

  it('throws when there is no table at all', () => {
    assert.throws(
      () => parseChartTable('<html><body><p>hi</p></body></html>', SPOTIFY_SPEC, 'https://k/'),
      /No chart table found/,
    );
  });
});

describe('parseChartTable (youtube all-time)', () => {
  it('treats Views as cumulative and Yesterday as the daily figure', () => {
    const { entries } = parseChartTable(YOUTUBE_HTML, YOUTUBE_SPEC, 'u');
    assert.equal(entries[0]!.total, 9099389704);
    assert.equal(entries[0]!.streams, 803323);
  });

  it('leaves a video with no separator whole', () => {
    const { entries } = parseChartTable(YOUTUBE_HTML, YOUTUBE_SPEC, 'u');
    assert.equal(entries[1]!.title, 'Baby Shark Dance');
    assert.equal(entries[1]!.artist, '');
  });

  it('reports unknown movement rather than inventing a debut or a hold', () => {
    // This table has no movement column. Reporting `0` would print `=` for every
    // row; reporting a debut would print NEW for a decade-old video.
    const { entries } = parseChartTable(YOUTUBE_HTML, YOUTUBE_SPEC, 'u');
    assert.equal(entries[0]!.move, null);
    assert.equal(entries[0]!.isNew, false);
  });

  it('ranks by document order when there is no rank column', () => {
    const { entries } = parseChartTable(YOUTUBE_HTML, YOUTUBE_SPEC, 'u');
    assert.deepEqual(entries.map((e) => e.rank), [1, 2]);
  });
});

describe('parseMove', () => {
  it('reads the four shapes kworb emits', () => {
    assert.deepEqual(parseMove('='), { move: 0, isNew: false });
    assert.deepEqual(parseMove('+3'), { move: 3, isNew: false });
    assert.deepEqual(parseMove('-1'), { move: -1, isNew: false });
    assert.deepEqual(parseMove('NEW'), { move: null, isNew: true });
  });

  it('treats a re-entry like a debut', () => {
    assert.deepEqual(parseMove('RE'), { move: null, isNew: true });
  });

  it('distinguishes an absent column from a held position', () => {
    assert.deepEqual(parseMove(undefined), { move: null, isNew: false });
    assert.deepEqual(parseMove(''), { move: null, isNew: false });
  });
});

describe('splitArtistTitle', () => {
  it('splits on the first separator, not the last', () => {
    assert.deepEqual(splitArtistTitle('Tyler, The Creator - Sticky - Remix'), {
      artist: 'Tyler, The Creator',
      title: 'Sticky - Remix',
    });
  });

  it('keeps a string with no separator whole', () => {
    assert.deepEqual(splitArtistTitle('Baby Shark Dance'), { artist: '', title: 'Baby Shark Dance' });
  });

  it('does not split on a hyphen without spaces', () => {
    assert.deepEqual(splitArtistTitle('Spider-Man'), { artist: '', title: 'Spider-Man' });
  });
});

/* -------------------------------------------------------------- box office */

const BO_HTML = `<html><body><h1>Domestic Box Office For Aug 14, 2026</h1>
<table>
  <tr><th>TD</th><th>YD</th><th>Release</th><th>Daily</th><th>%± YD</th><th>%± LW</th>
      <th>Theaters</th><th>Avg</th><th>To Date</th><th>Days</th><th>Distributor</th></tr>
  <tr><td>1</td><td>3</td><td>Spider-Man: Brand New Day</td><td>$19,000,000</td><td>+68.9%</td><td>-55.6%</td>
      <td>4,539</td><td>$4,185</td><td>$734,831,670</td><td>15</td><td>Sony Pictures Releasing</td></tr>
  <tr><td>2</td><td>-</td><td>The End of Oak Street</td><td>$8,050,000</td><td>-</td><td>-</td>
      <td>3,446</td><td>$2,336</td><td>$8,050,000</td><td>1</td><td>Warner Bros.</td></tr>
</table></body></html>`;

describe('parseDailyPage', () => {
  it('does not confuse the yesterday rank with the day-over-day percentage', () => {
    // `YD` and `%± YD` both normalise to `yd` unless the percent sign is kept,
    // which would report a rank of 3 as a +3% change.
    const { entries } = parseDailyPage(BO_HTML, '2026-08-14', 'u');
    assert.equal(entries[0]!.lastRank, 3);
    assert.equal(entries[0]!.changeDay, 68.9);
    assert.equal(entries[0]!.changeWeek, -55.6);
  });

  it('parses money into whole dollars', () => {
    const { entries } = parseDailyPage(BO_HTML, '2026-08-14', 'u');
    assert.equal(entries[0]!.gross, 19000000);
    assert.equal(entries[0]!.totalGross, 734831670);
    assert.equal(entries[0]!.average, 4185);
  });

  it('computes movement as rank improvement', () => {
    const { entries } = parseDailyPage(BO_HTML, '2026-08-14', 'u');
    assert.equal(entries[0]!.move, 2); // 3 → 1
  });

  it('treats a release with no yesterday rank as an opener', () => {
    const { entries } = parseDailyPage(BO_HTML, '2026-08-14', 'u');
    assert.equal(entries[1]!.isNew, true);
    assert.equal(entries[1]!.changeDay, null);
    assert.equal(entries[1]!.daysInRelease, 1);
  });

  it('sums the day’s gross', () => {
    assert.equal(parseDailyPage(BO_HTML, '2026-08-14', 'u').totalGross, 27050000);
  });

  it('reports an unposted day as not_found rather than a parse failure', () => {
    // Mojo serves 200 OK with this notice for a date it has not posted yet.
    // Calling that a parse failure sends someone to debug a working scraper.
    const empty = '<html><body><h1>Domestic Box Office For Aug 15, 2026</h1><p>No data available.</p></body></html>';
    assert.throws(() => parseDailyPage(empty, '2026-08-15', 'u'), /has no grosses for 2026-08-15 yet/);
  });

  it('still reports a genuine layout change as a parse failure', () => {
    assert.throws(
      () => parseDailyPage('<html><body><p>surprise</p></body></html>', '2026-08-14', 'u'),
      /No box office table found/,
    );
  });
});

/* ------------------------------------------------------------------- steam */

const STEAM_HTML = `<html><body><table>
  <thead><tr><th></th><th>Name</th><th>Current Players</th><th>Last 30 Days</th><th>Peak Players</th><th>Hours Played</th></tr></thead>
  <tbody>
    <tr><td>1.</td><td><a href="/app/730">Counter-Strike 2</a></td><td>1258099</td><td></td><td>1332784</td><td>611619348</td></tr>
    <tr><td>2.</td><td><a href="/app/570">Dota 2</a></td><td>949773</td><td></td><td>949773</td><td>426889899</td></tr>
  </tbody>
</table></body></html>`;

describe('steam parseTopPage', () => {
  it('reads rank, name and both player figures', () => {
    const { games } = parseTopPage(STEAM_HTML, 'u');
    assert.equal(games.length, 2);
    assert.equal(games[0]!.rank, 1);
    assert.equal(games[0]!.name, 'Counter-Strike 2');
    assert.equal(games[0]!.currentPlayers, 1258099);
    assert.equal(games[0]!.peakPlayers, 1332784);
  });

  it('strips the trailing dot from the rank', () => {
    assert.equal(parseTopPage(STEAM_HTML, 'u').games[1]!.rank, 2);
  });

  it('recovers the app id from the row link', () => {
    const { games } = parseTopPage(STEAM_HTML, 'u');
    assert.equal(games[0]!.appId, 730);
    assert.equal(games[1]!.appId, 570);
  });

  it('throws a diagnosable error when the table is empty', () => {
    assert.throws(
      () => parseTopPage('<html><body><table></table></body></html>', 'https://s/'),
      /No games found/,
    );
  });
});

/* ---------------------------------------------------------------- tv guide */

describe('tvmaze normaliseSchedule', () => {
  const raw = [
    {
      airtime: '20:00',
      name: 'Week 6',
      season: 28,
      number: 12,
      runtime: 60,
      type: 'regular',
      show: { name: 'Big Brother', network: { name: 'CBS' } },
    },
    {
      airtime: '',
      name: 'Episode 1',
      season: 1,
      number: 1,
      runtime: null,
      type: 'regular',
      show: { name: 'Streaming Only', network: null, webChannel: { name: 'Netflix' } },
    },
    {
      airtime: '06:00',
      name: 'Early',
      season: null,
      number: null,
      runtime: 30,
      type: 'regular',
      show: { name: 'Morning Show', network: { name: 'NBC' } },
    },
  ];

  it('flattens the nested show and network objects', () => {
    const { episodes } = normaliseSchedule(raw, '2026-08-20', 'US', 'u');
    const bb = episodes.find((e) => e.show === 'Big Brother')!;
    assert.equal(bb.network, 'CBS');
    assert.equal(bb.season, 28);
    assert.equal(bb.episode, 12);
  });

  it('falls back to the web channel for streaming-only shows', () => {
    const { episodes } = normaliseSchedule(raw, '2026-08-20', 'US', 'u');
    assert.equal(episodes.find((e) => e.show === 'Streaming Only')!.network, 'Netflix');
  });

  it('sorts by airtime and pushes untimed entries to the end', () => {
    const { episodes } = normaliseSchedule(raw, '2026-08-20', 'US', 'u');
    assert.deepEqual(episodes.map((e) => e.show), ['Morning Show', 'Big Brother', 'Streaming Only']);
  });

  it('keeps a missing season or episode as null rather than 0', () => {
    const { episodes } = normaliseSchedule(raw, '2026-08-20', 'US', 'u');
    const morning = episodes.find((e) => e.show === 'Morning Show')!;
    assert.equal(morning.season, null);
    assert.equal(morning.episode, null);
  });

  it('drops entries with no show name', () => {
    const { episodes } = normaliseSchedule([{ airtime: '10:00' }], '2026-08-20', 'US', 'u');
    assert.deepEqual(episodes, []);
  });
});
