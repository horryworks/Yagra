// SPDX-License-Identifier: AGPL-3.0-only
// My Dashboard widget catalog. The REGISTRY is the single source of truth for which widgets
// exist, their grid spans, and their backing tag. Only buildable widgets (those with a real
// component + live/started-backend data) are listed today; the broader catalog (rollup/new
// widgets awaiting endpoints) is tracked in the implementation plan and added here as its
// backend lands. `type` strings are stable and persisted — never renumber them.

import { DASHBOARD_VERSION, type RegistryView } from './layout';
import type { DashboardLayout, RowSpan, Span, WidgetDefinition, WidgetInstance } from './types';
import {
  ActiveAlertsActions,
  ActiveAlertsWidget,
  AlertCalendarWidget,
  AlertVolumeWidget,
  FlappingWatchlistWidget,
  RecentStateChangesWidget,
  SeverityMixWidget,
  TopAlertingNodesWidget,
} from './widgets/alerts';
import {
  FleetHealthTimelineWidget,
  HealthRingWidget,
  NodesDownWidget,
  StatusSummaryWidget,
} from './widgets/fleet';
import {
  AuditWidget,
  DataCoverageWidget,
  DiscoveryQueueWidget,
  MaintenanceWidget,
  PollerHealthWidget,
  StaleDataWidget,
} from './widgets/monitoring';
import {
  BusiestInterfacesWidget,
  MostDiscardsWidget,
  MostErrorsWidget,
  TopAggActions,
  TopCpuWidget,
  TopMemoryWidget,
  TopRttWidget,
  TopTalkersWidget,
} from './widgets/performance';
import {
  MetricChartSettings,
  MetricChartWidget,
  MetricTopSettings,
  MetricTopWidget,
} from './widgets/metrics';
import { GeoMapWidget, RegionRollupWidget, SiteHealthMatrixWidget } from './widgets/sites';
import { DependencyWidget } from './widgets/topology';
import {
  AggregateThroughputWidget,
  InterfaceHeatmapWidget,
  InterfaceTrafficActions,
  InterfaceTrafficSettings,
  InterfaceTrafficWidget,
  TrafficDropsWidget,
  TrafficSpikesWidget,
} from './widgets/capacity';
import {
  EventFeedActions,
  EventFeedWidget,
  EventKindMixWidget,
  EventRuleCoverageWidget,
  EventTriageMixWidget,
  EventVolumeWidget,
  NoisyEventSourcesWidget,
  TopTrapTypesWidget,
} from './widgets/events';
import {
  FlowAsDirActions,
  FlowConversationsWidget,
  FlowProtoMixWidget,
  FlowTopAsWidget,
  FlowTopPortsWidget,
  FlowTopTalkersWidget,
  FlowTrendWidget,
} from './widgets/flow';
import './widgets/widgets.css';

// Section order is the registry array order (catalogBySection preserves it). Values are i18n keys
// (namespace `dashboard`) resolved at the call site (CatalogModal) — the registry is a module-level
// constant, so it can't resolve translations itself without pinning one language (see i18n rule 6).
const SECTION = {
  fleet: 'registry.sections.fleet',
  alerts: 'registry.sections.alerts',
  performance: 'registry.sections.performance',
  sites: 'registry.sections.sites',
  capacity: 'registry.sections.capacity',
  events: 'registry.sections.events',
  flow: 'registry.sections.flow',
  monitoring: 'registry.sections.monitoring',
} as const;

