// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Missing IP prefixes (ADR-170 Inc.2) — the screen's judgement, where a node-environment
// test runs. `MissingPrefixesPage.tsx` is layout plus the call (`testing.md`).
//
// The comparison is the server's (`crates/yagra-core/src/api/prefix_gaps.rs`). What is decided here
// is only how the answer is shown: which tab a site sits on, what the search and the kind tiles
// keep, how the subnet list is ordered, and what the CSV carries. Why one gap is reported is the
// folder pane's sentence (`components/NodeDetail/prefixGaps.ts::gapReason`), reused as it is.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import type { PrefixGap, PrefixGapSitesView, SiteGapStatus, SitePrefixGaps } from '../types/api';
import { PREFIX_GAP_KINDS, type PrefixGapKind } from '../components/NodeDetail/prefixGaps';
import { csvField } from '../lib/csv';

/** Where a site stands, in the server's order — the tabs of the by-site view.
 *  ⚠️ **Keep the array on one line**: `yagra-core`'s
 *  `api/prefix_gaps.rs::the_webuis_status_list_is_this_enum_in_order` reads it. */
export const SITE_GAP_STATUSES = ['gaps', 'clean', 'no_data'] as const satisfies readonly SiteGapStatus[];

/** The two ways the screen lays the answer out. By site is the default (ADR-170 decision 11). */
export const MISSING_PREFIX_VIEWS = ['site', 'subnet'] as const;
export type MissingPrefixView = (typeof MISSING_PREFIX_VIEWS)[number];

/** A site's row key. `null` is the root — every device filed in no folder. */
export function siteKey(site: SitePrefixGaps): string {
  return site.site_id ?? 'root';
}

/** What narrows the answer: one kind (a tile) and a search term. */
export interface GapFilter {
  kind: PrefixGapKind | null;
  q: string;
}

/** Whether a gap survives the filter. The term matches the subnet, a listed address, a listed
 *  device's name, or the site's own name and folders — case-insensitively. */
export function gapMatches(
  gap: PrefixGap,
  site: SitePrefixGaps,
  filter: GapFilter,
  nodeName: (id: string) => string,
): boolean {
  if (filter.kind !== null && gap.kind !== filter.kind) return false;
  const q = filter.q.trim().toLowerCase();
  if (!q) return true;
  if (siteMatches(site, q)) return true;
  if (gap.subnet.toLowerCase().includes(q)) return true;
  return gap.seen_on.some(
    (s) => s.ip.toLowerCase().includes(q) || nodeName(s.node_id).toLowerCase().includes(q),
  );
}

function siteMatches(site: SitePrefixGaps, q: string): boolean {
  return [site.name ?? '', ...site.path].some((n) => n.toLowerCase().includes(q));
}

/** One site row, with the gaps of its that survive the filter. */
export interface SiteRow {
  site: SitePrefixGaps;
  gaps: PrefixGap[];
}

/** The sites on one tab. On the gaps tab a site with nothing left after the filter drops out;
 *  the other tabs hold no gaps, so there the term matches the site's name and folders only, and a
 *  kind tile does not apply. The server already orders the sites. */
export function sitesOn(
  view: PrefixGapSitesView | null,
  tab: SiteGapStatus,
  filter: GapFilter,
  nodeName: (id: string) => string,
): SiteRow[] {
  if (!view) return [];
  const q = filter.q.trim().toLowerCase();
  return view.sites
    .filter((site) => site.status === tab)
    .map((site) => ({ site, gaps: site.gaps.filter((g) => gapMatches(g, site, filter, nodeName)) }))
    .filter(({ site, gaps }) =>
      tab === 'gaps' ? gaps.length > 0 : !q || siteMatches(site, q),
    );
}

/** One line of the subnet view. */
export interface SubnetRow {
  key: string;
  site: SitePrefixGaps;
  gap: PrefixGap;
}

