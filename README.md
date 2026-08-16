# Prediction Terminal

A Bloomberg-style terminal for [Kalshi](https://kalshi.com) prediction markets,
[FRED](https://fred.stlouisfed.org) economic data, the
[Billboard](https://www.billboard.com/charts/) charts, and the entertainment
feeds Kalshi settles against — driven entirely from a command prompt.

```
> GP KXFEDDECISION-27JAN-H26 1d 1y
> FRED UNRATE
> BB hot-100
> ENT film
> RT dune part three
```

Type a command, get a panel. Panels tile into a grid, poll on their own timers,
and every table row is clickable so the mouse and the keyboard drive the same
code path.

---

## Quick start

```bash
npm install
npm run dev          # API on :8787, client on :5173 with proxying
```

Open http://localhost:5173 and type `HELP`.

For a single-process production build:

```bash
npm run build
npm start            # serves the built client and the API on :8787
```

---

## Commands

Run `HELP` in the terminal for the live reference — it is generated from the
command table, so it can never drift from what the app does. `HELP <command>`
shows usage and runnable examples.

### Kalshi markets

| Command | Usage | What it does |
| --- | --- | --- |
| `GP` | `GP <ticker> [1m\|1h\|1d] [range] [candle\|line]` | Price chart with volume and a crosshair readout |
| `DES` | `DES <ticker>` | Quote, contract description, settlement rules |
| `OB` | `OB <ticker>` | Order-book ladder with depth bars |
| `TAS` | `TAS <ticker>` | Time and sales tape |
| `SRCH` | `SRCH <words>` | Search open events, grouped with their strike ladders |
| `EVT` | `EVT <event-ticker>` | Every contract in an event |
| `TOP` | `TOP [volume\|gainers\|losers\|oi\|liquidity]` | Leaderboards |
| `ENT` | `ENT [music\|film\|tv\|games\|awards\|celeb]` | The entertainment book, by genre, with a link to each market's settlement feed |

Arguments after the ticker on `GP` are order-independent — `GP X 1d 1y line`
and `GP X line 1y 1d` are the same chart.

### Data sources

| Command | Usage | What it does |
| --- | --- | --- |
| `FRED` | `FRED <series-id> [start] [end]` | Economic series chart plus units, frequency and vintage |
| `FSRCH` | `FSRCH <words>` | Find a FRED series id |
| `BB` | `BB [chart-slug] [YYYY-MM-DD]` | A Billboard chart as a ranked table |
| `BB` | `BB CHARTS` | List the chart slugs |
| `RT` | `RT <title>` · `RT SEARCH <words>` | Tomatometer and Popcornmeter, with review counts |
| `NFLX` | `NFLX [tv\|films] [global\|<country>]` | Netflix Top 10, with views and hours viewed |
| `SPOT` | `SPOT [global\|<country>] [daily\|weekly]` | Spotify streaming chart |
| `YT` | `YT [today\|alltime\|trending]` | YouTube music video views |
| `BO` | `BO [YYYY-MM-DD]` | Domestic daily box office |
| `STEAM` | `STEAM [game\|appid]` | Live concurrent players, or the most-played leaderboard |
| `TV` | `TV [YYYY-MM-DD] [country]` | What airs that day, by network |

`NFLX`, `SPOT` and `TV` take their arguments in any order, so `NFLX films gb`
and `NFLX gb films` are the same chart.

### Entertainment markets, and what settles them

`ENT` exists because Kalshi files ~2,500 series under one flat `Entertainment`
category with no sub-category, and its `/events` endpoint accepts a `category`
parameter and then ignores it. The only usable discriminator is the ticker, and
Kalshi's tickers are disciplined enough to classify on — `KXOSCARPIC`,
`KXNETFLIXRANKSHOW`, `KXGAMEAWARDS`. Genres are tags rather than a partition,
because an Oscar market is both `film` and `awards`.

Each row carries a FEED column naming the command that shows the data the market
resolves against. Those pairings are read off each series' own
`settlement_sources`, not guessed:

| Kalshi series | Settles on | Command |
| --- | --- | --- |
| `KXRT`, `KXRTCOMPARE` | rottentomatoes.com | `RT` |
| `KXNETFLIXRANK*`, `KXNETFLIXTOPVIEWS*` | Netflix's own Top 10 publication | `NFLX` |
| `KXTOPARTIST*`, `KXTOPSONGSPOTIFY`, `KXARTISTSTREAMSY` | Spotify | `SPOT` |
| `KXYTVIEWSW`, `KXYTTOPSONGW`, `KXYTDAILYTOPVIDEO` | YouTube | `YT` |
| `KXTOPSONG`, `KXTOPALBUM`, `KXALBUMEQUIV` | Billboard / Luminate | `BB` |
| `KXSTEAM*`, `GAMERANK` | Steam | `STEAM` |
| `KXBIGBROTHER*`, `KXDWTS`, `KXSNL` | what actually aired | `TV` |

### Workspace

| Command | Usage |
| --- | --- |
| `W` | `W` · `W ADD <ticker>` · `W DEL <ticker>` · `W CLEAR` |
| `LAY` | `LAY <1-4>` — panel columns |
| `THEME` | `THEME amber\|green\|ice` |
| `CLS` | `CLS` · `CLS ALL` |
| `REFRESH` | Force-reload every panel |
| `HELP` | `HELP [command]` |

### Keys

| Key | Action |
| --- | --- |
| `Enter` | Run |
| `↑` / `↓` | Walk command history |
| `Tab` | Complete the verb |
| `Esc` | Clear the line |
| `Ctrl`+`←`/`→` | Move focus between panels |
| `Ctrl`+`W` | Close the focused panel |
| `Ctrl`+`L` | Clear the message log |
| `/` | Focus the prompt from anywhere |

---

## How it works

```
browser (Vite + TypeScript, no framework)
  └── /api/*  ──► Express server
                    ├── Kalshi     trade-api v2  (JSON)
                    ├── FRED       scraped from fred.stlouisfed.org
                    ├── Billboard  scraped from billboard.com/charts
                    ├── Netflix    published TSV at netflix.com/tudum/top10
                    ├── TVmaze     public JSON API
                    ├── Steam      Valve's API + steamcharts.com
                    └── scraped    rottentomatoes.com, boxofficemojo.com, kworb.net
```

The server exists for three reasons the browser cannot handle alone: none of
these upstreams allow cross-origin reads, most of them serve HTML that has to be
parsed somewhere, and N polling panels should make 1 upstream request.
Responses are cached with per-source TTLs and concurrent misses for the same
key are collapsed onto a single in-flight request.

Charts are [lightweight-charts](https://github.com/tradingview/lightweight-charts)
v5. The chart theme is read from live CSS custom properties, so `THEME` restyles
every open chart with no per-panel bookkeeping.

### A few things worth knowing

**Kalshi prices are dollars, displayed as cents.** A contract settles at $1, so
`52¢` is both the price and a 52% implied probability. The upstream API speaks
fixed-point decimal *strings* (`"0.5200"`, `"12645.98"`); none of that leaks
past the server boundary.

**Search runs against events, not markets.** Kalshi's open-market list is
~99.98% auto-generated multivariate parlay legs — paging `/markets` returned
11,998 of them in the first 12,000 rows, two of which were real contracts.
`/events?with_nested_markets=true` excludes them entirely. The server crawls
that into a snapshot at boot (~9,800 events / ~82,000 markets) so the first
`SRCH` does not pay for the crawl.

That crawl used to stop at 20 pages, which covered 4,000 of the ~9,800 open
events. Because the upstream does not order by category, the truncation fell
unevenly: it hid 265 of the 545 open Entertainment events, so half of them were
simply unreachable from `SRCH`. The page cap now leaves headroom above the open
universe, and the loop still stops as soon as the cursor runs out.

**Quiet candles are drawn flat, not skipped.** Kalshi returns a bucket with
only `previous_dollars` when nothing printed in the period. Those become flat
bars at the previous close, tagged `NO TRADE` in the crosshair legend, so a
chart stays continuous instead of gapping.

**The order book is inverted for you.** Kalshi quotes two *bid* ladders — YES
and NO — with no ask side, because an offer to sell YES at `p` is a bid to buy
NO at `1 - p`. The server converts the NO ladder into YES ask terms once, so
every panel sees a conventional bid/ask.

**"No score yet" is not zero.** A film awaiting reviews reports a `null`
Tomatometer, never `0` — the distinction is the entire point when the market is
"will it score above 85". The same rule holds across these feeds: a box office
day Mojo has not posted is a `not_found` with a "grosses land the following
afternoon" hint, not a parse failure, and a chart with no movement column
reports unknown movement rather than claiming every row held its position.

**Two columns can normalise to the same key.** Box Office Mojo prints `YD`
(yesterday's rank) beside `%± YD` (the day-over-day change), and kworb prints
`Streams` beside `Streams+`. Strip the punctuation and each pair collapses onto
one key, so the parser silently reads a rank where a percentage belongs. Both
header normalisers keep the distinguishing character — `%` becomes `pct`, `+`
becomes `plus` — and the tests assert exactly that.

**Netflix publishes the data, so it is not scraped.** The Top 10 site is backed
by TSV files Netflix publishes itself, carrying the same views and hours-viewed
figures the `KXNETFLIX*` markets settle on. The country file is ~31 MB, so it is
fetched at most once per TTL, reduced immediately to the latest week for *every*
country, and only that reduction is cached — `NFLX us` and `NFLX gb` share one
download. Expect the first country request after a cold start to take ~10s.

**Spotify and YouTube come via kworb.net, and the panel says so.** Neither
platform publishes those numbers in a form a server can read — charts.spotify.com
requires a login and charts.youtube.com renders client-side — so the terminal
reads the mirror the trading community actually quotes, and labels it rather
than passing it off as first-party data.

---

## Configuration

All optional. Copy `.env.example` to `.env` or export directly.

| Variable | Default | Purpose |
| --- | --- | --- |
| `PORT` | `8787` | API server port |
| `HOST` | `127.0.0.1` | API bind address |
| `FRED_API_KEY` | — | Enables an official-API fallback when the FRED scrape fails |
| `RATE_LIMIT` | `600` | Max API calls per IP per minute |
| `KALSHI_API_BASE` | Kalshi v2 | Override the upstream base URL |
| `FRED_WEB_BASE` | `https://fred.stlouisfed.org` | Override for testing against a fixture |

### About FRED and `FRED_API_KEY`

FRED data is scraped, as intended: observations come from
`/graph/fredgraph.csv?id=<ID>` (the endpoint behind every FRED graph's
"Download → CSV" button) and metadata is parsed out of the `/series/<ID>` page.

There is a real-world catch. **`fred.stlouisfed.org` resets connections from
datacentre and cloud IP ranges.** From a laptop the scrape works; from a VPS,
container, or CI runner it usually will not, and the terminal will show:

> fred.stlouisfed.org refused the connection — hosts behind bot protection
> commonly do this to datacentre and cloud IPs.

Setting `FRED_API_KEY` (free, from
[fred.stlouisfed.org/docs/api/api_key.html](https://fred.stlouisfed.org/docs/api/api_key.html))
makes the server fall back to `api.stlouisfed.org`, which does answer from those
networks. The scrape is always tried first; the key is only a safety net.

---

## API

| Route | Returns |
| --- | --- |
| `GET /api/kalshi/markets/:ticker` | Normalised market |
| `GET /api/kalshi/markets/:ticker/orderbook?depth=` | Book with YES asks derived |
| `GET /api/kalshi/markets/:ticker/trades?limit=` | Recent prints |
| `GET /api/kalshi/markets/:ticker/candles?interval=&start=&end=` | Candles (`interval` ∈ 1, 60, 1440) |
| `GET /api/kalshi/events/:eventTicker` | Event with nested markets |
| `GET /api/kalshi/search?q=&limit=` | Ranked event search |
| `GET /api/kalshi/top?sort=&limit=` | Leaderboards |
| `GET /api/fred/series/:id?start=&end=` | Series metadata + observations |
| `GET /api/fred/search?q=&limit=` | FRED series search |
| `GET /api/billboard/chart/:slug?date=` | Chart entries |
| `GET /api/billboard/charts` | Known chart slugs |
| `GET /api/billboard/art?u=` | Artwork proxy (allowlisted hosts only) |
| `GET /api/ent/markets?genre=&limit=` | Entertainment events, genre-tagged, with settlement feeds |
| `GET /api/ent/rt?q=` | Rotten Tomatoes title with both scores |
| `GET /api/ent/rt/search?q=&limit=` | Rotten Tomatoes title search |
| `GET /api/ent/netflix?category=&scope=` | Netflix Top 10 for a week |
| `GET /api/ent/spotify?scope=&period=&limit=` | Spotify chart |
| `GET /api/ent/youtube?view=&limit=` | YouTube chart |
| `GET /api/ent/charts?source=` | Known streaming chart slugs |
| `GET /api/ent/boxoffice?date=` | Domestic daily box office |
| `GET /api/ent/steam?q=&limit=` | Steam leaderboard, or one game's live count |
| `GET /api/ent/tv?date=&country=` | TV schedule for a day |
| `GET /api/health` | Liveness, cache stats, whether a FRED key is set |

Errors are JSON: `{ error, code, hint? }`. The `hint` is written to be shown to
a person and is surfaced verbatim in the panel.

---

## Development

```bash
npm run dev          # server + client with reload
npm test             # 168 tests
npm run typecheck    # client and server
npm run check        # typecheck + test
```

Tests cover the parsers and normalisers rather than the network: the scraped
parsers run against fixtures captured from the real pages, the Kalshi
normalisers against trimmed real API responses, and `test/fred.integration.test.ts`
exercises the whole FRED scrape path against a local fixture server — which is
how that path stays covered on networks where the live host refuses to answer.

The entertainment tests lean on the cases where a plausible-looking parser reads
the wrong number without ever failing: the two header collisions above, a film
with no Tomatometer, a chart with no movement column, and an upstream that
answers `200 OK` with "no data available" instead of an error.

---

## Scope

Read-only market data. Nothing here places an order, holds a credential, or
touches a Kalshi account — the terminal uses only public, unauthenticated
endpoints, and no entertainment feed needs a key either. Scraped sources are
third-party sites whose markup can change without notice; the parsers are
written to degrade with a diagnosable error rather than silently return wrong
numbers.

The entertainment feeds show what a market is *likely* to settle against, not
what it *will*. Kalshi resolves against its own stated settlement sources under
its own rules, and a scraped page can lag, revise, or disagree. Read these
panels as the public evidence, not as the settlement.
