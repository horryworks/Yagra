// My Dashboard widget catalog. The REGISTRY is the single source of truth for which widgets
// exist, their grid spans, and their backing tag. Only buildable widgets (those with a real
// component + live/started-backend data) are listed today; the broader catalog (rollup/new
// widgets awaiting endpoints) is tracked in the implementation plan and added here as its
// backend lands. `type` strings are stable and persisted — never renumber them.

import { DASHBOARD_VERSION, type RegistryView } from './layout';
import type { DashboardLayout, Span, WidgetDefinition, WidgetInstance } from './types';
import {
  ActiveAlertsActions,
  ActiveAlertsWidget,
  AlertVolumeWidget,
  FlappingWatchlistWidget,
  SeverityMixWidget,
} from './widgets/alerts';
import { HealthRingWidget, NodesDownWidget, StatusSummaryWidget } from './widgets/fleet';
import { AuditWidget, MaintenanceWidget } from './widgets/monitoring';
import {
  BusiestInterfacesWidget,
  MostErrorsWidget,
  TopAggActions,
  TopCpuWidget,
  TopMemoryWidget,
  TopRttWidget,
  TopTalkersWidget,
} from './widgets/performance';
import { RegionRollupWidget, SiteHealthMatrixWidget } from './widgets/sites';
import './widgets/widgets.css';

const SECTION = {
  fleet: '01 · Fleet status',
  alerts: '02 · Alerts',
  performance: '03 · Performance hotspots',
  sites: '04 · Sites & topology',
  capacity: '05 · Capacity & traffic',
  monitoring: '06 · Monitoring health',
} as const;

export const REGISTRY: WidgetDefinition[] = [
  {
    type: 'status-summary',
    title: 'Status summary',
    section: SECTION.fleet,
    blurb: 'Total node count + breakdown by state.',
    backing: 'live',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    Component: StatusSummaryWidget,
  },
  {
    type: 'health-ring',
    title: 'Health ring',
    section: SECTION.fleet,
    blurb: 'Donut of healthy / warning / critical with % healthy.',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    Component: HealthRingWidget,
  },
  {
    type: 'nodes-down',
    title: 'Nodes down',
    section: SECTION.fleet,
    blurb: 'Big-number tile: nodes currently critical or unreachable.',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    Component: NodesDownWidget,
  },
  {
    type: 'active-alerts',
    title: 'Active alerts',
    section: SECTION.alerts,
    blurb: 'Worst-first live feed of firing alerts.',
    backing: 'live',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    Component: ActiveAlertsWidget,
    Actions: ActiveAlertsActions,
  },
  {
    type: 'alert-volume',
    title: 'Alert volume',
    section: SECTION.alerts,
    blurb: 'Alerts opened per hour over the last 24h.',
    backing: 'live',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    Component: AlertVolumeWidget,
  },
  {
    type: 'severity-mix',
    title: 'Severity mix',
    section: SECTION.alerts,
    blurb: 'Donut of active alerts by severity.',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    Component: SeverityMixWidget,
  },
  {
    type: 'flapping-watchlist',
    title: 'Flapping watchlist',
    section: SECTION.alerts,
    blurb: 'Checks toggling repeatedly (flapping flag).',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    Component: FlappingWatchlistWidget,
  },
  {
    type: 'top-rtt',
    title: 'Top ICMP RTT',
    section: SECTION.performance,
    blurb: 'Highest-latency nodes (now or 1h peak).',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    Component: TopRttWidget,
    Actions: TopAggActions,
  },
  {
    type: 'top-cpu',
    title: 'Top CPU',
    section: SECTION.performance,
    blurb: 'Busiest control planes, % utilization (vendor CPU gauges).',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    Component: TopCpuWidget,
    Actions: TopAggActions,
  },
  {
    type: 'top-memory',
    title: 'Top memory',
    section: SECTION.performance,
    blurb: 'Nodes nearest the memory ceiling, % used (vendor % gauges).',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    Component: TopMemoryWidget,
    Actions: TopAggActions,
  },
  {
    type: 'busiest-interfaces',
    title: 'Busiest interfaces',
    section: SECTION.performance,
    blurb: 'Highest-throughput links, utilization % where speed is known.',
    backing: 'rollup',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    Component: BusiestInterfacesWidget,
    Actions: TopAggActions,
  },
  {
    type: 'most-interface-errors',
    title: 'Most interface errors',
    section: SECTION.performance,
    blurb: 'Links shedding the most errors/sec (in+out).',
    backing: 'rollup',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    Component: MostErrorsWidget,
    Actions: TopAggActions,
  },
  {
    type: 'top-talkers',
    title: 'Top talkers',
    section: SECTION.capacity,
    blurb: 'Interfaces moving the most bits now (in+out).',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    Component: TopTalkersWidget,
    Actions: TopAggActions,
  },
  {
    type: 'site-matrix',
    title: 'Site health matrix',
    section: SECTION.sites,
    blurb: 'A tile per node-group: worst member state + up/total.',
    backing: 'live',
    defaultSpan: 8,
    allowedSpans: [6, 8, 12],
    Component: SiteHealthMatrixWidget,
  },
  {
    type: 'region-rollup',
    title: 'Region rollup',
    section: SECTION.sites,
    blurb: 'Percent healthy per top-level group.',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    Component: RegionRollupWidget,
  },
  {
    type: 'maintenance',
    title: 'Maintenance windows',
    section: SECTION.monitoring,
    blurb: "What's suppressed now + upcoming windows.",
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    Component: MaintenanceWidget,
  },
  {
    type: 'audit',
    title: 'Recent changes',
    section: SECTION.monitoring,
    blurb: 'Latest config/admin actions (audit log; admin-only).',
    backing: 'live',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    Component: AuditWidget,
  },
];

const BY_TYPE = new Map(REGISTRY.map((d) => [d.type, d]));

/** The definition for a widget type, if it's in the catalog. */
export function getDefinition(type: string): WidgetDefinition | undefined {
  return BY_TYPE.get(type);
}

/** Registry-derived predicates for the pure layout helpers. */
export const registryView: RegistryView = {
  isKnownType: (type) => BY_TYPE.has(type),
  allowedSpansFor: (type) => BY_TYPE.get(type)?.allowedSpans ?? [],
  defaultSpanFor: (type) => BY_TYPE.get(type)?.defaultSpan ?? (6 as Span),
};

/** Catalog grouped by section, in registry order (for the picker). */
export function catalogBySection(): { section: string; widgets: WidgetDefinition[] }[] {
  const out: { section: string; widgets: WidgetDefinition[] }[] = [];
  for (const def of REGISTRY) {
    let group = out.find((g) => g.section === def.section);
    if (!group) {
      group = { section: def.section, widgets: [] };
      out.push(group);
    }
    group.widgets.push(def);
  }
  return out;
}

/** A starter board for a user who has never saved one: a representative cross-section. Stable
 *  instanceIds so a re-render/round-trip doesn't churn them. */
const DEFAULT_WIDGETS: WidgetInstance[] = [
  { instanceId: 'w-status', type: 'status-summary', span: 6 },
  { instanceId: 'w-alerts', type: 'active-alerts', span: 6 },
  { instanceId: 'w-health', type: 'health-ring', span: 4 },
  { instanceId: 'w-severity', type: 'severity-mix', span: 4 },
  { instanceId: 'w-rtt', type: 'top-rtt', span: 4 },
];

/** A fresh copy of the default layout (callers mutate their own copy). */
export function defaultLayout(): DashboardLayout {
  return { version: DASHBOARD_VERSION, widgets: DEFAULT_WIDGETS.map((w) => ({ ...w })) };
}
