# Prediction Terminal

A Bloomberg-style terminal for [Kalshi](https://kalshi.com) prediction markets,
live stock and crypto prices, equity and crypto **options with the Greeks**,
[FRED](https://fred.stlouisfed.org) economic data, the
[Billboard](https://www.billboard.com/charts/) charts, and the entertainment
feeds Kalshi settles against — driven entirely from a command prompt.

```
> GP KXFEDDECISION-27JAN-H26 1d 1y
> STK NVDA 1d 1y
> CRY BTC 1h 7d
> IMP BTC                     # Kalshi's implied BTC price, over the real one
> OPT AAPL 30d greeks         # option chain, delta/gamma/vega/theta/rho
> VOL BTC                     # volatility smile and term structure
> OI SPY                      # open interest by strike, and max pain
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

### Options

| Command | Usage | What it does |
| --- | --- | --- |
| `OPT` | `OPT <symbol> [expiry] [calls\|puts] [greeks] [strikes]` | The chain: both legs around the strike, with implied vol and the Greeks |
| `OPD` | `OPD <contract>` | One contract in full — Greeks, breakeven, and price history where a venue publishes it |
| `VOL` | `VOL <symbol> [expiry]` | Volatility smile across strikes, and the at-the-money term structure |
| `OI` | `OI <symbol> [expiry]` | Open interest and volume by strike, with max pain |

```
> OPT AAPL                 # front expiry, both legs
> OPT NVDA greeks          # delta/gamma/vega/theta instead of the quote columns
> OPT SPY 30d calls 20     # nearest expiry to 30 days, calls only, 20 strikes deep
> OPT BTC #3               # the third expiry on the strip
> OPD BTC-25DEC26-104000-C
> VOL SPY                  # is the front rich against the back?
> OI TSLA 30d
```

Arguments are order-independent after the symbol, like `GP` and `STK`. An expiry
can be an exact date (`2026-09-18`), a horizon (`30d`, `3m`, `1y` — the nearest
listed expiry to it), or a position on the strip (`#2`). Every panel carries the
same expiry chips, and each row of a chain is clickable: click the call side or
the put side and `OPD` opens that contract.

Both asset classes work the same way. `OPT AAPL` and `OPT BTC` differ only in
which venue answers.

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
                    ├── Yahoo      equities, ETFs and cash indices
                    │   └ Nasdaq   fallback, daily bars only
                    ├── Coinbase   crypto spot
                    ├── Deribit    crypto option boards (BTC, ETH, SOL, …)
                    ├── Yahoo v7   equity option chains
                    │   └ Nasdaq   fallback, paged
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

## Options and the Greeks

Two venues, one pipeline. Each source normalises into a single internal *board*
— every live contract, prices per unit of underlying — and the chain, the
volatility surface and the open-interest ladder are all derived from that board.
They are the same numbers seen three ways rather than three pipelines that can
disagree about what the delta of a contract is.

| | Crypto | Equities, ETFs, indices |
| --- | --- | --- |
| Venue | Deribit | OPRA, via Yahoo `v7` → Nasdaq |
| Underlyings | BTC, ETH, SOL, XRP, AVAX, HYPE, TRX | most US listed names with options |
| Forward | published per expiry | fitted from put-call parity |
| Implied vol | the venue's mark IV | solved from the book mid |
| Price history | yes | none published free |
| Contract size | 1 (10 on some linear boards) | 100 |

### Why Deribit for crypto

There was no real choice to make, which is worth stating plainly rather than
dressing up as a survey: **Deribit is where crypto options trade.** It has
consistently carried the large majority of global open interest in BTC and ETH
options, and its expiries are the ones desks quote off. A crypto options panel
sourced from anywhere else would be showing a shadow of the real board.

It also happens to satisfy the same constraints that picked Coinbase for spot:
no key, no account, no geo-fence, documented rate limits, and it answers from
datacentre IPs. The whole board arrives in two requests per currency —
`get_instruments` for the contract definitions and `get_book_summary_by_currency`
for every quote — rather than one request per contract, which for BTC alone
would be 818.

**Inverse contracts are quoted in the coin.** `BTC-25DEC26-104000-C` marks at
`0.0119` — that is 0.0119 *BTC*, about $757. Every price is multiplied by the
index once, at the source boundary, so nothing downstream has to remember which
product family it is holding. The panel says so rather than presenting the
conversion as native data. Linear USDC boards (SOL, XRP, AVAX, HYPE, TRX) are
already quoted in dollars.

### Why the model is Black-76

A terminal has no business asking its user for a risk-free rate and a dividend
yield. Both are unobservable, both are stale by the time you have typed them,
and a Greek computed from a guessed carry is a guessed Greek. So the model
prices options on the **forward**, and the forward is quoted:

* **Crypto.** Deribit publishes `underlying_price` per expiry. That *is* the
  forward its own marks are struck against.
* **Equities.** Put-call parity says `C - P = DF·(F - K)` — linear in the
  strike. Regressing the near-the-money call-put spread against the strike
  recovers the discount factor from the slope and the forward from the
  intercept, using quoted prices alone. No rate input, no dividend calendar.

Given `(spot, forward, discount factor)` the carry is fully determined —
`r = -ln(DF)/T` and `q = r - ln(F/S)/T` — so the classical Greeks follow without
anything having been assumed. The panel reports which of `venue`, `parity` or
`assumed` produced the forward, because a Greek is only as trustworthy as its
carry and the difference should never be invisible. On AAPL the parity fit
recovers a dividend yield within a few basis points of the real one, from option
quotes only.

Greeks are in the units a trader reads them in, which is also what Deribit
publishes, so the two are directly comparable:

| Greek | Units |
| --- | --- |
| delta | per 1 unit of underlying (spot delta, not forward delta) |
| gamma | delta per 1 unit of underlying |
| vega | per **1 volatility point** — a move from 40% to 41% |
| theta | per **calendar day** |
| rho | per **1 percentage point** of rate |

### A few things worth knowing

**Deribit's `interest_rate` field says zero, and its forward disagrees.** Every
instrument reports `interest_rate: 0.0` while the ten-month forward sits 3.9%
above the index — a 4.5% annualised rate. Believing the field left every
long-dated delta on the board disagreeing with Deribit's own by that amount. The
forward is the market's statement of the carry, so `DF = S/F` is what gets used,
and the deltas then reproduce Deribit's published values to five decimals.

**An annualised rate is not identifiable from a short-dated board.** The parity
regression's slope is a discount factor, so over a one-day expiry it is ~0.999
and a couple of cents of bid/ask noise annualises to a 30%+ rate. A *negative*
fitted rate is the same failure in the other direction — a discount factor above
1, which no USD expiry has — and it is not a small error: the discount factor
scales every price on the board, and a 1.6% scaling moves at-the-money implied
volatility by about two points. The fit is gated on the rate it implies; when
the gate fires, the forward is kept (it is the *intercept*, which stays well
determined) and only the discount is pinned to a configured rate.

**Only near-the-money strikes are eligible for the parity fit.** Listed equity
options are American and parity is a European identity, so a deep in-the-money
put can carry an early-exercise premium that parity reads as a distorted
forward. That premium is negligible around the money, which is also where the
books are tightest — one window fixes both problems.

**No vendor's implied volatility is adopted for equities.** Yahoo publishes one,
against its own undisclosed carry assumptions. Mixing that with a forward fitted
here would put two disagreeing volatilities on one screen and Greeks matching
neither. Every equity vol is solved from the book mid against the fitted
forward, so price, vol and Greeks are one consistent set.

**A one-sided option book is not halved.** A Kalshi contract offered at 1¢ with
no bid is worth somewhere in `[0, 1¢]`, because its payoff is bounded — that is
why `implied.ts` reads it as `ask / 2`. An option bid at 4.20 with no offer is
worth about 4.20. The bounded payoff is what makes the binary case different,
and options do not have one.

**The cumulative normal is the double-precision one, not the textbook one.**
Abramowitz & Stegun 7.1.26 is accurate to 1.5e-7 *absolute*, which sounds far
finer than any tick size and is not: in the wings `N(d1)` and `N(d2)` are
themselves of order 1e-8, so the price of a far out-of-the-money contract becomes
mostly approximation error. The wings are exactly where a smile is read.

**Max pain is a positioning read, not a forecast.** The panel says so on screen.
It reports the whole pain curve rather than only its minimum, because the shape
is what distinguishes a sharp pin from a flat basin.

**US listed options expire at 16:00 New York, not at UTC midnight.** Yahoo dates
an expiry at midnight UTC and Nasdaq gives no time at all. Hard-coding 20:00 UTC
would be an hour wrong for five months of the year, and an hour is not a
rounding error on an expiry-day chain — it is a third of a 0DTE contract's
remaining life, and theta and gamma both scale on it.

**Equity option price history is not available for free, and the panel says so.**
`OPD` on a crypto contract draws a candle chart from Deribit's own history,
converted from the coin bar-by-bar against the perpetual rather than against
today's index. `OPD` on an equity contract shows the live quote and states why
there is no chart, instead of rendering an empty one that reads as a failure.

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
| `DERIBIT_API_BASE` | `https://www.deribit.com/api/v2` | Override the crypto options upstream |
| `OPTIONS_RATE` | `0.04` | Rate used only when no forward can be observed or fitted |

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

The **option** chain hits this harder. Yahoo's `v7` hosts are blocked from
shared datacentre addresses more aggressively than `v8/chart` is, so a
deployment where `STK AAPL` works can still find `OPT AAPL` falling through to
Nasdaq. Nasdaq answers, but it paginates and publishes no implied volatility —
which costs nothing here, because the vol is solved from the book either way.

Crypto has no such issue: Coinbase and Deribit both answer from anywhere.

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
| `GET /api/options/underlyings` | Symbols with a known option board |
| `GET /api/options/:symbol/expiries` | The expiry strip, with contracts and open interest |
| `GET /api/options/:symbol/chain?expiry=` | Both legs at every strike, with implied vol and Greeks |
| `GET /api/options/:symbol/surface?expiry=` | Volatility smile, at-the-money term structure and 25Δ skew |
| `GET /api/options/:symbol/positioning?expiry=` | Open interest and volume by strike, with max pain |
| `GET /api/options/contract/:contract` | One contract, its pair leg, and price history where published |
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
| `GET /api/health` | Liveness, cache stats, whether a FRED key is set |

Errors are JSON: `{ error, code, hint? }`. The `hint` is written to be shown to
a person and is surfaced verbatim in the panel.

---

## Development

```bash
npm run dev          # server + client with reload
npm test             # 353 tests
npm run typecheck    # client and server
npm run check        # typecheck + test
```

Tests cover the parsers and normalisers rather than the network: the scraped
parsers run against fixtures captured from the real pages, the Kalshi, Yahoo,
Nasdaq and Coinbase normalisers against trimmed real API responses, and
`test/fred.integration.test.ts` exercises the whole FRED scrape path against a
local fixture server — which is how that path stays covered on networks where
the live host refuses to answer.

The option maths in `src/shared/greeks.ts` is checked three ways, because each
catches what the others cannot: against **arithmetic** (put-call parity,
intrinsic bounds), against **finite differences** through the economic variables
`(S, r, q, T, σ)` — which is what pins the units down, since a vega quoted per
unit rather than per point is off by exactly 100 and nothing else notices — and
against **Deribit's own published Greeks**, from a captured live response. An
exchange's risk system is the only external oracle available, and it is a good
one: it is what caught the `interest_rate: 0.0` problem above.

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

The Greeks are a model's opinion, not a measurement. They are Black-76 values
computed from a forward this terminal observes or fits, and the panel names
which — read an `assumed` forward with suspicion. Listed equity options are
American while the model is European, so deep in-the-money contracts carry an
early-exercise premium it does not price; that is why the parity fit only looks
near the money, and why the wings of an equity smile deserve more scepticism
than its middle.

The entertainment feeds show what a market is *likely* to settle against, not
what it *will*. Kalshi resolves against its own stated settlement sources under
its own rules, and a scraped page can lag, revise, or disagree. Read these
panels as the public evidence, not as the settlement.
