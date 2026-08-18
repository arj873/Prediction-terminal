/**
 * Which component renders which kind of panel.
 *
 * The command registry decides *what* to open — a kind and its props — and this
 * decides what that looks like. Keeping the two apart is what lets a command
 * handler be a one-liner and a panel be a component with no knowledge of the
 * command that opened it.
 *
 * Every kind resolves to something. A kind still being ported resolves to
 * `MissingPanel`, so a half-finished registry runs rather than throwing at the
 * grid.
 */

import type { Component } from 'svelte';

import BillboardChartsPanel from './BillboardChartsPanel.svelte';
import BillboardPanel from './BillboardPanel.svelte';
import BoxOfficePanel from './BoxOfficePanel.svelte';
import FredPanel from './FredPanel.svelte';
import FredSearchPanel from './FredSearchPanel.svelte';
import HelpPanel from './HelpPanel.svelte';
import KeysPanel from './KeysPanel.svelte';
import MissingPanel from './MissingPanel.svelte';
import NetflixPanel from './NetflixPanel.svelte';
import RtPanel from './RtPanel.svelte';
import RtSearchPanel from './RtSearchPanel.svelte';
import SteamPanel from './SteamPanel.svelte';
import StreamChartPanel from './StreamChartPanel.svelte';
import TvPanel from './TvPanel.svelte';
import type { PanelKind } from '../state/panels.svelte';

/* eslint-disable @typescript-eslint/no-explicit-any */
type AnyPanel = Component<any>;

export const PANEL_COMPONENTS: Record<PanelKind, AnyPanel> = {
  quote: MissingPanel,
  depth: MissingPanel,
  trades: MissingPanel,
  chart: MissingPanel,
  search: MissingPanel,
  event: MissingPanel,
  top: MissingPanel,
  watchlist: MissingPanel,
  spot: MissingPanel,
  'linked-series': MissingPanel,
  compare: MissingPanel,
  news: MissingPanel,
  fred: FredPanel,
  'fred-search': FredSearchPanel,
  billboard: BillboardPanel,
  'billboard-charts': BillboardChartsPanel,
  ent: MissingPanel,
  rt: RtPanel,
  'rt-search': RtSearchPanel,
  netflix: NetflixPanel,
  'stream-chart': StreamChartPanel,
  boxoffice: BoxOfficePanel,
  steam: SteamPanel,
  tv: TvPanel,
  help: HelpPanel,
  keys: KeysPanel,
};