// `title` / `blurb` hold i18n keys (namespace `dashboard`), resolved where the definition is
// rendered (CatalogModal, WidgetFrame) — never here, so the label follows the active language.
export const REGISTRY: WidgetDefinition[] = [
  {
    type: 'status-summary',
    title: 'registry.widgets.status-summary.title',
    section: SECTION.fleet,
    blurb: 'registry.widgets.status-summary.blurb',
    backing: 'live',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    reads: ['GET /api/v1/fleet/summary'],
    Component: StatusSummaryWidget,
  },
  {
    type: 'health-ring',
    title: 'registry.widgets.health-ring.title',
    section: SECTION.fleet,
    blurb: 'registry.widgets.health-ring.blurb',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    reads: ['GET /api/v1/fleet/summary'],
    Component: HealthRingWidget,
  },
  {
    type: 'nodes-down',
    title: 'registry.widgets.nodes-down.title',
    section: SECTION.fleet,
    blurb: 'registry.widgets.nodes-down.blurb',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    reads: ['GET /api/v1/fleet/summary'],
    Component: NodesDownWidget,
  },
  {
    type: 'fleet-health-timeline',
    title: 'registry.widgets.fleet-health-timeline.title',
    section: SECTION.fleet,
    blurb: 'registry.widgets.fleet-health-timeline.blurb',
    backing: 'rollup',
    defaultSpan: 8,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2],
    reads: ['GET /api/v1/fleet/state-history'],
    Component: FleetHealthTimelineWidget,
  },
  {
    type: 'recent-state-changes',
    title: 'registry.widgets.recent-state-changes.title',
    section: SECTION.fleet,
    blurb: 'registry.widgets.recent-state-changes.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/alerts/transitions'],
    Component: RecentStateChangesWidget,
  },
  {
    type: 'active-alerts',
    title: 'registry.widgets.active-alerts.title',
    section: SECTION.alerts,
    blurb: 'registry.widgets.active-alerts.blurb',
    backing: 'live',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/stream/alerts'],
    Component: ActiveAlertsWidget,
    Actions: ActiveAlertsActions,
  },
  {
    type: 'alert-volume',
    title: 'registry.widgets.alert-volume.title',
    section: SECTION.alerts,
    blurb: 'registry.widgets.alert-volume.blurb',
    backing: 'live',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2],
    reads: ['GET /api/v1/alerts/history'],
    Component: AlertVolumeWidget,
  },
  {
    type: 'severity-mix',
    title: 'registry.widgets.severity-mix.title',
    section: SECTION.alerts,
    blurb: 'registry.widgets.severity-mix.blurb',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    reads: ['GET /api/v1/stream/alerts'],
    Component: SeverityMixWidget,
  },
  {
    type: 'flapping-watchlist',
    title: 'registry.widgets.flapping-watchlist.title',
    section: SECTION.alerts,
    blurb: 'registry.widgets.flapping-watchlist.blurb',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: [
      'GET /api/v1/stream/alerts',
      'POST /api/v1/node-names',
    ],
    Component: FlappingWatchlistWidget,
  },
  {
    type: 'top-alerting-nodes',
    title: 'registry.widgets.top-alerting-nodes.title',
    section: SECTION.alerts,
    blurb: 'registry.widgets.top-alerting-nodes.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/alerts/top-nodes'],
    Component: TopAlertingNodesWidget,
  },
  {
    type: 'alert-calendar',
    title: 'registry.widgets.alert-calendar.title',
    section: SECTION.alerts,
    blurb: 'registry.widgets.alert-calendar.blurb',
    backing: 'rollup',
    defaultSpan: 8,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2],
    reads: ['GET /api/v1/alerts/calendar'],
    Component: AlertCalendarWidget,
  },
  {
    type: 'top-rtt',
    title: 'registry.widgets.top-rtt.title',
    section: SECTION.performance,
    blurb: 'registry.widgets.top-rtt.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/metrics/top'],
    Component: TopRttWidget,
    Actions: TopAggActions,
  },
  {
    type: 'top-cpu',
    title: 'registry.widgets.top-cpu.title',
    section: SECTION.performance,
    blurb: 'registry.widgets.top-cpu.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/metrics/top'],
    Component: TopCpuWidget,
    Actions: TopAggActions,
  },
  {
    type: 'top-memory',
    title: 'registry.widgets.top-memory.title',
    section: SECTION.performance,
    blurb: 'registry.widgets.top-memory.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/metrics/top'],
    Component: TopMemoryWidget,
    Actions: TopAggActions,
  },
  {
    // The only widget whose subject the catalog does not know in advance: it charts whichever
    // metric the operator picks off a node's inventory, including ones no curated card covers
    // (ADR-046 Inc.2). Taller heights are worth offering — a chart is what it is showing.
    type: 'metric-chart',
    title: 'registry.widgets.metric-chart.title',
    section: SECTION.performance,
    blurb: 'registry.widgets.metric-chart.blurb',
    backing: 'live',
    defaultSpan: 6,
    allowedSpans: [4, 6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: [
      'GET /api/v1/nodes/{node_id}/metrics',
      'GET /api/v1/nodes/{node_id}/metrics/{metric}/range',
    ],
    Component: MetricChartWidget,
    // No view-mode actions at all: both of this widget's controls choose its subject, so both sit
    // behind the ⚙ (ADR-072). It has no window and no lens to leave in the header.
    Settings: MetricChartSettings,
  },
  {
    // Its fleet-wide twin (ADR-046 Inc.3): the same "any metric" question asked of every node at
    // once. The metric is typed rather than picked from a list, because nothing may enumerate the
    // fleet's metric names — see `metricTop.ts`.
    type: 'metric-top',
    title: 'registry.widgets.metric-top.title',
    section: SECTION.performance,
    blurb: 'registry.widgets.metric-top.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/metrics/top'],
    Component: MetricTopWidget,
    // The window is a view control and stays in the header; the metric name is the subject.
    Actions: TopAggActions,
    Settings: MetricTopSettings,
  },
  {
    type: 'busiest-interfaces',
    title: 'registry.widgets.busiest-interfaces.title',
    section: SECTION.performance,
    blurb: 'registry.widgets.busiest-interfaces.blurb',
    backing: 'rollup',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/metrics/interface-top'],
    Component: BusiestInterfacesWidget,
    Actions: TopAggActions,
  },
  {
    type: 'most-interface-errors',
    title: 'registry.widgets.most-interface-errors.title',
    section: SECTION.performance,
    blurb: 'registry.widgets.most-interface-errors.blurb',
    backing: 'rollup',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/metrics/interface-top'],
    Component: MostErrorsWidget,
    Actions: TopAggActions,
  },
  {
    type: 'most-interface-discards',
    title: 'registry.widgets.most-interface-discards.title',
    section: SECTION.performance,
    blurb: 'registry.widgets.most-interface-discards.blurb',
    backing: 'rollup',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/metrics/interface-top'],
    Component: MostDiscardsWidget,
    Actions: TopAggActions,
  },
  {
    type: 'top-talkers',
    title: 'registry.widgets.top-talkers.title',
    section: SECTION.capacity,
    blurb: 'registry.widgets.top-talkers.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/metrics/interface-top'],
    Component: TopTalkersWidget,
    Actions: TopAggActions,
  },
  {
    type: 'aggregate-throughput',
    title: 'registry.widgets.aggregate-throughput.title',
    section: SECTION.capacity,
    blurb: 'registry.widgets.aggregate-throughput.blurb',
    backing: 'rollup',
    defaultSpan: 8,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2],
    reads: ['GET /api/v1/metrics/throughput-range'],
    Component: AggregateThroughputWidget,
  },
  {
    type: 'interface-heatmap',
    title: 'registry.widgets.interface-heatmap.title',
    section: SECTION.capacity,
    blurb: 'registry.widgets.interface-heatmap.blurb',
    backing: 'rollup',
    defaultSpan: 8,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: [
      'GET /api/v1/metrics/interface-heatmap',
      'GET /api/v1/nodes/{node_id}/interfaces',
    ],
    Component: InterfaceHeatmapWidget,
  },
  {
    // Never renumber: the type is what a saved board stores.
    type: 'interface-traffic',
    title: 'registry.widgets.interface-traffic.title',
    section: SECTION.capacity,
    blurb: 'registry.widgets.interface-traffic.blurb',
    backing: 'live',
    defaultSpan: 8,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: [
      'GET /api/v1/nodes/{node_id}/interfaces',
      'GET /api/v1/nodes/{node_id}/interfaces/{ifindex}/series',
    ],
    Component: InterfaceTrafficWidget,
    // Unit and window in the header; which interfaces are plotted behind the ⚙ (ADR-072).
    Actions: InterfaceTrafficActions,
    Settings: InterfaceTrafficSettings,
  },
  {
    type: 'traffic-spikes',
    title: 'registry.widgets.traffic-spikes.title',
    section: SECTION.capacity,
    blurb: 'registry.widgets.traffic-spikes.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/metrics/interface-delta'],
    Component: TrafficSpikesWidget,
  },
  {
    type: 'traffic-drops',
    title: 'registry.widgets.traffic-drops.title',
    section: SECTION.capacity,
    blurb: 'registry.widgets.traffic-drops.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/metrics/interface-delta'],
    Component: TrafficDropsWidget,
  },
  {
    type: 'event-feed',
    title: 'registry.widgets.event-feed.title',
    section: SECTION.events,
    blurb: 'registry.widgets.event-feed.blurb',
    backing: 'live',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: [
      'GET /api/v1/events',
      'POST /api/v1/node-names',
    ],
    Component: EventFeedWidget,
    Actions: EventFeedActions,
  },
  {
    type: 'event-volume',
    title: 'registry.widgets.event-volume.title',
    section: SECTION.events,
    blurb: 'registry.widgets.event-volume.blurb',
    backing: 'rollup',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2],
    reads: ['GET /api/v1/events/stats'],
    Component: EventVolumeWidget,
  },
  {
    type: 'event-kind-mix',
    title: 'registry.widgets.event-kind-mix.title',
    section: SECTION.events,
    blurb: 'registry.widgets.event-kind-mix.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    reads: ['GET /api/v1/events/stats'],
    Component: EventKindMixWidget,
  },
  {
    type: 'top-trap-types',
    title: 'registry.widgets.top-trap-types.title',
    section: SECTION.events,
    blurb: 'registry.widgets.top-trap-types.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/events/stats'],
    Component: TopTrapTypesWidget,
  },
  {
    type: 'event-triage-mix',
    title: 'registry.widgets.event-triage-mix.title',
    section: SECTION.events,
    blurb: 'registry.widgets.event-triage-mix.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    reads: ['GET /api/v1/events/stats'],
    Component: EventTriageMixWidget,
  },
  {
    type: 'noisy-event-sources',
    title: 'registry.widgets.noisy-event-sources.title',
    section: SECTION.events,
    blurb: 'registry.widgets.noisy-event-sources.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: [
      'GET /api/v1/events/stats',
      'POST /api/v1/node-names',
    ],
    Component: NoisyEventSourcesWidget,
  },
  {
    type: 'event-rule-coverage',
    title: 'registry.widgets.event-rule-coverage.title',
    section: SECTION.events,
    blurb: 'registry.widgets.event-rule-coverage.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    reads: ['GET /api/v1/events/stats'],
    Component: EventRuleCoverageWidget,
  },
  {
    type: 'flow-top-talkers',
    title: 'registry.widgets.flow-top-talkers.title',
    section: SECTION.flow,
    blurb: 'registry.widgets.flow-top-talkers.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/flow/top-talkers'],
    Component: FlowTopTalkersWidget,
  },
  {
    type: 'flow-top-as',
    title: 'registry.widgets.flow-top-as.title',
    section: SECTION.flow,
    blurb: 'registry.widgets.flow-top-as.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/flow/top-as'],
    Component: FlowTopAsWidget,
    Actions: FlowAsDirActions,
  },
  {
    type: 'flow-top-ports',
    title: 'registry.widgets.flow-top-ports.title',
    section: SECTION.flow,
    blurb: 'registry.widgets.flow-top-ports.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/flow/top-ports'],
    Component: FlowTopPortsWidget,
  },
  {
    type: 'flow-proto-mix',
    title: 'registry.widgets.flow-proto-mix.title',
    section: SECTION.flow,
    blurb: 'registry.widgets.flow-proto-mix.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    reads: ['GET /api/v1/flow/protocols'],
    Component: FlowProtoMixWidget,
  },
  {
    type: 'flow-conversations',
    title: 'registry.widgets.flow-conversations.title',
    section: SECTION.flow,
    blurb: 'registry.widgets.flow-conversations.blurb',
    backing: 'rollup',
    defaultSpan: 8,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/flow/conversations'],
    Component: FlowConversationsWidget,
  },
  {
    type: 'flow-trend',
    title: 'registry.widgets.flow-trend.title',
    section: SECTION.flow,
    blurb: 'registry.widgets.flow-trend.blurb',
    backing: 'rollup',
    defaultSpan: 8,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2],
    reads: ['GET /api/v1/flow/series'],
    Component: FlowTrendWidget,
  },
  {
    type: 'site-matrix',
    title: 'registry.widgets.site-matrix.title',
    section: SECTION.sites,
    blurb: 'registry.widgets.site-matrix.blurb',
    backing: 'live',
    defaultSpan: 8,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: [
      'GET /api/v1/fleet/group-summary',
      'GET /api/v1/node-groups',
    ],
    Component: SiteHealthMatrixWidget,
  },
  {
    type: 'region-rollup',
    title: 'registry.widgets.region-rollup.title',
    section: SECTION.sites,
    blurb: 'registry.widgets.region-rollup.blurb',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2],
    reads: [
      'GET /api/v1/fleet/group-summary',
      'GET /api/v1/node-groups',
    ],
    Component: RegionRollupWidget,
  },
  {
    type: 'geo-map',
    title: 'registry.widgets.geo-map.title',
    section: SECTION.sites,
    blurb: 'registry.widgets.geo-map.blurb',
    backing: 'new',
    defaultSpan: 6,
    allowedSpans: [4, 6, 8],
    allowedRowSpans: [1, 2, 3],
    reads: [
      'GET /api/v1/fleet/group-summary',
      'GET /api/v1/node-groups',
    ],
    Component: GeoMapWidget,
  },
  {
    type: 'dependency-view',
    title: 'registry.widgets.dependency-view.title',
    section: SECTION.sites,
    blurb: 'registry.widgets.dependency-view.blurb',
    backing: 'rollup',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: [
      'GET /api/v1/topology',
      'GET /api/v1/stream/node-states',
    ],
    Component: DependencyWidget,
  },
  {
    type: 'maintenance',
    title: 'registry.widgets.maintenance.title',
    section: SECTION.monitoring,
    blurb: 'registry.widgets.maintenance.blurb',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/maintenance-windows'],
    Component: MaintenanceWidget,
  },
  {
    type: 'poller-health',
    title: 'registry.widgets.poller-health.title',
    section: SECTION.monitoring,
    blurb: 'registry.widgets.poller-health.blurb',
    backing: 'new',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    reads: ['GET /api/v1/poller-health'],
    Component: PollerHealthWidget,
  },
  {
    type: 'discovery-queue',
    title: 'registry.widgets.discovery-queue.title',
    section: SECTION.monitoring,
    blurb: 'registry.widgets.discovery-queue.blurb',
    backing: 'live',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/discovery/candidates'],
    Component: DiscoveryQueueWidget,
  },
  {
    type: 'data-coverage',
    title: 'registry.widgets.data-coverage.title',
    section: SECTION.monitoring,
    blurb: 'registry.widgets.data-coverage.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    reads: ['GET /api/v1/fleet/coverage'],
    Component: DataCoverageWidget,
  },
  {
    type: 'stale-data',
    title: 'registry.widgets.stale-data.title',
    section: SECTION.monitoring,
    blurb: 'registry.widgets.stale-data.blurb',
    backing: 'rollup',
    defaultSpan: 4,
    allowedSpans: [4, 6],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/fleet/coverage'],
    Component: StaleDataWidget,
  },
  {
    type: 'audit',
    title: 'registry.widgets.audit.title',
    section: SECTION.monitoring,
    blurb: 'registry.widgets.audit.blurb',
    backing: 'live',
    defaultSpan: 6,
    allowedSpans: [6, 8, 12],
    allowedRowSpans: [1, 2, 3],
    reads: ['GET /api/v1/audit'],
    Component: AuditWidget,
  },
];

