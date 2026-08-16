# Prediction Terminal

A Bloomberg-style terminal for [Kalshi](https://kalshi.com) prediction markets,
live stock and crypto prices, [FRED](https://fred.stlouisfed.org) economic data,
the [Billboard](https://www.billboard.com/charts/) charts, and the entertainment
feeds Kalshi settles against — driven entirely from a command prompt.

```
> GP KXFEDDECISION-27JAN-H26 1d 1y
> STK NVDA 1d 1y
> CRY BTC 1h 7d
> IMP BTC                     # Kalshi's implied BTC price, over the real one
> FRED UNRATE
> BB hot-100
> ENT film
> RT dune part three
> AWRD best picture 2026
> TRND US
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

### Stocks, crypto and implied prices

| Command | Usage | What it does |
| --- | --- | --- |
| `STK` | `STK <symbol> [1m\|1h\|1d] [range] [candle\|line]` | Stock, ETF or cash-index chart |
| `CRY` | `CRY <symbol> [1m\|1h\|1d] [range] [candle\|line]` | Crypto chart |
| `IMP` | `IMP <symbol> [event-ticker…] [interval] [range] [median\|mean]` | The same chart with Kalshi's implied price drawn over it |

```
> STK ^GSPC 1h 5d          # cash indices take their caret form
> STK SPX                  # …or an alias the terminal knows
> CRY ETH 1d 6mo line
> IMP BTC                  # pick expiries from the strip under the chart
> IMP BTC KXBTCD-26AUG2117 1h 7d
```

`IMP` is additive: re-issuing it for a symbol already on screen adds overlays to
that chart rather than opening a second one, and clicking a chip in the picker
does exactly what naming its event ticker does.

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
| `AWRD` | `AWRD [award] [year]` · `AWRD` | Nominees and winners for a ceremony; bare `AWRD` lists the awards |
| `TRND` | `TRND [country]` | What is being searched on Google right now, and why |
| `REL` | `REL <artist> [album\|song]` | An artist's releases, newest first, announced ones flagged |
| `POD` | `POD [top\|episodes] [country]` | Apple's podcast charts |

`NFLX`, `SPOT`, `TV` and `POD` take their arguments in any order, so `NFLX films
gb` and `NFLX gb films` are the same chart.

`AWRD` and `REL` read a trailing token as the year and the kind respectively, so
the free-text part can be as many words as it needs: `AWRD supporting actress
2026`, `REL sabrina carpenter song`.

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
| `KXOSCAR*`, `KXEMMY*`, `KXGRAM*`, `KXGAMEAWARDS` | the awarding body | `AWRD` |
| `KXRANKLISTGOOGLESEARCH*`, `KXGOOGLESEARCH` | Google Trends | `TRND` |
| `KXALBUMRELEASE*`, `KXSONGRELEASE*`, `KXNEWTAYLOR` | Spotify / the release itself | `REL` |
| `KXTOPPOD`, `KXROGANGUEST`, `KXPODCASTGUEST*` | Apple Podcasts | `POD` |

Each Oscar and Emmy category maps to the `AWRD` argument that looks up *that*
category, not a generic one — `KXOSCARSUPACTR` shows `AWRD supporting actress` —
because the point of the column is that clicking it answers the market's own
question.

#### Coverage

Measured against a full crawl of the open universe — 545 entertainment events
across 307 series:

| | series with a feed | 24h volume | open interest |
| --- | --- | --- | --- |
| before | 69 / 307 | 78.0% | 38.9% |
| after | 154 / 307 | 93.2% | 72.7% |

Awards were the large hole — 57 of the 85 newly-covered series and ~89% of the
newly-covered volume. `KXOSCARPIC` alone carries 2.7M in open interest, more than
any single market the terminal already covered, which is why open-interest
coverage nearly doubles while volume coverage moves 15 points.

#### What is still uncovered, and why

What remains uncovered is uncovered for a reason worth stating rather than
papering over:

* **Film release dates and casting** (`KXMOVIERELEASEDATE`, `KXROLEINPRODUCTION*`,
  `KXBOND`) settle on trade reporting — Variety, Deadline, Marvel's own site.
  Apple's Search API used to answer the release-date half; it now returns
  `resultCount: 0` for every film title while music works normally, so `REL` is
  music-only and these series are deliberately left unmapped.
* **Live events and tours** (`KXHEADLINE`, `KXTOUR`, `KXVENUEPERFORMANCE*`) have
  no key-free structured source — Ticketmaster, Songkick and Bandsintown all
  require credentials.
* **Auctions** (`KXART`, `KXHERMES*`) settle on Sotheby's, which publishes no
  machine-readable results feed.

A market with no `FEED` column has no feed, which is the honest reading; the
alternative is a command that opens a panel and cannot answer.

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
                    ├── Yahoo      equities, ETFs and cash indices
                    │   └ Nasdaq   fallback, daily bars only
                    ├── Coinbase   crypto spot
                    ├── FRED       scraped from fred.stlouisfed.org
                    ├── Billboard  scraped from billboard.com/charts
                    ├── Netflix    published TSV at netflix.com/tudum/top10
                    ├── TVmaze     public JSON API
                    ├── Steam      Valve's API + steamcharts.com
                    ├── Wikidata   WDQS SPARQL — award nominees and winners
                    ├── Google     trending-search RSS
                    ├── Apple      iTunes Search + the podcast chart JSON
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

**Awards come from Wikidata, and winners are firmer than nominees.** The bodies
that settle these markets do not answer a datacentre IP — oscars.org and its
awards database both return 403 from a container, the way fred.stlouisfed.org
resets one. Wikidata does answer, and holds the same facts as statements
(`P166` award received, `P1411` nominated for, qualified by `P585` for the
ceremony year). The asymmetry matters: editors record a winner within minutes and
fill the losing slate in over days or weeks — the 98th Academy Awards had its
Best Picture winner immediately and three of ten nominees. The panel says so
above the table and reports the count it actually got, because a partial ballot
presented as a whole one is exactly the kind of wrong number that still looks
right. A ceremony that has not happened returns nothing at all, and *that is the
answer*: `AWRD best picture 2027` reads "nominations are announced weeks before
the ceremony", not `not_found`.

Queries go to the WDQS SPARQL endpoint, not `wikidata.org/w/api.php`, which
rate-limits shared egress hard enough to 429 on the first request. Entity search
still happens against the MediaWiki API, but *server-side* through WDQS's
`wikibase:mwapi` service, so the request Wikimedia throttles comes from WDQS
rather than from here. The two halves of a ceremony are also two queries rather
than one `UNION`: combined, the join runs the label service over the product of
both statement patterns, and WDQS answers Best Picture with a 502.

**`TRND` is today's list, not December's ranking.** `KXRANKLISTGOOGLESEARCH`
settles on Google's annual Year in Search, published once, in December. What the
feed carries is what is trending right now — the evidence a trader has in August
for a market that resolves in December, the same relationship `BO` has to a
total-gross market. The panel labels it rather than implying it is the ranking.
Traffic figures are lower bounds (`500+`, `2M+`) and are rendered with the `+`,
because that is part of what the number means; an absent figure is `null`, never
zero.

**An artist search is not a keyword search.** `REL` passes Apple's
`attribute=artistTerm`, which narrows the result set but does not close it: five
of fifty live results for "taylor swift" were tribute and covers acts. Those
release weekly, so they sort to the *top* of a newest-first list, and the panel
would have answered "her latest album" with a compilation by a band called
8waves. Results are therefore held to the artist named, with containment either
way so a record credited to "Madonna & Sabrina Carpenter" still counts. The
dedupe is conservative for the mirror-image reason: collapsing bracketed suffixes
folds `1989` together with `1989 (Taylor's Version)`, and those are two releases
that two different markets trade. Apple also accepts `sort=recent` and ignores
it, so the ordering is done here and the tests pin it.

**Spotify and YouTube come via kworb.net, and the panel says so.** Neither
platform publishes those numbers in a form a server can read — charts.spotify.com
requires a login and charts.youtube.com renders client-side — so the terminal
reads the mirror the trading community actually quotes, and labels it rather
than passing it off as first-party data.

---

## The implied price

A binary contract quoted at `p` is a forecast that `P(settlement in this
contract's region) = p`. A *ladder* of them over one underlying and one expiry
therefore quotes a whole distribution, and `IMP` collapses that into the number
traders actually want: where does the market think BTC prints at 5pm?

```
IMP BTC        →  BTC / USD candles from Coinbase
                  + a dashed line per selected Kalshi expiry
```

The ladder is the unit of choice, not the contract — one strike cannot locate a
price, so the picker offers *events*. Each chip shows what that expiry implies
right now, so choosing between four of them is an informed choice.

**How the number is derived.** Every contract becomes a constraint on the
survival function `S(k) = P(price > k)`; that curve is forced to be
non-increasing; then the 50% crossing is read off it.

| Step | Why |
| --- | --- |
| Legs → survival knots | An "or above" ladder *is* `S(k)` and must not be summed. A range ladder is a mass function and must be accumulated from the top. |
| Normalise range ladders | Bid/ask spreads push the bucket sum past 1. Un-normalised, a 300-bucket ETH ladder mispriced by over $200. |
| Monotone fit (PAVA) | `S` cannot rise with the strike, but adjacent strikes are quoted by different people at different moments and a 1¢ tick is wider than the true gap. |
| Median, by default | The 50% crossing is bracketed by two real strikes with real quotes, so it needs no assumption about the tails. `mean` weights the whole distribution but has to guess beyond the end rungs — the panel reports the tail mass so you can judge. |

A one-sided book is read as `ask / 2`, not as the ask: an offer at 1¢ with no
bid means the true value is somewhere in `[0, ask]`. Reading those as mids is
what makes eighty dead wings add up to real probability and drag the crossing.

**It is checked against reality.** Over a five-day BTC ladder the implied median
tracks Coinbase spot to a mean absolute error of ~24bp, and the residual is a
consistently *positive* forward basis rather than noise — which is the thing the
overlay exists to show. Watch it converge to zero as expiry approaches.

**Kalshi has no single-stock ladders.** Price ladders exist for major crypto,
the index complex (S&P 500, Nasdaq-100, Dow), gold, oil and two FX pairs.
`STK AAPL` charts fine; `IMP AAPL` says plainly that nothing prices it.

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
| `YAHOO_API_BASE` | Yahoo chart API | Override the equity/index upstream |
| `NASDAQ_API_BASE` | `https://api.nasdaq.com` | Override the equity fallback |
| `COINBASE_API_BASE` | `https://api.exchange.coinbase.com` | Override the crypto upstream |
| `WIKIDATA_SPARQL_BASE` | WDQS | Override the awards upstream |
| `GOOGLE_TRENDS_BASE` | `https://trends.google.com` | Override the trends upstream |
| `ITUNES_API_BASE` | `https://itunes.apple.com` | Override the release-calendar upstream |
| `APPLE_RSS_BASE` | Apple marketing RSS v2 | Override the podcast-chart upstream |

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

### About equity prices from a datacentre

The same class of problem, with a different remedy. **Yahoo's API hosts
(`query1/query2.finance.yahoo.com`) answer 429 to shared datacentre
addresses.** From a laptop they are fine; from a container or CI runner they
often are not. Nothing to configure — the server falls back to `api.nasdaq.com`
automatically — but the fallback is narrower, and the terminal says which
provider answered in the chart's `SRC` field rather than leaving it invisible:

* **Nasdaq has daily bars only.** An intraday request reports that plainly
  instead of quietly serving daily bars for an hourly chart.
* **Nasdaq has no S&P 500 or Dow feed** — only its own indices (`COMP`, `NDX`).
  Substituting SPY for `^GSPC` would be off by a factor of ten against a
  `KXINX` ladder, so the terminal errors rather than guessing.

Crypto has no such issue: Coinbase answers from anywhere.

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
| `GET /api/spot/:class/:symbol` | Live quote (`:class` is `stock` or `crypto`) |
| `GET /api/spot/:class/:symbol/candles?interval=&start=&end=` | Candles on Kalshi's 1/60/1440-minute grid |
| `GET /api/spot/search?q=&class=` | Symbol search, flagged with whether a ladder prices it |
| `GET /api/implied/underlyings` | Symbols with a mapped Kalshi ladder |
| `GET /api/implied/candidates?symbol=` | Ladders pricing a symbol, each with its live implied price |
| `GET /api/implied/series?event=&interval=&start=&end=&method=` | Implied price through time for one ladder |
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
| `GET /api/ent/awards?q=&year=&limit=` | Award nominees and winners |
| `GET /api/ent/awards/list` | Award categories with a short-name alias |
| `GET /api/ent/trends?geo=&limit=` | Google trending searches |
| `GET /api/ent/releases?q=&kind=&limit=` | An artist's releases, upcoming flagged |
| `GET /api/ent/podcasts?view=&country=&limit=` | Apple podcast chart |
| `GET /api/health` | Liveness, cache stats, whether a FRED key is set |

Errors are JSON: `{ error, code, hint? }`. The `hint` is written to be shown to
a person and is surfaced verbatim in the panel.

---

## Development

```bash
npm run dev          # server + client with reload
npm test             # 274 tests
npm run typecheck    # client and server
npm run check        # typecheck + test
```

Tests cover the parsers and normalisers rather than the network: the scraped
parsers run against fixtures captured from the real pages, the Kalshi, Yahoo,
Nasdaq and Coinbase normalisers against trimmed real API responses, and
`test/fred.integration.test.ts` exercises the whole FRED scrape path against a
local fixture server — which is how that path stays covered on networks where
the live host refuses to answer.

The implied-price maths in `src/shared/implied.ts` is the most heavily tested
part of the codebase, because it is the one piece whose output looks plausible
when it is wrong: a mishandled ladder shape still returns a number near the
money. `test/implied.test.ts` checks each stage against arithmetic, including
that an "or above" ladder and the equivalent range ladder price to the same
level, and `test/implied-series.test.ts` covers the historical assembly — a rung
that stops printing carries forward, and no rung is ever priced with a candle it
did not yet have.

The entertainment tests lean on the cases where a plausible-looking parser reads
the wrong number without ever failing: the two header collisions above, a film
with no Tomatometer, a chart with no movement column, and an upstream that
answers `200 OK` with "no data available" instead of an error.

`test/culturefeeds.test.ts` covers the four newer feeds the same way, and every
case in it is a bug that was caught against the live upstream rather than
imagined: a covers band heading an artist's "latest release", a dedupe that folds
a re-recording into the original, the namespaced RSS siblings that collapse onto
each other under an HTML parse and put an image URL where the traffic figure
belongs, and the Game Awards alias whose real Wikidata label — spelled with a
U+2212 minus — resolves to nothing at all.

---

## Scope

Read-only market data. Nothing here places an order, holds a credential, or
touches a Kalshi account — the terminal uses only public, unauthenticated
endpoints, and no price or entertainment feed needs a key either. Scraped
sources are third-party sites whose markup can change without notice; the
parsers are written to degrade with a diagnosable error rather than silently
return wrong numbers.

The implied price is a reading of public quotes, not advice, and it is only as
good as the ladder underneath it — an expiry with a thin or one-sided book will
imply a number the panel reports the tail mass for precisely so you can
distrust it.

The entertainment feeds show what a market is *likely* to settle against, not
what it *will*. Kalshi resolves against its own stated settlement sources under
its own rules, and a scraped page can lag, revise, or disagree. Read these
panels as the public evidence, not as the settlement.

That caveat is sharpest for `AWRD` and `TRND`. Wikidata is a community database,
not the Academy — it is what editors have recorded so far, which for a nominee
slate is often less than the full field for days after the announcement. And the
Google Trends feed is the daily trending list, while the markets quoting it
settle on an annual ranking published in December. Both panels label their source
on screen for that reason.
