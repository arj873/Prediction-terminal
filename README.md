# Prediction Terminal

A Bloomberg-style terminal for six prediction markets — [Kalshi](https://kalshi.com),
[Polymarket](https://polymarket.com), [Polymarket US](https://polymarket.us),
[Gemini](https://www.gemini.com/prediction-markets),
[predict.fun](https://predict.fun) and [ForecastEx](https://forecastex.com) —
plus live stock and crypto prices, the [Alpaca](https://alpaca.markets) news
wire, [FRED](https://fred.stlouisfed.org) economic data, the
[Billboard](https://www.billboard.com/charts/) charts, and the entertainment
feeds these markets settle against, driven entirely from a command prompt.

```
> GP KXFEDDECISION-27JAN-H26 1d 1y
> DES pm:will-there-be-no-change-in-fed-interest-rates-after-the-september-2026-meeting-615
> XV KXFEDDECISION-26OCT      # the same ladder, priced at every broker listing it
> XV fed                      # what else more than one broker lists
> STK NVDA 1d 1y
> IMP BTC                     # Kalshi's implied BTC price, over the real one
> NEWS NVDA
> FRED UNRATE
> BB hot-100
```

Type a command, get a panel. Panels tile into a grid, poll on their own timers,
and every table row is clickable so the mouse and the keyboard drive the same
code path. It is meant to be driven without the mouse at all: see
[Keys](#keys).

---

## Quick start

The terminal is two programs: a Rust API server and a SvelteKit client. You
need a Rust toolchain (1.85 or newer) and Node 20.11 or newer.

```bash
make install         # client dependencies
make dev             # API on :8787, client on :5173 with proxying
```

Open http://localhost:5173 and type `HELP`.

For a single-process production build:

```bash
make build
make start           # serves the built client and the API on :8787
```

`make` on its own lists every target.

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
| `SRCH` | `SRCH <words> [kalshi\|pm\|pmus\|gemini\|pf\|fex]` | Search open events at every venue at once, grouped with their strike ladders |
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
| `gem:` | Gemini | ticker, `GEMI-FED260917-MAINTAIN` |
| `pf:` | predict.fun | slug, `big-game-champion-2027~26952` |
| `fx:` | ForecastEx | contract id, `HORC_1126_Republican` |

```
> DES KXFEDDECISION-26OCT-T3.75
> OB pmus:apdc-jerpowgov-2026-12-31
> GP pm:will-there-be-no-change-in-fed-interest-rates-after-the-september-2026-meeting-615 1h 7d
> DES gem:GEMI-FED260917-MAINTAIN
> TAS fx:HORC_1126_Republican
> W ADD pm:fed-decision-in-october-20260617190323537
```

Case is part of the identifier, not decoration: Kalshi 404s a lower-case ticker
and both Polymarkets 404 an upper-case slug, so the prefix decides the folding
and you can type either. `pm`, `poly` and `intl` all mean the international
book; `pmus`, `polyus` and `us` all mean the US one; `gemini`, `pf` and `fex`
name the three newer ones.

Two of those need more than folding. ForecastEx publishes mixed-case ids
(`HORC_1126_Republican`) and its price endpoint answers only the exact spelling
— an upper-cased id comes back as a silent, empty, HTTP 200 — so the terminal
folds ids up like everything else and restores the canonical spelling from the
catalogue before it charts. predict.fun names a contract with two identifiers, a
slug for the question and a number for the leg, so a reference joins them with
`~`: `big-game-champion-2027~26952`. Typing the slug alone works when the
question has only one leg, and otherwise tells you to open `EVT`.

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
| `TV` | `TV [YYYY-MM-DD] [country]` |
| `AWRD` | `AWRD <award> [year]` |
| `TRND` | `TRND [country]` |
| `REL` | `REL <artist> [album\|song]` |
| `POD` | `POD [top\|episodes] [country]` | What airs that day, by network |

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

Six exchanges list many of the same questions and agree on no identifier for
any of them:

| | Kalshi | Polymarket | Polymarket US | Gemini | predict.fun | ForecastEx |
| --- | --- | --- | --- | --- | --- | --- |
| Series | `KXFEDDECISION` | `fomc` | `usfed-fomc` | `FED` | `fed-decision-in` | `FFDEC` |
| Event | `KXFEDDECISION-26OCT` | `fed-decision-in-october-2026…` | `usfed-fomc-2026-10-28` | `FED260917` | `fed-decision-in-september-762` | `FFDEC_091626` |
| Rung | `Cut 25bps` | `25 bps decrease` | `25 bps Decrease` | `Fed maintains rate` | `No change` | `Will the Fed leave the rate unchanged in September 2026?` |

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

The matcher itself lives in `crates/core/src/matching.rs` and touches no network, so its
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

**The board is built once per catalogue refresh, not once per request.** Reading
five thousand series against each other is the largest piece of arithmetic in
the terminal, and it depends on nothing a request supplies — `XV fed` and a row
limit only narrow a board that already exists. So the pairing runs with the
index it is derived from, on a blocking thread rather than on the async runtime,
and every request is a scan of the finished list. Two things make that fast
enough to do at all: each listing's half of a comparison — its terms, its
numbers, its flattened identifier — is derived once and reused against every
candidate, and a pair whose token overlap makes the match floor arithmetically
unreachable is declined before any of the rest runs. The second is exact rather
than approximate: only the identifier boost and the number reward can raise a
score, both are bounded, and the caller discards anything below the floor
anyway, so declining early turns away precisely the pairs a full scoring pass
would have thrown out. On the live catalogues that is a board in well under a
second where it was six and a half minutes, with every score unchanged —
`crates/core/examples/matchbench.rs` is the harness that proves both halves of
that sentence.

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
| `KXOSCAR*`, `KXEMMY*`, `KXGRAMMY*`, `KXGOTY` | the ceremony itself | `AWRD` |
| `KXGOOGLESEARCH*`, `KXRANKLISTGOOGLESEARCH` | trends.google.com | `TRND` |
| `KXALBUMRELEASE*`, `KXSONGRELEASE*`, `KXNEWTAYLOR` | when a record ships | `REL` |
| `KXTOPPOD`, `KXROGANGUEST`, `KXCALLHERDADDY*` | Apple's podcast chart | `POD` |

Four of those exist because the market's own settlement source will not answer a
datacentre IP, and the terminal had nothing to show against them:

**`AWRD` reads Wikidata, not oscars.org.** Award markets were the largest hole in
the table — oscars.org and its awards database both return 403 from a container,
the way fred.stlouisfed.org resets one. Wikidata holds the same facts as
statements (`P166` award received, `P1411` nominated for, `P585` which ceremony)
and answers. Two properties of that source change how the panel reads, and it
says both rather than implying otherwise: **winners are recorded within minutes
and losing slates fill in over days**, so the panel reports the nominee count it
actually got; and **a ceremony that has not happened is empty**, which is the
state an open market exists to price rather than a failure to find anything.

**`TRND` is the daily list, not the annual one.** The markets naming
trends.google.com settle on Year in Search, published once in December. What the
feed shows is who is being searched today — the evidence a trader has in August
for a market resolving then, the same relationship `BO` has to a total-gross
market. The panel captions itself that way.

**`REL` reads Apple, not Spotify.** Spotify's API needs an OAuth client, which
would be the first credential in this codebase; Apple's Search API needs nothing,
covers the same catalogue, and lists pre-orders with their announced date — which
is the most direct evidence a "will they release by X" market has. Results are
held to the artist named, because `attribute=artistTerm` narrows the search and
does not close it: a live query for one artist returns tribute and covers acts,
and because those release constantly they would head a newest-first list.

**`POD` reads Apple's own chart.** Two views, because they answer different
questions: `top` ranks shows, which is what a "most popular podcast" market is
about, and `episodes` is what is charting now, which is where a guest booking
shows up. Apple spells its own advisory rating `Explict`, so the reading matches
on a prefix — an equality test against `explicit` reports an all-clean chart,
which is wrong without ever looking broken.

### Workspace

| Command | Usage |
| --- | --- |
| `W` | `W` · `W ADD <ticker>` · `W DEL <ticker>` · `W CLEAR` |
| `LAY` | `LAY <1-4>` · `LAY +` · `LAY -` — panel columns |
| `THEME` | `THEME amber\|green\|ice` |
| `CLS` | `CLS` · `CLS ALL` |
| `ZOOM` | Give the focused panel the whole workspace, or restore the grid |
| `FOCUS` | `FOCUS <NEXT\|PREV\|1-9\|LAST\|NAV\|CMD>` |
| `ROW` | `ROW <NEXT\|PREV\|TOP\|END\|OPEN\|ALT>` — the row cursor inside a panel |
| `REFRESH` | `REFRESH` · `REFRESH THIS` |
| `CLR` | Clear the message log |
| `KEYS` | `KEYS` · `KEYS <chord> <command>` · `KEYS DEL <chord>` · `KEYS RESET` |
| `HELP` | `HELP [command]` |

### The menu bar

Memorising a command table and a key map is the cost of admission to a terminal
like this one, and there was nothing between "type `HELP` and read 39 verbs" and
knowing them already. So: five menus across the top — MARKETS, PRICES, DATA,
WORKSPACE, LEARN — with everything the terminal does under one of them.

```
MARKETS   PRICES   DATA   WORKSPACE   LEARN            Alt+M opens these menus

  Search every venue at once…                SRCH …          Alt+S
  What more than one broker lists            XV              Alt+X
  Leaderboard                                TOP             Alt+T · NAV T
  Leaderboard by…                                                  ▸
  THE MARKET UNDER THE ROW CURSOR — this is what $ means
  Chart it                                   GP $            NAV C
```

**It is a teaching surface, not a mouse convenience.** Every entry carries three
columns: what it does, the command line it runs, and the key that runs it.
Choosing one echoes the resolved command into the message log in the shape a
typed line takes, pushes it onto the history where `↑` will find it, and says
which key would have done the same. A week of using the menus should end the
need for them.

**Nothing here is a second dispatch path.** An entry hands its command line to
the same `run()` a typed line goes to, `$` is filled the way a key binding fills
it, and the chord column is read out of the live key map — so `KEYS alt+b OB $`
relabels the menu with no code aware that it might. The columns are honest in
the other direction too: `NAV` marks a key that only fires in NAV mode, an entry
that would answer a bare verb with a usage error types it at the prompt instead
(read off the usage line, not decided by hand), and an entry whose `$` cannot be
filled greys out with the reason underneath.

`MENU` drives the bar the way `FOCUS` drives the panels — `MENU`, `MENU markets`,
`MENU NEXT|PREV|CLOSE` — with `Alt+M` and `F10` bound globally and `m` in NAV
mode, so the menus are reachable by typing, by key and by mouse without any of
the three being a special case.

Only the curated entries are hand-written. LEARN's "Every command" is generated
from the command table, so a verb added to the table is in the menus the same
day — and `menu.test.ts` fails if it somehow is not, alongside checks that no
entry names a command the terminal lacks and none runs a bare verb that needs an
argument.

### Keys

Run `KEYS` for the live map — like `HELP`, it is generated from the bindings
themselves. The terminal is built to be driven without the mouse: every panel
opens from a key, every row can be reached with a cursor, and every binding can
be replaced.

**At the prompt.** These fire anywhere, including mid-word, so they never
interrupt what you are typing.

| Key | Action |
| --- | --- |
| `Enter` | Run · `↑`/`↓` walks history · `Tab` completes the verb |
| `Esc` | Clear the line — again on an empty line hands the keyboard to the panels |
| `Alt`+`1`…`9` | Focus that panel — the number is in its header |
| `Ctrl`+`←`/`→` | Move focus between panels |
| `Alt`+`↑`/`↓` | Move the row cursor inside the focused panel |
| `Alt`+`Enter` | Open the row under the cursor · `Alt`+`Backspace` runs its second action |
| `Alt`+`F` | Maximise the focused panel · `Alt`+`Q` closes it · `Alt`+`R` reloads them all |
| `Alt`+`W` `T` `N` `X` `K` `H` | Watchlist · leaderboard · news · cross-venue · key map · help |
| `Alt`+`G` `S` `D` | Start a `GP`, `SRCH` or `DES` command without running it |
| `Ctrl`+`L` | Clear the message log |

**NAV mode.** `Esc` on an empty line (or `Alt`+`J`) hands the keyboard to the
workspace — the header says `NAV` while it holds it. Plain letters are bindings
there, so the vim keys are free:

| Key | Action |
| --- | --- |
| `j` / `k` | Row cursor down / up — or scroll, in a panel with no rows |
| `g` / `G` | First / last row · `Enter` or `o` opens it · `d` runs its second action |
| `h` / `l` | Previous / next panel · `1`…`9` jumps to one · `Tab` cycles |
| `c` `b` `q` `s` `v` `a` | Chart · book · quote · tape · cross-venue · add to watchlist, for the row under the cursor |
| `f` `x` `r` | Maximise · close · reload this panel · `[` and `]` change the column count |
| `w` `t` `n` `?` `H` | Watchlist · leaderboard · news · key map · help |
| `/` or `Esc` | Back to the command line |

An unbound letter in NAV mode is not swallowed: it takes you to the prompt with
the character intact, so `SRCH` still starts by typing `S`.

### Binding your own

```
> KEYS                       # the whole map, presets and edits together
> KEYS alt+b OB $            # bind a chord
> KEYS "g w" W               # a sequence — two chords, in order
> KEYS alt+e EVT --panel     # force the scope the chord would not have chosen
> KEYS alt+b                 # what is this bound to?
> KEYS DEL alt+b             # unbind, presets included
> KEYS RESET                 # forget every edit
```

Two rules decide where a binding lives. A chord carrying `Ctrl`, `Alt` or `Meta`
— or a key that types nothing, like `F4` — is **global** and fires even while
you are typing. Anything else is **panel**-scoped and fires only in NAV mode,
because a global `j` would eat every `j` you ever type. `--global` and `--panel`
override the guess, and a global binding that would swallow typing is refused.

Two characters mean something inside a bound command:

* **`$`** is the market under the row cursor, or failing that whatever the
  focused panel is about. It is what makes one binding useful everywhere: `OB $`
  is "the order book for the thing I am looking at", whether that is a
  leaderboard row, a chart, or a quote.
* **`>`** types the command at the prompt instead of running it, for the verbs
  that still need an argument — `>GP` leaves you at `GP ` with the cursor after
  it.

Edits are stored as a sparse overlay on the presets rather than a copy of them,
so a preset added in a later version still reaches you, and switching one off
stays off across a reload.

---

## How it works

```
browser (SvelteKit, static, no server rendering)
  └── /api/*  ──► Rust API server (axum)
                    ├── Kalshi     trade-api v2  (JSON)
                    ├── Polymarket gamma-api  catalogue
                    │   ├ clob       book and price history
                    │   └ data-api   public print tape
                    ├── PolymarketUS gateway  catalogue, book, BBO
                    ├── Gemini     gemini.com  catalogue
                    │   └ api.gemini.com  book, tape, candles
                    ├── predict.fun graphql  catalogue, book, tape, series
                    ├── ForecastEx forecastex.com/api  catalogue, prices
                    │   └ public S3  end-of-session tape and daily bars
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
every open chart with no per-panel bookkeeping. A chart instance outlives its
data: a poll calls `setData` rather than rebuilding, so a view you have zoomed
into stays where you put it.

There is one dispatch path in the client, and everything funnels into it. A
typed line, a clicked row and a key press all end up in the same `run()`: a
binding is a chord and a command string, and a row registers the command it runs
so the keyboard can read it back. That is what keeps the three in step — a
shortcut can only do something you could also have typed, `KEYS` can only rebind
what the router already fires, and a row that gains a click handler gains a
keyboard cursor the same day.

Panels are descriptions rather than objects — an id, a kind and its arguments —
rendered through a keyed list. Re-running a command for a panel already on
screen is therefore a change of arguments, not a teardown, which is what makes
`GP X` safe to mash.

### A few things worth knowing

**Prices are dollars, displayed as cents.** A contract settles at $1, so `52¢`
is both the price and a 52% implied probability. Each upstream has its own
dialect — Kalshi speaks fixed-point decimal *strings* (`"0.5200"`,
`"12645.98"`), Polymarket wraps prices in `{value, currency}` objects and ships
JSON arrays as JSON *strings*, Gemini quotes dollar strings and states its 24h
move as a *percentage of the older price*, predict.fun ships trade sizes as
1e18-scaled integer strings, and ForecastEx wraps every response in a JSON
*string* inside a JSON object — and none of that leaks past the server boundary.
A normalised market from any of the six is the same shape.

**Almost no venue quotes an ask.** Kalshi publishes two *bid* ladders;
Polymarket, Gemini and predict.fun each run one book per contract and quote only
its YES side. All of them are converted into a conventional YES bid/ask with a
derived NO ladder, once, on the server — so any two books can be read side by
side without remembering which inversion applies to which. predict.fun's
complement is its own: it rounds to the *market's* decimal precision, which
differs from its category's on 326 of 963 live legs, and using the wrong one
moves the NO side by a tick.

**A blank is not a zero.** Polymarket US's public catalogue carries no volume,
no open interest and no resting depth at all, and Gemini states turnover per
event rather than per contract, so those fields are `null` rather than `0` and
render as `--`. The same rule decides who appears on a leaderboard:
a market whose venue does not publish the sorted figure is left off that board
entirely, and the panel says which venue and why, because ranking it as zero
would be a statement about its API dressed up as one about its book.

**Polymarket US charts and tapes need a key, so there are none.** Its price
history and prints live behind `api.polymarket.us`, which requires an account
and an API key; the public gateway serves catalogue, book and BBO. The terminal
holds no credentials, so `GP` and `TAS` on a `pmus:` market name the endpoint
that would answer and what it costs, instead of drawing an empty panel.

**predict.fun's documented API is not the one it uses.** `api.predict.fun` needs
an account key on every path, including its own "get the message to sign" step.
Its web app never calls it: the data is on a public GraphQL host that needs no
credential at all, which is what the terminal reads. That host answers a wrong
`Origin` header with a hard `403`, so the server sends none. GraphQL also reports
failure as an HTTP `200` with an `errors` array, so the client inspects the body
rather than the status, and treats a `null` entity with no error as the
`not_found` it is.

**ForecastEx has no order book, and that is how the exchange works.** It matches
by pairing a YES buyer with a NO buyer, so a print is the whole of what exists —
there is no bid, no ask and no ladder anywhere in its public API, and live quotes
sit behind IBKR ForecastTrader's login. `OB` on an `fx:` contract says that
rather than synthesising a one-level ladder out of the last two prints. Its
turnover, its daily move and its bars come from the exchange's end-of-session CSV
archive, published after the 16:15 CT roll, so those figures are a session behind
the tape and the panel says so.

**A wrong sort key can hide a tenth of a catalogue.** Crawling ForecastEx by open
interest returned 11,750 rows and only 10,270 distinct contracts: ties are not
broken, so the offset window shifts under the crawl and 1,480 contracts are
served twice while as many are never served at all. Ordering by `contract_id`
returns each one exactly once. Gemini has the same shape of problem twice over —
identical tape requests answer 278 or 349 prints depending on which replica
replies, so its turnover is read off its candles instead; and its catalogue is a
single request with a `limit`, which the universe outgrows, so the crawl reads
the pagination footer and asks again rather than quietly serving a board with
holes in it.

**Three venues publish prices, not candles.** Polymarket's history is a sample
series — a price at a moment, with no size attached — and so are predict.fun's
and ForecastEx's. The server buckets those onto the same 1/60/1440-minute grid
Kalshi uses, so every venue's bars land on one x-axis, leaves volume `null`
rather than inventing a zero, and says so in the panel header. Two quirks of
Polymarket's upstream are worked around rather than papered over: a
`startTs`/`endTs` window wider than exactly 15 days returns an empty history with
a `200`, so wide windows are stitched from chunks or fall back to a named
lookback; and its offset pagination refuses to go past ~2,300 events, which is
why that crawl is ordered by 24h volume — the truncation then drops the dead tail
rather than a random slice. Gemini is the exception among the six: `/v2/klines`
serves true OHLC off the matching engine, so its chart carries no disclaimer.

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
| `TRUST_PROXY` | off | Let `X-Forwarded-For` name the client. **Only behind a proxy that rewrites it** — otherwise a caller picks its own rate-limit bucket |
| `KALSHI_API_BASE` | Kalshi v2 | Override the upstream base URL |
| `POLYMARKET_GAMMA_BASE` | `https://gamma-api.polymarket.com` | Polymarket catalogue |
| `POLYMARKET_CLOB_BASE` | `https://clob.polymarket.com` | Polymarket book and price history |
| `POLYMARKET_DATA_BASE` | `https://data-api.polymarket.com` | Polymarket print tape |
| `POLYMARKET_US_API_BASE` | `https://gateway.polymarket.us` | Polymarket US public gateway |
| `GEMINI_CATALOGUE_BASE` | `https://www.gemini.com` | Gemini prediction-market catalogue |
| `GEMINI_API_BASE` | `https://api.gemini.com` | Gemini book, tape and candles |
| `PREDICTFUN_GRAPHQL_BASE` | `https://graphql.predict.fun/graphql` | predict.fun public GraphQL host |
| `FORECASTEX_API_BASE` | `https://forecastex.com` | ForecastEx catalogue, products and prices |
| `FORECASTEX_ARCHIVE_BASE` | `https://forecastex-public-data.s3.amazonaws.com` | ForecastEx end-of-session archive |
| `FRED_WEB_BASE` | `https://fred.stlouisfed.org` | Override for testing against a fixture |
| `YAHOO_API_BASE` | Yahoo chart API | Override the equity/index upstream |
| `NASDAQ_API_BASE` | `https://api.nasdaq.com` | Override the equity fallback |
| `COINBASE_API_BASE` | `https://api.exchange.coinbase.com` | Override the crypto upstream |
| `ALPACA_DATA_BASE` | `https://data.alpaca.markets/v1beta1` | Override the news upstream |
| `BILLBOARD_BASE` | `https://www.billboard.com` | Override the charts upstream |
| `BOXOFFICE_BASE` | `https://www.boxofficemojo.com` | Override the box-office upstream |
| `NETFLIX_BASE` | `https://www.netflix.com` | Override the Top 10 upstream |
| `ROTTENTOMATOES_BASE` | `https://www.rottentomatoes.com` | Override the scores upstream |
| `KWORB_BASE` | `https://kworb.net` | Override the Spotify/YouTube chart mirror |
| `STEAMCHARTS_BASE` | `https://steamcharts.com` | Override the concurrents leaderboard |
| `STEAM_API_BASE` | `https://api.steampowered.com` | Override the live player-count API |
| `STEAM_STORE_BASE` | `https://store.steampowered.com` | Override the Steam store search |
| `TVMAZE_API_BASE` | `https://api.tvmaze.com` | Override the TV schedule upstream |
| `CLIENT_DIR` | — | Where the built client lives. Unset in development |
| `RUST_LOG` | `info` | Log filter, in `tracing-subscriber` syntax |

Every upstream base is overridable, which is what lets each scraper be tested
against a fixture server rather than the live internet. Seven of them were
hardcoded before and so could only ever be exercised against the real site.

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
make dev
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
| `GET /api/venue/:venue/markets/:id` | Normalised market at any venue (`kalshi`, `polymarket`, `polymarket-us`, `gemini`, `predictfun`, `forecastex`) |
| `GET /api/venue/:venue/markets/:id/orderbook?depth=` | Book with the NO ladder derived (501 at ForecastEx, which has no book) |
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
make dev             # server + client, both reloading
make test            # the whole suite, Rust and client
make check           # what CI runs: fmt, clippy, tests, type-gen drift, build
```

### How it is laid out

```
crates/core/     terminal-core: the wire contract, the implied-price maths,
                 the cross-venue matcher, the venue registry. No I/O.
server/          the axum API. sources/ speak to upstreams and normalise;
                 routes/ put HTTP in front of them and know no upstream.
client/          the SvelteKit terminal.
contract/        parity fixtures shared by both halves of the venue registry.
```

The client's TypeScript types are **generated** from the Rust structs with
`ts-rs`, so the two cannot disagree about the wire: `make gen-types` rewrites
`client/src/lib/api/gen/`, and CI regenerates and diffs to prove nobody edited
one side without the other. That matters most for the distinctions the contract
used to state in prose and hope both sides remembered — that a `null` volume
means the venue does not publish the figure and must never render as a zero,
and that a candle's timestamp is the period *end* for a prediction market but
the period *start* for spot.

The venue registry is the one thing deliberately written twice, because the
client needs `parseRef` synchronously at the prompt and codegen cannot emit
logic. `contract/venue-cases.json` pins the two implementations against each
other, and both suites read it.

### What the tests cover

Everything runs offline. Parsers and normalisers are tested rather than the
network: the scraped parsers against fixtures captured from the real pages, the
Kalshi, Yahoo, Nasdaq and Coinbase normalisers against trimmed real API
responses, and every upstream base URL is overridable so a source can be pointed
at a `wiremock` fixture instead of the internet. `server/tests/fred_integration.rs`
exercises the whole FRED scrape path that way — which is how it stays covered on
networks where the live host refuses to answer — and `news_integration.rs` does
the same for the wire nothing in CI has a key for: it asserts the key pair goes
in the headers rather than the query string, that the request overrides the two
upstream defaults that would otherwise cost headlines, and that a missing key
and a rejected key are different, actionable errors.

The implied-price maths in `crates/core/src/implied.rs` is the most heavily
tested part of the codebase, because it is the one piece whose output looks
plausible when it is wrong: a mishandled ladder shape still returns a number
near the money. Each stage is checked against arithmetic, including that an "or
above" ladder and the equivalent range ladder price to the same level, and the
historical assembly is covered separately — a rung that stops printing carries
forward, and no rung is ever priced with a candle it did not yet have.

The cross-venue matcher was ported by differential testing rather than by
reading: both implementations were run over the same corpus and their scores,
confidence bands, shared terms and reason strings diffed to fifteen decimal
places. The same harness is how it is kept honest through changes meant only to
make it faster — `crates/core/examples/matchbench.rs` dumps every score, band,
shared-term list and reason for a corpus and the two runs are diffed. The corpus
is real: `crates/core/tests/fixtures/series.json` is six hundred titles taken
from the live catalogues by the server's `dump_descriptors` example, sampled
towards the families that are hard — House districts several venues word
almost identically, Emmy categories that differ by one qualifier, rate ladders
that differ by one number. `crates/core/tests/matching_corpus.rs` asserts over
all of them that the prefilter never declines a pair that could have cleared the
floor, which is the property the whole board's speed rests on.

The entertainment tests lean on the cases where a plausible-looking parser reads
the wrong number without ever failing: the two header collisions above, a film
with no Tomatometer, a chart with no movement column, and an upstream that
answers `200 OK` with "no data available" instead of an error.

The cross-venue matcher is covered for the same reason: pairing
two brokers' listings incorrectly still produces a tidy table of two prices, and
a reader will take the gap between them for an edge. So the cases are the near
misses — `2nd place` against `3rd place`, October's Fed meeting against
January's, `NFL Champion` against `NFL Rookie of the Year` — alongside the real
five-rung FOMC ladder, which must line up across every venue listing it without
ever mapping two rungs onto one. That last case is what caught the wording gap
the newest venues opened: ForecastEx states each rung as a whole question, and
once the clause its siblings share is stripped the middle one reads "leave the
rate unchanged" against Kalshi's "Fed maintains rate". Both fold to `nochange` —
but only after the four-word clause is folded whole, because the bare
`unchanged` rule left `leave` and `rate` behind and dropped the pair to 0.22
against a floor of 0.34. The rung that failed was the one carrying most of the
ladder's volume.

The three newest venues are tested for the traps their APIs set rather than for
their happy paths, because each answers `200 OK` while being wrong. The Gemini
tests cover the strikes whose structured value is mangled label text
(`{"type":"above","value":"KE25"}` on a rung labelled "Hike 25bps"), the
contracts that carry no `bestBid`/`bestAsk` key at all, the turnover stated per
event that must not be copied onto its twelve legs, and the catalogue limit the
universe outgrows. The predict.fun tests cover the 1e18-scaled trade amounts, the
prints priced in the *named* outcome's terms rather than in YES, and the slug
rule that has to fold September's and October's Fed books into one series without
fusing all 38 Texas districts. The ForecastEx tests cover the archive rows where
an untraded day reports `0.00` as its high, low and VWAP, the blank cells that
must stay `null` rather than settle a contract at zero, and the case folding that
decides whether a price call answers at all — that exchange's price endpoint is
case-sensitive and reports a wrong case as an empty `200`, so what a test can
check is that the canonical spelling is restored before the call.

The Polymarket US tests pin down the field the normaliser deliberately does
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
any of the six exchanges — every market, price and entertainment feed is a
public, unauthenticated endpoint. That is a deliberate limit, and it is visible:
Polymarket US serves its price history and prints only to an authenticated
caller, and predict.fun's documented REST API gates even its read paths behind an
account key, so those panels name the endpoint that would answer and what it
costs rather than appearing broken. Where a venue's own web app reads a public
host instead — as predict.fun's does — the terminal reads that host too.

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