const BY_TYPE = new Map(REGISTRY.map((d) => [d.type, d]));

/** The definition for a widget type, if it's in the catalog. */
export function getDefinition(type: string): WidgetDefinition | undefined {
  return BY_TYPE.get(type);
}

/** Registry-derived predicates for the pure layout helpers. A widget with no declared
 *  `allowedRowSpans` is fixed-height: it allows only `[1]` and defaults to `1`. */
export const registryView: RegistryView = {
  isKnownType: (type) => BY_TYPE.has(type),
  allowedSpansFor: (type) => BY_TYPE.get(type)?.allowedSpans ?? [],
  defaultSpanFor: (type) => BY_TYPE.get(type)?.defaultSpan ?? (6 as Span),
  allowedRowSpansFor: (type) => BY_TYPE.get(type)?.allowedRowSpans ?? [1],
  defaultRowSpanFor: (type) => BY_TYPE.get(type)?.defaultRowSpan ?? (1 as RowSpan),
};

/** Catalog grouped by section, in registry order (for the picker). */
export function catalogBySection(
  defs: WidgetDefinition[] = REGISTRY,
): { section: string; widgets: WidgetDefinition[] }[] {
  const out: { section: string; widgets: WidgetDefinition[] }[] = [];
  for (const def of defs) {
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

/** A fresh copy of the default layout — one board with the starter widgets (callers mutate their
 *  own copy). */
export function defaultLayout(): DashboardLayout {
  return {
    version: DASHBOARD_VERSION,
    boards: [{ id: 'board-1', name: 'Dashboard 1', widgets: DEFAULT_WIDGETS.map((w) => ({ ...w })) }],
  };
}

/** The public board's starting point: **empty**, unlike [`defaultLayout`] (ADR-123).
 *
 *  🚨 The default here is a publishing decision, not a presentation one. Seeding it with the same
 *  five widgets would mean that turning the switch on published a fleet summary nobody chose to
 *  publish — and, because the anonymous route allow-list is derived from the widgets on this board,
 *  it would open their API routes too. An admin composes what strangers see, deliberately, or they
 *  see an empty board and a message saying so. */
export function emptyPublicLayout(): DashboardLayout {
  return {
    version: DASHBOARD_VERSION,
    boards: [{ id: 'board-1', name: 'Public', widgets: [] }],
  };
}
