// SPDX-License-Identifier: AGPL-3.0-only
// Navigation IA — the single source of truth for the site map (.claude/docs/design-system.md §2.2,
// decision log §6). The top bar renders SECTIONS (text-only tabs); the sidebar renders the
// selected section's ITEMS, grouped so read/monitor screens sit apart from configuration screens
// (a labeled group per cluster). `implemented: false` items route to the ComingSoon placeholder
// (their backend endpoints don't exist yet) but still appear so the information architecture stays
// whole and screens can be slotted in later without renumbering the nav — the SideBar collects them
// into a trailing "Coming soon" group so they don't clutter the working items.
//
// Labels are i18n keys into the `nav` namespace (see locales/<lng>/nav.json), resolved with `t()`
// at render time — never store the display string here, or a language switch wouldn't re-render.

export interface NavItem {
  /** i18n key into the `nav` namespace (e.g. 'nodes.all'). Resolved via `t()` at render. */
  labelKey: string;
  /** Optional i18n key into `descriptions.*` — a one-line hover tooltip for non-obvious items. */
  descKey?: string;
  /** Absolute route path. */
  path: string;
  /** Has a real backend today. false ⇒ ComingSoon placeholder. */
  implemented: boolean;
  /** Short monogram shown in the collapsed icon rail. */
  mono: string;
  /** Optional live count badge keyed by a discriminator the SideBar resolves to a store value
   *  (e.g. 'troubleshoot-runs' ⇒ number of running analysis jobs). */
  liveBadge?: 'troubleshoot-runs';
}

export interface NavGroup {
  /** i18n key into `groups.*`. Omit ⇒ unlabeled group (a single-group section renders no header). */
  labelKey?: string;
  items: NavItem[];
}

export interface NavSection {
  key: string;
  /** i18n key into the `nav` namespace (e.g. 'sections.nodes'). Resolved via `t()` at render. */
  labelKey: string;
  /** Where the top-bar tab navigates (its first/landing child). */
  path: string;
  groups: NavGroup[];
}

