# Prediction Terminal

A Bloomberg-style terminal for three prediction markets — [Kalshi](https://kalshi.com),
[Polymarket](https://polymarket.com) and [Polymarket US](https://polymarket.us) —
plus live stock and crypto prices, the [Alpaca](https://alpaca.markets) news
wire, [FRED](https://fred.stlouisfed.org) economic data, the
[Billboard](https://www.billboard.com/charts/) charts, and the entertainment
feeds these markets settle against, driven entirely from a command prompt.

```
> GP KXFEDDECISION-27JAN-H26 1d 1y
> DES pm:will-there-be-no-change-in-fed-interest-rates-after-the-september-2026-meeting-615
> XV KXFEDDECISION-26OCT      # the same ladder, priced at all three brokers
> XV fed                      # what else more than one broker lists
> STK NVDA 1d 1y
> IMP BTC                     # Kalshi's implied BTC price, over the real one
> NEWS NVDA
> FRED UNRATE
> BB hot-100
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

### Prediction markets

| Command | Usage | What it does |
| --- | --- | --- |
| `GP` | `GP [venue:]<ticker> [1m\|1h\|1d] [range] [candle\|line]` | Price chart with volume and a crosshair readout |
| `DES` | `DES [venue:]<ticker>` | Quote, contract description, settlement rules |
| `OB` | `OB [venue:]<ticker>` | Order-book ladder with depth bars |
| `TAS` | `TAS [venue:]<ticker>` | Time and sales tape |
| `SRCH` | `SRCH <words> [kalshi\|pm\|pmus]` | Search open events at every venue at once, grouped with their strike ladders |
| `EVT` | `EVT [venue:]<event-ticker>` | Every contract in an event |
| `TOP` | `TOP [volume\|gainers\|losers\|oi\|liquidity] [venue]` | Leaderboards |
| `XV` | `XV [words]` · `XV [venue:]<event-ticker>` | Cross-venue: what else lists this, and at what price |
| `ENT` | `ENT [music\|film\|tv\|games\|awards\|celeb]` | Kalshi's entertainment book, by genre, with a link to each market's settlement feed |

Arguments after the ticker on `GP` are order-independent — `GP X 1d 1y line`
and `GP X line 1y 1d` are the same chart.

### Naming a venue

Every market command takes one reference: an optional venue prefix, then the
identifier that venue uses. An unprefixed reference is Kalshi, so nothing that
worked before means anything different now.

| Prefix | Venue | Identifier |
| --- | --- | --- |
| *(none)* or `kx:` | Kalshi | ticker, `KXFEDDECISION-26OCT` |
| `pm:` | Polymarket International | slug, `fed-decision-in-october-20260617190323537` |
| `pmus:` | Polymarket US | slug, `usfed-fomc-2026-10-28` |

```
> DES KXFEDDECISION-26OCT-T3.75
> OB pmus:apdc-jerpowgov-2026-12-31
> GP pm:will-there-be-no-change-in-fed-interest-rates-after-the-september-2026-meeting-615 1h 7d
> W ADD pm:fed-decision-in-october-20260617190323537
```

Case is part of the identifier, not decoration: Kalshi 404s a lower-case ticker
and both Polymarkets 404 an upper-case slug, so the prefix decides the folding
and you can type either. `pm`, `poly` and `intl` all mean the international
book; `pmus`, `polyus` and `us` all mean the US one.

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
| `NEWS` | `NEWS [symbol…] [count] [window]` | The news wire, for a symbol or the whole tape |
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

```
> NEWS                        # the whole wire
> NEWS NVDA                   # …filtered to one ticker
> NEWS AAPL MSFT 50           # two tickers, 50 headlines
> NEWS BTCUSD 30d             # crypto is tagged as a pair
```

`NEWS` is the one command that needs a key — see
[About the news wire](#about-the-news-wire-and-its-key). Its arguments are
order-independent and told apart by shape: a bare integer is a headline count, a
duration is the look-back window, anything else is a symbol. Each headline links
to the publisher, and the rest of the row opens the chart of what the story is
about.

### Cross-venue: lining the brokers up

Three exchanges list many of the same questions and agree on no identifier for
any of them:

| | Kalshi | Polymarket | Polymarket US |
| --- | --- | --- | --- |
| Series | `KXFEDDECISION` | `fomc` | `usfed-fomc` |
| Event | `KXFEDDECISION-26OCT` | `fed-decision-in-october-2026…` | `usfed-fomc-2026-10-28` |
| Rung | `Cut 25bps` | `25 bps decrease` | `25 bps Decrease` |

Nothing in any payload connects those, so `XV` does it from the language.

```
> XV                    # every series more than one broker lists
> XV fed                # …narrowed
> XV KXFEDDECISION-26OCT
```

```
XV KXFEDDECISION-26OCT   Fed decision in Oct 2026?

  KAL   KXFEDDECISION-26OCT                          LINKED   73d 00h
  PM    fed-decision-in-october-20260617190323235…   STRONG   73d 06h
  PMUS  usfed-fomc-2026-10-28                        STRONG   87d 05h

  CONTRACT             KAL BID  KAL ASK   PM BID  PM ASK   PMUS BID  PMUS ASK  DIVERGE  EDGE
  Cut >25bps                1¢       3¢     1.4¢    1.7¢         1¢        2¢     0.5¢   -1¢
  Cut 25bps                 4¢       5¢     4.3¢    4.9¢         7¢        8¢       3¢   +2¢
  Fed maintains rate       70¢      73¢      71¢     72¢        70¢       71¢       1¢    0¢
  Hike 25bps               23¢      24¢      23¢     24¢        23¢       24¢       0¢   -1¢
  Hike >25bps               1¢       3¢     0.9¢      1¢         1¢        3¢     1.1¢    0¢
```

**DIVERGE is what the books disagree by; EDGE is what could be traded.** The
first is the widest mid minus the narrowest. The second is the cheapest ask
anywhere against the richest bid anywhere, so it is positive only when the books
are genuinely crossed between brokers — before fees, and with both legs still to
fill. They differ by the width of the spreads, which is exactly the part that is
not profit.

**Every pairing carries its confidence and its reason.** Hover any chip.

| Band | Means |
| --- | --- |
| `LINKED` | Named in the curated table of series identifiers, and found in both live catalogues |
| `STRONG` / `LIKELY` / `WEAK` | The matcher's reading of two titles, scored and labelled as such |

The curated table is short on purpose — it exists for the pairs no amount of
reading can reach, like `Fed decision in Oct 2026?` against a series slug of
`fomc`, which share not one token. Everything else is left to the matcher, and
every curated entry is re-checked against the live catalogues on each request:
an identifier that no longer exists is dropped, so the table can go stale
without ever quoting a market that is gone.

The matcher itself lives in `src/shared/match.ts` and touches no network, so its
judgement is tested against fixed strings rather than against whatever happens
to be listed today. The cases that matter are the near misses:

* **Numbers are the question.** `Big Brother Season 28 · 2nd place` and
  `…3rd place` share five words out of six. Set overlap cannot see that the word
  they differ on is the whole market, so numbers are weighed separately — and
  *partial* agreement is not agreement, or the shared `28` would cover for the
  `2` against the `3`.
* **A series recurs; an event does not.** Matching series ignores the year, so
  `Fed decision in Oct 2026?` and `Fed Decision in September?` are recognised as
  one series. Matching events counts it, so October's meeting is never quoted
  against January's.
* **Units get welded to their numbers.** Kalshi writes `25bps`, Polymarket
  writes `25 bps`. Splitting digits from letters is what lets them meet.
* **Comparators survive.** `>25bps` and `50+ bps` both keep the fact that they
  are open-ended, so an open-ended rung cannot be paired onto an exact one.
* **Ladders pair greedily, best first.** Each rung is struck off once matched,
  because a five-rung ladder of near-identical labels will otherwise map two of
  one venue's rungs onto one of the other's. Anything left over is reported
  under the table rather than dropped.

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
                    ├── Polymarket gamma-api  catalogue
                    │   ├ clob       book and price history
                    │   └ data-api   public print tape
                    ├── PolymarketUS gateway  catalogue, book, BBO
                    ├── Yahoo      equities, ETFs and cash indices
                    │   └ Nasdaq   fallback, daily bars only
                    ├── Coinbase   crypto spot
                    ├── Alpaca     news wire (Benzinga), the one keyed feed
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

**Prices are dollars, displayed as cents.** A contract settles at $1, so `52¢`
is both the price and a 52% implied probability. Each upstream has its own
dialect — Kalshi speaks fixed-point decimal *strings* (`"0.5200"`,
`"12645.98"`), Polymarket wraps prices in `{value, currency}` objects and ships
JSON arrays as JSON *strings* — and none of that leaks past the server boundary.
A normalised market from any of the three is the same shape.

**Only one venue quotes an ask.** Kalshi publishes two *bid* ladders; Polymarket
runs one book per token pair and quotes only the first token. Both are converted
into a conventional YES bid/ask with a derived NO ladder, once, on the server —
so a Kalshi book and a Polymarket book can be read side by side without
remembering which inversion applies to which.

**A blank is not a zero.** Polymarket US's public catalogue carries no volume,
no open interest and no resting depth at all, so those fields are `null` rather
than `0` and render as `--`. The same rule decides who appears on a leaderboard:
a market whose venue does not publish the sorted figure is left off that board
entirely, and the panel says which venue and why, because ranking it as zero
would be a statement about its API dressed up as one about its book.

**Polymarket US charts and tapes need a key, so there are none.** Its price
history and prints live behind `api.polymarket.us`, which requires an account
and an API key; the public gateway serves catalogue, book and BBO. The terminal
holds no credentials, so `GP` and `TAS` on a `pmus:` market name the endpoint
that would answer and what it costs, instead of drawing an empty panel.

**Polymarket publishes prices, not candles.** Its history is a sample series —
a price at a moment, with no size attached. The server buckets those onto the
same 1/60/1440-minute grid Kalshi uses, so both venues' bars land on one x-axis,
leaves volume `null` rather than inventing a zero, and says so in the panel
header. Two quirks of that upstream are worked around rather than papered over:
a `startTs`/`endTs` window wider than exactly 15 days returns an empty history
with a `200`, so wide windows are stitched from chunks or fall back to a named
lookback; and its offset pagination refuses to go past ~2,300 events, which is
why that crawl is ordered by 24h volume — the truncation then drops the dead
tail rather than a random slice.

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

**"No news today" is not "no news".** Alpaca's `start` defaults to the beginning
of the current day, so `NEWS MU` before lunchtime on a thinly-covered ticker
answers `200 OK` with an empty list — which reads as "nothing has happened"
rather than "nothing since midnight". The window is therefore always sent
explicitly, defaults to seven days, and the panel states the width it used, so
an empty wire is a fact about a stated period. The upstream's
`exclude_contentless` filter stays off for the same reason: a halt notice with
no article body is the fastest-moving item on the wire, not an empty row.

**A news wire sorts on publication, not on the last edit.** Alpaca orders by
`updated_at`, so a story filed at 09:02 and corrected at 21:10 outranks
everything published in between — the top of the panel would be the newest
*correction*, not the newest news. The server re-sorts on `created_at` after
normalising.

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
| `ALPACA_API_KEY_ID` | — | Enables `NEWS`. Also read from `APCA_API_KEY_ID` |
| `ALPACA_API_SECRET_KEY` | — | The other half of the pair. Also `APCA_API_SECRET_KEY` |
| `RATE_LIMIT` | `600` | Max API calls per IP per minute |
| `KALSHI_API_BASE` | Kalshi v2 | Override the upstream base URL |
| `POLYMARKET_GAMMA_BASE` | `https://gamma-api.polymarket.com` | Polymarket catalogue |
| `POLYMARKET_CLOB_BASE` | `https://clob.polymarket.com` | Polymarket book and price history |
| `POLYMARKET_DATA_BASE` | `https://data-api.polymarket.com` | Polymarket print tape |
| `POLYMARKET_US_API_BASE` | `https://gateway.polymarket.us` | Polymarket US public gateway |
| `FRED_WEB_BASE` | `https://fred.stlouisfed.org` | Override for testing against a fixture |
| `YAHOO_API_BASE` | Yahoo chart API | Override the equity/index upstream |
| `NASDAQ_API_BASE` | `https://api.nasdaq.com` | Override the equity fallback |
| `COINBASE_API_BASE` | `https://api.exchange.coinbase.com` | Override the crypto upstream |
| `ALPACA_DATA_BASE` | `https://data.alpaca.markets/v1beta1` | Override the news upstream |

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

### About the news wire, and its key

`NEWS` is the only command here that does not work out of the box, and the
reason is not laziness: there is no unauthenticated equity news feed that is
both licensed to redistribute and stable enough to parse. Scraping one would
mean shipping a parser that breaks on a template change and a redistribution
question nobody wants. Alpaca resells Benzinga's wire as JSON, keyed to an
account, and a **free paper-trading account issues a working pair** — no funding,
no trading permissions needed for this feed.

```bash
export ALPACA_API_KEY_ID=PK...
export ALPACA_API_SECRET_KEY=...
npm run dev
```

Alpaca's own SDK variable names (`APCA_API_KEY_ID`, `APCA_API_SECRET_KEY`) are
read as a fallback, so an environment already set up for Alpaca needs nothing
new. The pair is sent as request headers to `data.alpaca.markets` and nowhere
else — never in a query string, never to the browser, never into a log line —
and `GET /api/health` reports only whether one is configured, never its value.

The two failures worth distinguishing are distinguished:

* **No key** → HTTP 503 `not_configured`, with the variable names to set. The
  server never leaves the process, and says so on startup too.
* **Wrong key** → HTTP 502 `bad_credentials`. Alpaca answers a bad pair with a
  401, which the generic path would have reported as "this host may be blocking
  your IP" — a hint that sends you hunting a network problem you do not have.

Everything else in the terminal keeps working with no key at all.

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
| `GET /api/venue/:venue/markets/:id` | Normalised market at any venue (`kalshi`, `polymarket`, `polymarket-us`) |
| `GET /api/venue/:venue/markets/:id/orderbook?depth=` | Book with the NO ladder derived |
| `GET /api/venue/:venue/markets/:id/trades?limit=` | Recent prints (501 at Polymarket US) |
| `GET /api/venue/:venue/markets/:id/candles?interval=&start=&end=` | Candles (501 at Polymarket US) |
| `GET /api/venue/:venue/events/:id` | Event with nested markets |
| `GET /api/venue/:venue/search?q=&limit=` | Ranked event search |
| `GET /api/venue/:venue/top?sort=&limit=` | Leaderboards |
| `GET /api/venue/:venue/catalogue` | Snapshot size, age, and whether the crawl was truncated |
| `GET /api/xv/series?q=&limit=` | Series more than one broker lists, with confidence and reason |
| `GET /api/xv/compare?event=&venue=` | One question, quoted at every venue, with divergence and edge |
| `GET /api/spot/:class/:symbol` | Live quote (`:class` is `stock` or `crypto`) |
| `GET /api/spot/:class/:symbol/candles?interval=&start=&end=` | Candles on Kalshi's 1/60/1440-minute grid |
| `GET /api/spot/search?q=&class=` | Symbol search, flagged with whether a ladder prices it |
| `GET /api/implied/underlyings` | Symbols with a mapped Kalshi ladder |
| `GET /api/implied/candidates?symbol=` | Ladders pricing a symbol, each with its live implied price |
| `GET /api/implied/series?event=&interval=&start=&end=&method=` | Implied price through time for one ladder |
| `GET /api/news?symbols=&limit=&days=` | Headlines, newest first (needs Alpaca keys) |
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
| `GET /api/health` | Liveness, cache stats, whether the FRED and Alpaca keys are set |

Errors are JSON: `{ error, code, hint? }`. The `hint` is written to be shown to
a person and is surfaced verbatim in the panel.

---

## Development

```bash
npm run dev          # server + client with reload
npm test             # 399 tests
npm run typecheck    # client and server
npm run check        # typecheck + test
```

Tests cover the parsers and normalisers rather than the network: the scraped
parsers run against fixtures captured from the real pages, the Kalshi, Yahoo,
Nasdaq and Coinbase normalisers against trimmed real API responses, and
`test/fred.integration.test.ts` exercises the whole FRED scrape path against a
local fixture server — which is how that path stays covered on networks where
the live host refuses to answer. `test/news.integration.test.ts` does the same
for the news wire, which nothing in CI has a key for: it asserts that the key
pair goes in the headers rather than the query string, that the request
overrides the two upstream defaults that would otherwise cost headlines, and
that a missing key and a rejected key are different, actionable errors.

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

`test/match.test.ts` covers the cross-venue matcher for the same reason: pairing
two brokers' listings incorrectly still produces a tidy table of two prices, and
a reader will take the gap between them for an edge. So the cases are the near
misses — `2nd place` against `3rd place`, October's Fed meeting against
January's, `NFL Champion` against `NFL Rookie of the Year` — alongside the real
five-rung FOMC ladder, which must line up across all three venues without ever
mapping two rungs onto one.

`test/polymarketus.test.ts` pins down the field the normaliser deliberately does
not read. `outcomePrices` exists on every Polymarket US market and means
different things on different endpoints: in a nested event listing it holds
`[bestBid, bestAsk]`, and the same market fetched by slug holds
`[askToBuyYes, askToBuyNo]` — for one contract, `["0.4600","0.4780"]` in one
place and `["0.4780","0.54"]` in the other. Nor does the `outcomes` array order
it: one payload labels a market `["Yes","No"]` and its neighbour `["No","Yes"]`
with both price arrays still in bid/ask order. Reading either would silently
mislabel a bid as an ask on half the book, so the quote comes from
`bestBidQuote`/`bestAskQuote` alone, and the tests assert the reading does not
move when those fields do.

---

## Scope

Read-only market data. Nothing here places an order or touches an account at
any of the three exchanges — every market, price and entertainment feed is a
public, unauthenticated endpoint. That is a deliberate limit, and it is visible:
Polymarket US serves its price history and prints only to an authenticated
caller, so those two panels say so rather than appearing broken.

The news wire is the one exception to "holds no credential", and it is worth
stating plainly: `NEWS` sends an Alpaca key pair to `data.alpaca.markets` to
read headlines. Alpaca issues one pair per account, and the same pair can trade
that account, so the terminal treats it accordingly — it is read from the
environment, sent to that one host as a header, never written to a log or a
response, and never used for anything but a `GET` on the news endpoint. Keys
from a paper account carry no real money and are the recommended pair to use
here. Leave the variables unset and every other command behaves exactly as
before.

Scraped sources are third-party sites whose markup can change without notice;
the parsers are written to degrade with a diagnosable error rather than silently
return wrong numbers.

Cross-venue pairings are a reading of two catalogues, not a statement from
either exchange. A `LINKED` pair is one the terminal names and re-checks against
both live catalogues; everything else is scored text, labelled with its
confidence and the terms it matched on. Read the EDGE column the same way: it is
the arithmetic on two public books, before fees, latency, and the fact that both
legs have to fill. Contracts that look alike can still settle on different
sources under different rules, and the panel showing them side by side is not a
claim that they will settle the same way.

The implied price is a reading of public quotes, not advice, and it is only as
good as the ladder underneath it — an expiry with a thin or one-sided book will
imply a number the panel reports the tail mass for precisely so you can
distrust it.

The entertainment feeds show what a market is *likely* to settle against, not
what it *will*. Kalshi resolves against its own stated settlement sources under
its own rules, and a scraped page can lag, revise, or disagree. Read these
panels as the public evidence, not as the settlement.
