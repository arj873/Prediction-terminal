/**
 * Which component renders which kind of panel.
 *
 * The command registry decides *what* to open — a kind and its props — and this
 * decides what that looks like. Keeping the two apart is what lets a command
 * handler be a one-liner and a panel be a component with no knowledge of the
 * command that opened it.
 *
 * The map is total over `PanelKind`, so adding a kind without a component is a
 * compile error rather than an empty tile.
 */

import type { Component } from 'svelte';

import BillboardChartsPanel from './BillboardChartsPanel.svelte';
import BillboardPanel from './BillboardPanel.svelte';
import BoxOfficePanel from './BoxOfficePanel.svelte';
import ChartPanel from './ChartPanel.svelte';
import ComparePanel from './ComparePanel.svelte';
import DepthPanel from './DepthPanel.svelte';
import EntPanel from './EntPanel.svelte';
import EventPanel from './EventPanel.svelte';
import DataSeriesPanel from './DataSeriesPanel.svelte';
import DataSearchPanel from './DataSearchPanel.svelte';
import SourcesPanel from './SourcesPanel.svelte';
import HelpPanel from './HelpPanel.svelte';
import KeysPanel from './KeysPanel.svelte';
import LinkedSeriesPanel from './LinkedSeriesPanel.svelte';
import NetflixPanel from './NetflixPanel.svelte';
import NewsPanel from './NewsPanel.svelte';
import QuotePanel from './QuotePanel.svelte';
import RtPanel from './RtPanel.svelte';
import RtSearchPanel from './RtSearchPanel.svelte';
import SearchPanel from './SearchPanel.svelte';
import SpotPanel from './SpotPanel.svelte';
import AwardsPanel from './AwardsPanel.svelte';
import PodcastsPanel from './PodcastsPanel.svelte';
import ReleasesPanel from './ReleasesPanel.svelte';
import SteamPanel from './SteamPanel.svelte';
import TrendsPanel from './TrendsPanel.svelte';
import StreamChartPanel from './StreamChartPanel.svelte';
import TopPanel from './TopPanel.svelte';
import TradesPanel from './TradesPanel.svelte';
import TvPanel from './TvPanel.svelte';
import WatchlistPanel from './WatchlistPanel.svelte';
import type { PanelKind } from '../state/panels.svelte';

/* eslint-disable @typescript-eslint/no-explicit-any */
type AnyPanel = Component<any>;

export const PANEL_COMPONENTS: Record<PanelKind, AnyPanel> = {
  quote: QuotePanel,
  depth: DepthPanel,
  trades: TradesPanel,
  chart: ChartPanel,
  search: SearchPanel,
  event: EventPanel,
  top: TopPanel,
  watchlist: WatchlistPanel,
  spot: SpotPanel,
  'linked-series': LinkedSeriesPanel,
  compare: ComparePanel,
  news: NewsPanel,
  'data-series': DataSeriesPanel,
  'data-search': DataSearchPanel,
  sources: SourcesPanel,
  billboard: BillboardPanel,
  'billboard-charts': BillboardChartsPanel,
  ent: EntPanel,
  rt: RtPanel,
  'rt-search': RtSearchPanel,
  netflix: NetflixPanel,
  'stream-chart': StreamChartPanel,
  boxoffice: BoxOfficePanel,
  steam: SteamPanel,
  tv: TvPanel,
  awards: AwardsPanel,
  trends: TrendsPanel,
  releases: ReleasesPanel,
  podcasts: PodcastsPanel,
  help: HelpPanel,
  keys: KeysPanel,
};