export const NAV: NavSection[] = [
  {
    key: 'dashboard',
    labelKey: 'sections.dashboard',
    path: '/dashboard',
    groups: [
      {
        items: [
          { labelKey: 'dashboard.shared', path: '/dashboard', implemented: true, mono: 'Sh' },
          { labelKey: 'dashboard.my', path: '/dashboard/my', implemented: true, mono: 'My' },
          { labelKey: 'dashboard.reports', path: '/dashboard/reports', implemented: true, mono: 'Rp' },
        ],
      },
    ],
  },
  {
    key: 'nodes',
    labelKey: 'sections.nodes',
    path: '/nodes',
    groups: [
      {
        labelKey: 'groups.inventory',
        items: [
          { labelKey: 'nodes.all', path: '/nodes', implemented: true, mono: 'Al' },
          { labelKey: 'nodes.discovery', path: '/nodes/discovery', implemented: true, mono: 'Di' },
        ],
      },
      {
        // ⚠️ ORDER IS THE ORDER OF THE WORK (ADR-055 R7): what to read → the bundle that names it →
        // the profile that attaches the bundle → the rule that assigns the profile. It ran the
        // other way and the first item's own description said "Attach Metric sets here", which is
        // two entries further down — the menu was telling the reader to start at the end.
        labelKey: 'groups.monitoringConfig',
        items: [
          {
            labelKey: 'nodes.mib',
            descKey: 'descriptions.nodesMib',
            path: '/nodes/mib',
            implemented: true,
            mono: 'Mb',
          },
          {
            labelKey: 'nodes.metricSets',
            descKey: 'descriptions.nodesMetricSets',
            path: '/nodes/collection-templates',
            implemented: true,
            mono: 'Ms',
          },
          {
            labelKey: 'nodes.profiles',
            descKey: 'descriptions.nodesProfiles',
            path: '/nodes/profiles',
            implemented: true,
            mono: 'Pr',
          },
          {
            labelKey: 'nodes.classificationRules',
            descKey: 'descriptions.nodesClassificationRules',
            path: '/nodes/classification-rules',
            implemented: true,
            mono: 'Cl',
          },
        ],
      },
    ],
  },
  {
    key: 'topology',
    labelKey: 'sections.topology',
    path: '/topology/map',
    groups: [
      {
        items: [
          {
            labelKey: 'topology.map',
            descKey: 'descriptions.topologyMap',
            path: '/topology/map',
            implemented: true,
            mono: 'Nm',
          },
          {
            labelKey: 'topology.dependency',
            descKey: 'descriptions.topologyDependency',
            path: '/topology/dependency',
            implemented: true,
            mono: 'Dp',
          },
          { labelKey: 'topology.geo', path: '/topology/geo', implemented: true, mono: 'Ge' },
        ],
      },
    ],
  },
  {
    key: 'alerts',
    labelKey: 'sections.alerts',
    path: '/alerts',
    groups: [
      {
        labelKey: 'groups.monitor',
        items: [
          { labelKey: 'alerts.active', path: '/alerts', implemented: true, mono: 'Ac' },
          { labelKey: 'alerts.history', path: '/alerts/history', implemented: true, mono: 'Hi' },
          { labelKey: 'alerts.events', path: '/alerts/events', implemented: true, mono: 'Ev' },
        ],
      },
      {
        labelKey: 'groups.configure',
        items: [
          {
            labelKey: 'alerts.rules',
            descKey: 'descriptions.alertsRules',
            path: '/alerts/rules',
            implemented: true,
            mono: 'Ru',
          },
          {
            labelKey: 'alerts.routing',
            descKey: 'descriptions.alertsRouting',
            path: '/alerts/routing',
            implemented: true,
            mono: 'Nt',
          },
          {
            labelKey: 'alerts.eventRules',
            descKey: 'descriptions.alertsEventRules',
            path: '/alerts/event-rules',
            implemented: true,
            mono: 'Er',
          },
          {
            labelKey: 'alerts.eventSources',
            descKey: 'descriptions.alertsEventSources',
            path: '/alerts/event-sources',
            implemented: true,
            mono: 'Es',
          },
          {
            labelKey: 'alerts.maintenance',
            descKey: 'descriptions.alertsMaintenance',
            path: '/alerts/maintenance',
            implemented: true,
            mono: 'Mw',
          },
          {
            labelKey: 'alerts.mutes',
            descKey: 'descriptions.alertsMutes',
            path: '/alerts/mutes',
            implemented: true,
            mono: 'Mu',
          },
        ],
      },
    ],
  },
  {
    key: 'troubleshoot',
    labelKey: 'sections.troubleshoot',
    path: '/troubleshoot',
    groups: [
      {
        items: [
          { labelKey: 'troubleshoot.all', path: '/troubleshoot', implemented: true, mono: 'To' },
          {
            labelKey: 'troubleshoot.runs',
            descKey: 'descriptions.troubleshootRuns',
            path: '/troubleshoot/runs',
            implemented: true,
            mono: 'Ar',
            liveBadge: 'troubleshoot-runs',
          },
          {
            labelKey: 'troubleshoot.scheduled',
            descKey: 'descriptions.troubleshootScheduled',
            path: '/troubleshoot/scheduled',
            implemented: true,
            mono: 'Sc',
          },
          {
            labelKey: 'troubleshoot.findings',
            descKey: 'descriptions.troubleshootFindings',
            path: '/troubleshoot/findings',
            implemented: true,
            mono: 'Sf',
          },
        ],
      },
    ],
  },
  {
    key: 'settings',
    labelKey: 'sections.settings',
    path: '/settings/system-health',
    groups: [
      {
        labelKey: 'groups.system',
        items: [
          {
            labelKey: 'settings.systemHealth',
            descKey: 'descriptions.settingsSystemHealth',
            path: '/settings/system-health',
            implemented: true,
            mono: 'Sh',
          },
          {
            labelKey: 'settings.pollers',
            descKey: 'descriptions.settingsPollers',
            path: '/settings/pollers',
            implemented: true,
            mono: 'Po',
          },
          {
            labelKey: 'settings.forwarding',
            descKey: 'descriptions.settingsForwarding',
            path: '/settings/forwarding',
            implemented: true,
            mono: 'Fw',
          },
          {
            labelKey: 'settings.integrations',
            path: '/settings/integrations',
            implemented: true,
            mono: 'In',
          },
          {
            labelKey: 'settings.ai',
            descKey: 'descriptions.settingsAi',
            path: '/settings/ai',
            implemented: true,
            mono: 'Ai',
          },
          {
            labelKey: 'settings.system',
            descKey: 'descriptions.settingsSystem',
            path: '/settings/system',
            implemented: true,
            mono: 'Sy',
          },
          { labelKey: 'settings.tls', path: '/settings/tls', implemented: true, mono: 'Tl' },
          {
            labelKey: 'settings.configBundle',
            descKey: 'descriptions.settingsConfigBundle',
            path: '/settings/config-bundle',
            implemented: true,
            mono: 'Cb',
          },
          {
            labelKey: 'settings.upgrade',
            descKey: 'descriptions.settingsUpgrade',
            path: '/settings/upgrade',
            implemented: true,
            mono: 'Up',
          },
          // About sits at the end of System, not in Personal. It describes the deployment (build,
          // licence, links), which is the same subject as everything above it; `Personal` means
          // "settings that affect only me", and a version number is not one (ADR-055 決定 6).
          { labelKey: 'settings.about', path: '/settings/about', implemented: true, mono: 'Ab' },
        ],
      },
      {
        labelKey: 'groups.access',
        items: [
          {
            labelKey: 'settings.credentials',
            descKey: 'descriptions.settingsCredentials',
            path: '/settings/credentials',
            implemented: true,
            mono: 'Cr',
          },
          { labelKey: 'settings.users', path: '/settings/users', implemented: true, mono: 'Us' },
          { labelKey: 'settings.roles', path: '/settings/roles', implemented: true, mono: 'Rl' },
          { labelKey: 'settings.auth', path: '/settings/auth', implemented: true, mono: 'Au' },
          {
            labelKey: 'settings.apiTokens',
            descKey: 'descriptions.settingsApiTokens',
            path: '/settings/api-tokens',
            implemented: true,
            mono: 'Tk',
          },
          { labelKey: 'settings.audit', path: '/settings/audit', implemented: true, mono: 'Ad' },
        ],
      },
      {
        // One item, and the group header stays. Folding Preferences into System would put a
        // theme switch beside fleet-wide defaults and make it look like it changed them for
        // everyone — the line between "affects only me" and "affects the deployment" is the whole
        // reason this group exists (ADR-055 決定 6).
        labelKey: 'groups.personal',
        items: [
          {
            labelKey: 'settings.preferences',
            path: '/settings/preferences',
            implemented: true,
            mono: 'Pf',
          },
        ],
      },
    ],
  },
];

