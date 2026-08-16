# Prediction Terminal

A Bloomberg-style terminal for [Kalshi](https://kalshi.com) prediction markets,
[FRED](https://fred.stlouisfed.org) economic data, and the
[Billboard](https://www.billboard.com/charts/) charts — driven entirely from a
command prompt.

```
> GP KXFEDDECISION-27JAN-H26 1d 1y
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
                    ├── Kalshi   trade-api v2  (JSON)
                    ├── FRED     scraped from fred.stlouisfed.org
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
| `GET /api/health` | Liveness, cache stats, whether a FRED key is set |

Errors are JSON: `{ error, code, hint? }`. The `hint` is written to be shown to
a person and is surfaced verbatim in the panel.

---

## Development

```bash
npm run dev          # server + client with reload
npm test             # 95 tests
npm run typecheck    # client and server
npm run check        # typecheck + test
```

Tests cover the parsers and normalisers rather than the network: the Billboard
and FRED parsers run against fixtures captured from the real pages, the Kalshi
normalisers against trimmed real API responses, and `test/fred.integration.test.ts`
exercises the whole FRED scrape path against a local fixture server — which is
how that path stays covered on networks where the live host refuses to answer.

---

## Scope

Read-only market data. Nothing here places an order, holds a credential, or
touches a Kalshi account — the terminal uses only public, unauthenticated
endpoints. Scraped sources are third-party sites whose markup can change
without notice; the parsers are written to degrade with a diagnosable error
rather than silently return wrong numbers.
