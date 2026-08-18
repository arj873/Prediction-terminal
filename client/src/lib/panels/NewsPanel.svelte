<!--
  NEWS — the market news wire.

  Reads like the trade tape rather than like a chart: newest at the top, one
  line per story, the tagged symbols beside it. The headline links out to the
  publisher, and the rest of the row drills into the price of what the story is
  about, because "what is this doing to the stock" is the next question every
  time.

  Every count of headlines is a count *within a stated window*. The panel says
  how far back it reached rather than implying it is everything.
-->
<script lang="ts">
  import type { NewsArticle, NewsFeed } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import { chartCommand, wireTime } from './news';
  import type { Column, EmptyState, NoteSegment } from './table';
  import { news } from '../api/client';
  import { EM_DASH, truncate } from '../format';

  interface Props {
    id: string;
    /** Symbols to filter to. Empty means the whole wire. */
    symbols: string[];
    limit: number;
    days: number;
  }

  const { id, symbols, limit, days }: Props = $props();

  const data = createPanelData({
    // A wire is the one feed here that is worth watching in real time.
    load: (signal) => news.feed(symbols, limit, days, signal),
    refreshMs: 60_000,
  });

  const title = $derived(symbols.length > 0 ? truncate(symbols.join(' '), 30) : 'WIRE');
  const subtitle = $derived(
    data.data
      ? `${data.data.articles.length} headlines · ${data.data.days}d · ${data.data.source}`
      : '',
  );

  function note(feed: NewsFeed): NoteSegment[] {
    return [
      {
        text:
          feed.symbols.length > 0
            ? `${feed.articles.length} headlines on ${feed.symbols.join(', ')}`
            : `${feed.articles.length} headlines across the wire`,
      },
      { text: `  last ${feed.days}d`, tone: 'dim' },
      // Say whose wire this is rather than passing it off as first-party.
      { text: `  · via ${feed.source}`, tone: 'dim' },
    ];
  }

  function empty(feed: NewsFeed): EmptyState {
    // An empty wire is a fact about a stated window, so the window is stated
    // and the command that widens it is spelled out.
    const scope = feed.symbols.length > 0 ? feed.symbols.join(' ') : 'the wire';
    const reach = feed.days === 1 ? 'day' : `${feed.days} days`;
    const wider = `NEWS ${[...feed.symbols, '30d'].join(' ')}`;

    return {
      message: `Nothing on ${scope} in the last ${reach}.`,
      hint:
        feed.symbols.length > 0
          ? `Try a wider window — \`${wider}\` — or drop the symbol filter with \`NEWS\`.`
          : `Try a wider window — \`${wider}\`.`,
    };
  }

  /** The story's own subject: the first ticker the publisher tagged it with. */
  function rowCommand(article: NewsArticle): string | null {
    const primary = article.symbols[0];
    return primary ? chartCommand(primary) : null;
  }

  function rowTitle(article: NewsArticle): string | undefined {
    const command = rowCommand(article);
    return command ? `${article.headline}\nClick for ${command}` : undefined;
  }

  const columns: Column<NewsArticle>[] = [
    { header: 'TIME', cell: (a) => wireTime(a.time), class: 'num dim' },
    {
      header: 'HEADLINE',
      render: headline,
      title: (a) => (a.summary ? `${a.headline}\n\n${a.summary}` : a.headline),
    },
    {
      header: 'SYMBOLS',
      cell: (a) =>
        a.symbols.length === 0 ? EM_DASH : truncate(a.symbols.slice(0, 4).join(' '), 24),
      class: (a) => (a.symbols.length === 0 ? 'mono dim' : 'mono'),
      // The full tag list, since the cell shows at most four of them.
      title: (a) => (a.symbols.length > 0 ? a.symbols.join(' ') : undefined),
    },
    { header: 'SRC', cell: (a) => a.publisher.toUpperCase() || EM_DASH, class: 'dim tiny' },
  ];
</script>

<!--
  The article text arrives already plain: the server strips the markup and
  decodes the entities, so this is ordinary interpolation and never `{@html}`.
-->
{#snippet headline(article: NewsArticle)}
  <div class="news-headline">
    {#if article.url}
      <!--
        The row drills into the price; the headline opens the story. One click
        must not do both.
      -->
      <a
        class="news-link"
        href={article.url}
        target="_blank"
        rel="noopener noreferrer"
        onclick={(event) => event.stopPropagation()}>{article.headline}</a
      >
    {:else}
      {article.headline}
    {/if}
  </div>
  {#if article.summary}
    <div class="news-summary">{truncate(article.summary, 150)}</div>
  {/if}
{/snippet}

<TablePanel
  {id}
  kind="NEWS"
  {title}
  {subtitle}
  {data}
  rows={(feed: NewsFeed) => feed.articles}
  {columns}
  {note}
  {empty}
  {rowCommand}
  {rowTitle}
  tableClass="news-table"
/>