/** Every listed gap as one line, by kind (the tiles' order) and then subnet. */
export function subnetRows(
  view: PrefixGapSitesView | null,
  filter: GapFilter,
  nodeName: (id: string) => string,
): SubnetRow[] {
  if (!view) return [];
  const rows: SubnetRow[] = [];
  for (const site of view.sites) {
    for (const gap of site.gaps) {
      if (gapMatches(gap, site, filter, nodeName)) {
        rows.push({ key: `${siteKey(site)}|${gap.subnet}`, site, gap });
      }
    }
  }
  const order = (k: PrefixGapKind) => PREFIX_GAP_KINDS.indexOf(k);
  return rows.sort(
    (a, b) =>
      order(a.gap.kind) - order(b.gap.kind) ||
      a.gap.subnet.localeCompare(b.gap.subnet) ||
      (a.site.name ?? '').localeCompare(b.site.name ?? ''),
  );
}

/** The subnet view's filter keys carry this prefix: both tables live on one route, and the URL is
 *  where a filter is kept (`filterSpecRegistry.test.ts`'s route ledger). */
export const SUBNET_FILTER_PREFIX = 'subnets.';

/** A site's name and the folders above it — what the Site column filters on. */
function siteText(site: SitePrefixGaps, t: TFunction): string[] {
  return [site.name ?? t('missingPrefixes.root'), ...site.path];
}

/** The Site column's filter, the same in both views. Its hint is where "what a site is" and where
 *  the subnets come from are said (it used to be the page note, ADR-200 Inc.12). */
function siteFilter<T>(t: TFunction, site: (row: T) => SitePrefixGaps): ColumnFilterSpec<T> {
  return {
    kind: 'text',
    modes: ['contains', 'regex'],
    not: true,
    readText: (r) => siteText(site(r), t),
    containsSemantics: 'substring',
    placeholder: t('missingPrefixes.cols.site'),
    hint: t('missingPrefixes.siteFilterHint'),
  };
}

/** The by-site view's filter row, keyed by `Column.key`. */
export function siteRowFilters(t: TFunction): Record<string, ColumnFilterSpec<SiteRow>> {
  return { site: siteFilter<SiteRow>(t, (r) => r.site) };
}

/** The subnet view's filter row, keyed by `Column.key` (under `SUBNET_FILTER_PREFIX`). */
export function subnetRowFilters(t: TFunction): Record<string, ColumnFilterSpec<SubnetRow>> {
  return { site: siteFilter<SubnetRow>(t, (r) => r.site) };
}

/** How many listed gaps there are of each kind — the four tiles. */
export function kindCounts(gaps: Iterable<PrefixGap>): Record<PrefixGapKind, number> {
  const counts = Object.fromEntries(PREFIX_GAP_KINDS.map((k) => [k, 0])) as Record<
    PrefixGapKind,
    number
  >;
  for (const g of gaps) counts[g.kind] += 1;
  return counts;
}

/** Every gap the answer lists. */
export function allGaps(view: PrefixGapSitesView | null): PrefixGap[] {
  return view?.sites.flatMap((s) => s.gaps) ?? [];
}

/** How many sites sit on each tab. */
export function statusCounts(view: PrefixGapSitesView | null): Record<SiteGapStatus, number> {
  const counts = Object.fromEntries(SITE_GAP_STATUSES.map((s) => [s, 0])) as Record<
    SiteGapStatus,
    number
  >;
  for (const s of view?.sites ?? []) counts[s.status] += 1;
  return counts;
}

/** The CSV of the subnet view: one line per gap, with the columns a person registering the
 *  prefixes needs. The header is the API's own field vocabulary, not translated, so a script
 *  reading the file does not depend on the operator's language. Every cell goes through the shared
 *  `csvField`, which also neutralizes a value a spreadsheet would execute — the device name and
 *  addresses come from the network. CRLF per RFC 4180. */
export function gapsCsv(rows: SubnetRow[], nodeName: (id: string) => string): string {
  const head = ['subnet', 'kind', 'site', 'folders', 'range', 'devices', 'seen_on'];
  const lines = rows.map(({ site, gap }) =>
    [
      gap.subnet,
      gap.kind,
      site.name ?? '',
      site.path.join(' / '),
      gap.range ?? '',
      gap.node_count,
      gap.seen_on
        .map((s) => [nodeName(s.node_id), s.if_name ?? '', s.ip].filter(Boolean).join(' '))
        .join('; '),
    ]
      .map(csvField)
      .join(','),
  );
  return [head.map(csvField).join(','), ...lines].join('\r\n');
}
