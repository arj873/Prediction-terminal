# Prediction Terminal

A Bloomberg-style terminal for [Kalshi](https://kalshi.com) prediction markets,
live stock and crypto prices, [FRED](https://fred.stlouisfed.org) economic data,
and the [Billboard](https://www.billboard.com/charts/) charts — driven entirely
from a command prompt.

```
> GP KXFEDDECISION-27JAN-H26 1d 1y
> STK NVDA 1d 1y
> CRY BTC 1h 7d
> IMP BTC                     # Kalshi's implied BTC price, over the real one
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
                    ├── Kalshi    trade-api v2  (JSON)
                    ├── Yahoo     equities, ETFs and cash indices
                    │   └ Nasdaq  fallback, daily bars only
                    ├── Coinbase  crypto spot
                    ├── FRED      scraped from fred.stlouisfed.org
                    └── Billboard scraped from billboard.com/charts
```

The server exists for three reasons the browser cannot handle alone: none of
the three upstreams allow cross-origin reads, two of them serve HTML that has
to be parsed somewhere, and N polling panels should make 1 upstream request.
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
that into a snapshot at boot (~4,000 events / ~35,000 markets in ~15s) so the
first `SRCH` does not pay for the crawl.

**Quiet candles are drawn flat, not skipped.** Kalshi returns a bucket with
only `previous_dollars` when nothing printed in the period. Those become flat
bars at the previous close, tagged `NO TRADE` in the crosshair legend, so a
chart stays continuous instead of gapping.

**The order book is inverted for you.** Kalshi quotes two *bid* ladders — YES
and NO — with no ask side, because an offer to sell YES at `p` is a bid to buy
NO at `1 - p`. The server converts the NO ladder into YES ask terms once, so
every panel sees a conventional bid/ask.

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
| `GET /api/health` | Liveness, cache stats, whether a FRED key is set |

Errors are JSON: `{ error, code, hint? }`. The `hint` is written to be shown to
a person and is surfaced verbatim in the panel.

---

## Development

```bash
npm run dev          # server + client with reload
npm test             # 170 tests
npm run typecheck    # client and server
npm run check        # typecheck + test
```

Tests cover the parsers and normalisers rather than the network: the Billboard
and FRED parsers run against fixtures captured from the real pages, the Kalshi,
Yahoo, Nasdaq and Coinbase normalisers against trimmed real API responses, and
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

---

## Scope

Read-only market data. Nothing here places an order, holds a credential, or
touches a Kalshi account — the terminal uses only public, unauthenticated
endpoints. The implied price is a reading of public quotes, not advice, and it
is only as good as the ladder underneath it — an expiry with a thin or one-sided
book will imply a number the panel reports the tail mass for precisely so you
can distrust it. Scraped sources are third-party sites whose markup can change
without notice; the parsers are written to degrade with a diagnosable error
rather than silently return wrong numbers.