/** All items of a section, flattened across its groups (ComingSoon path lookup, tests). */
export const sectionItems = (s: NavSection): NavItem[] => s.groups.flatMap((g) => g.items);

/** A group ready for sidebar rendering. `comingSoon` marks the trailing group that collects every
 *  unimplemented item so placeholders sit together at the bottom, out of the working flow. */
export interface SidebarGroup {
  labelKey?: string;
  items: NavItem[];
  comingSoon?: boolean;
}

/** Render-ready sidebar groups: each semantic group minus its unimplemented items, followed by one
 *  trailing "Coming soon" group gathering all of them (omitted when there are none). Pure — the
 *  SideBar maps straight over the result and nav.test.ts asserts nothing is lost or duplicated. */
export function sidebarGroups(s: NavSection): SidebarGroup[] {
  const out: SidebarGroup[] = [];
  const soon: NavItem[] = [];
  for (const g of s.groups) {
    const live = g.items.filter((i) => i.implemented);
    for (const i of g.items) if (!i.implemented) soon.push(i);
    if (live.length > 0) out.push({ labelKey: g.labelKey, items: live });
  }
  if (soon.length > 0) out.push({ labelKey: 'groups.comingSoon', items: soon, comingSoon: true });
  return out;
}

/** The section that owns a given pathname (longest section-prefix match; defaults to the
 *  first section). Used to light the active top tab and pick the sidebar's item set. */
export function sectionForPath(pathname: string): NavSection {
  // Node detail (/nodes/:id) belongs to the Nodes section.
  const ranked = [...NAV].sort((a, b) => b.path.length - a.path.length);
  for (const s of ranked) {
    const base = '/' + s.key;
    if (pathname === base || pathname.startsWith(base + '/') || pathname === s.path) return s;
  }
  return NAV[0];
}
