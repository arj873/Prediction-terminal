/**
 * Billboard parser tests.
 *
 * The fixture mirrors the real billboard.com row structure — the semantic class
 * names, the `.c-span` stat labels, and the lazy-loaded artwork — trimmed down
 * from a live capture of the Hot 100.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { assertDate, assertSlug, parseChartPage } from '../src/server/sources/billboard.js';
import { assertArtUrl } from '../src/server/routes/billboard.js';

function row(opts: {
  rank: number;
  title: string;
  artist?: string;
  lw?: string;
  peak: string;
  weeks: string;
  img?: string;
  lazyImg?: string;
}): string {
  const artistBlock = opts.artist ? `<span class="c-label"><a href="/artist/x/">${opts.artist}</a></span>` : '';
  const img = opts.lazyImg
    ? `<img class="c-lazy-image__img" src="${opts.img ?? 'placeholder.gif'}" data-lazy-src="${opts.lazyImg}">`
    : `<img class="c-lazy-image__img" src="${opts.img ?? ''}">`;

  return `
  <div class="o-chart-results-list-row-container">
    <ul class="o-chart-results-list-row">
      <li class="o-chart-results-list__item"><span class="c-label">${opts.rank}</span></li>
      <li class="o-chart-results-list__item">${img}</li>
      <li class="lrv-u-width-100p">
        <ul>
          <li class="o-chart-results-list__item">
            <h3 id="title-of-a-story" class="c-title">${opts.title}</h3>
            ${artistBlock}
          </li>
          <div>
            <span class="c-span">LW</span>
            <li class="o-chart-results-list__item"><span class="c-label">${opts.lw ?? '-'}</span></li>
          </div>
          <div>
            <span class="c-span">PEAK</span>
            <li class="o-chart-results-list__item"><span class="c-label">${opts.peak}</span></li>
          </div>
          <div>
            <span class="c-span">WEEKS</span>
            <li class="o-chart-results-list__item"><span class="c-label">${opts.weeks}</span></li>
          </div>
        </ul>
      </li>
    </ul>
  </div>`;
}

const PAGE = `<!doctype html><html><head>
<meta property="og:title" content="Hot 100™ | Billboard">
</head><body>
<div class="chart-date-picker" data-date="2026-08-15" data-chart-code="HSI"></div>
<div class="chart-results-list">
  ${row({ rank: 1, title: "Choosin' Texas", artist: 'Ella Langley', lw: '1', peak: '1', weeks: '42', img: 'https://charts-static.billboard.com/a.jpg' })}
  ${row({ rank: 3, title: 'Been By Now', artist: 'Morgan Wallen', lw: '2', peak: '2', weeks: '2', img: 'https://charts-static.billboard.com/c.jpg' })}
  ${row({ rank: 2, title: 'Petal', artist: 'Ariana Grande', peak: '4', weeks: '1', img: 'placeholder.gif', lazyImg: 'https://charts-static.billboard.com/b.jpg' })}
</div>
</body></html>`;

describe('parseChartPage', () => {
  it('reads the chart week from the date picker', () => {
    assert.equal(parseChartPage(PAGE, 'hot-100', 'u').date, '2026-08-15');
  });

  it('strips the Billboard suffix from the chart title', () => {
    assert.equal(parseChartPage(PAGE, 'hot-100', 'u').title, 'Hot 100™');
  });

  it('extracts rank, title and artist for every row', () => {
    const { entries } = parseChartPage(PAGE, 'hot-100', 'u');
    assert.equal(entries.length, 3);
    assert.equal(entries[0]!.title, "Choosin' Texas");
    assert.equal(entries[0]!.artist, 'Ella Langley');
  });

  it('returns entries sorted by rank regardless of document order', () => {
    const { entries } = parseChartPage(PAGE, 'hot-100', 'u');
    assert.deepEqual(
      entries.map((e) => e.rank),
      [1, 2, 3],
    );
  });

  it('keys stats by their printed label, not by position', () => {
    const { entries } = parseChartPage(PAGE, 'hot-100', 'u');
    const wallen = entries.find((e) => e.title === 'Been By Now')!;
    assert.equal(wallen.lastWeek, 2);
    assert.equal(wallen.peak, 2);
    assert.equal(wallen.weeksOnChart, 2);
  });

  it('treats a missing last-week value as a new entry', () => {
    const { entries } = parseChartPage(PAGE, 'hot-100', 'u');
    const petal = entries.find((e) => e.title === 'Petal')!;
    assert.equal(petal.lastWeek, null);
    assert.equal(petal.isNew, true);
    assert.equal(petal.move, null);
  });

  it('computes movement as rank improvement', () => {
    const { entries } = parseChartPage(PAGE, 'hot-100', 'u');
    // Ranked 3 this week after 2 last week: down one.
    assert.equal(entries.find((e) => e.title === 'Been By Now')!.move, -1);
    assert.equal(entries.find((e) => e.title === "Choosin' Texas")!.move, 0);
  });

  it('prefers data-lazy-src over the placeholder image', () => {
    const { entries } = parseChartPage(PAGE, 'hot-100', 'u');
    assert.equal(entries.find((e) => e.title === 'Petal')!.imageUrl, 'https://charts-static.billboard.com/b.jpg');
  });

  it('collapses the duplicated name on artist charts', () => {
    const artistPage = `<html><body><div class="chart-date-picker" data-date="2026-08-15"></div>
      ${row({ rank: 1, title: 'Ariana Grande', artist: 'Ariana Grande', lw: '12', peak: '1', weeks: '555' })}
    </body></html>`;
    const { entries } = parseChartPage(artistPage, 'artist-100', 'u');
    assert.equal(entries[0]!.title, 'Ariana Grande');
    assert.equal(entries[0]!.artist, '');
  });

  it('throws a diagnosable error when no rows match', () => {
    assert.throws(
      () => parseChartPage('<html><body><p>nothing here</p></body></html>', 'hot-100', 'https://x/'),
      /No chart entries found/,
    );
  });
});

describe('input validation', () => {
  it('accepts real chart slugs and normalises case', () => {
    assert.equal(assertSlug('Hot-100'), 'hot-100');
    assert.equal(assertSlug('billboard-global-excl-us'), 'billboard-global-excl-us');
  });

  it('rejects slugs that could escape the chart path', () => {
    for (const bad of ['../admin', 'hot 100', 'hot-100?x=1', '', '/etc/passwd']) {
      assert.throws(() => assertSlug(bad), /not a valid Billboard chart slug/);
    }
  });

  it('accepts ISO chart dates and rejects anything else', () => {
    assert.equal(assertDate('2025-06-14'), '2025-06-14');
    for (const bad of ['06-14-2025', '2025-13-45', 'latest', '']) {
      assert.throws(() => assertDate(bad), /not a valid chart date/);
    }
  });
});

describe('artwork proxy allowlist', () => {
  it('accepts Billboard artwork over https', () => {
    const url = assertArtUrl('https://charts-static.billboard.com/img/2025/11/a-180x180.jpg');
    assert.equal(url.hostname, 'charts-static.billboard.com');
  });

  it('refuses hosts that merely look like Billboard', () => {
    // A suffix check would let this through and turn the route into an SSRF relay.
    assert.throws(
      () => assertArtUrl('https://charts-static.billboard.com.evil.test/x.jpg'),
      /Refusing to fetch artwork/,
    );
  });

  it('refuses link-local, localhost and private targets', () => {
    for (const url of [
      'http://169.254.169.254/latest/meta-data/',
      'https://127.0.0.1:8787/api/health',
      'https://10.0.0.5/x.jpg',
      'https://[::1]/x.jpg',
    ]) {
      assert.throws(() => assertArtUrl(url), /Refusing to fetch artwork/);
    }
  });

  it('refuses non-https schemes, including the allowlisted host over http', () => {
    for (const url of ['file:///etc/passwd', 'http://charts-static.billboard.com/x.jpg']) {
      assert.throws(() => assertArtUrl(url), /Refusing to fetch artwork/);
    }
  });

  it('refuses input that is not a URL at all', () => {
    for (const url of ['', 'not a url', '/relative/path.jpg']) {
      assert.throws(() => assertArtUrl(url), /not a valid URL|Refusing to fetch artwork/);
    }
  });
});
