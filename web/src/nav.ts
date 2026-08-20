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
  /**
   * i18n key into `descriptions.*` — the one line under the label saying what the screen is for.
   *
   * **Required, and that is ADR-055's G2** (R3: every menu item explains itself). It was optional
   * and half the menu had gone without: 21 of 42 items, and the missing half included the pairs
   * most easily confused with each other. The ADR proposed a test asserting every item has one;
   * a required field is the same guarantee enforced a step earlier and with no exemption list to
   * maintain, which is what `extensibility.md` §1 says to prefer. `nav.test.ts` still checks the
   * key *resolves*, because the compiler cannot know whether the string exists.
   */
  descKey: string;
  /** Absolute route path. */
  path: string;
  /** Has a real backend today. false ⇒ ComingSoon placeholder. */
  implemented: boolean;
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
          {
            labelKey: 'dashboard.shared',
            descKey: 'descriptions.dashboardShared',
            path: '/dashboard',
            implemented: true,
          },
          {
            labelKey: 'dashboard.my',
            descKey: 'descriptions.dashboardMy',
            path: '/dashboard/my',
            implemented: true,
          },
          {
            labelKey: 'dashboard.reports',
            descKey: 'descriptions.dashboardReports',
            path: '/dashboard/reports',
            implemented: true,
          },
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
          {
            labelKey: 'nodes.all',
            descKey: 'descriptions.nodesAll',
            path: '/nodes',
            implemented: true,
          },
          {
            labelKey: 'nodes.discovery',
            descKey: 'descriptions.nodesDiscovery',
            path: '/nodes/discovery',
            implemented: true,
          },
        ],
      },
      {
        // ⚠️ ORDER IS THE ORDER OF THE WORK (ADR-055 R7): what to read → the bundle that names it →
        // the profile that attaches the bundle → the rule that assigns the profile. It ran the
        // other way and the first item's own description said "Attach Metric sets here", which is
        // two entries further down — the menu was telling the reader to start at the end.
        labelKey: 'groups.monitoringConfig',
        items: [
          // The keys for reaching monitored devices. They lived under Settings ▸ Access next to
          // Users, Roles and Authentication — which is how a person signs in to Yagra, a different
          // subject with a different blast radius (ADR-055 決定 4 / R8). A device profile is
          // defined as "which sets, how often, WITH WHICH CREDENTIAL", so this is a part of
          // collection, and it comes first because nothing below it can be tried without it.
          {
            labelKey: 'nodes.credentials',
            descKey: 'descriptions.nodesCredentials',
            path: '/nodes/credentials',
            implemented: true,
          },
          {
            labelKey: 'nodes.mib',
            descKey: 'descriptions.nodesMib',
            path: '/nodes/mib',
            implemented: true,
          },
          {
            labelKey: 'nodes.metricSets',
            descKey: 'descriptions.nodesMetricSets',
            path: '/nodes/collection-templates',
            implemented: true,
          },
          {
            labelKey: 'nodes.profiles',
            descKey: 'descriptions.nodesProfiles',
            path: '/nodes/profiles',
            implemented: true,
          },
          {
            labelKey: 'nodes.classificationRules',
            descKey: 'descriptions.nodesClassificationRules',
            path: '/nodes/classification-rules',
            implemented: true,
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
          },
          {
            labelKey: 'topology.dependency',
            descKey: 'descriptions.topologyDependency',
            path: '/topology/dependency',
            implemented: true,
          },
          {
            labelKey: 'topology.geo',
            descKey: 'descriptions.topologyGeo',
            path: '/topology/geo',
            implemented: true,
          },
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
          {
            labelKey: 'alerts.active',
            descKey: 'descriptions.alertsActive',
            path: '/alerts',
            implemented: true,
          },
          {
            labelKey: 'alerts.history',
            descKey: 'descriptions.alertsHistory',
            path: '/alerts/history',
            implemented: true,
          },
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
          },
          // Event alert rules stays under Alerts, not Events. It is a thing that PRODUCES alerts,
          // and splitting the two rule screens across two tabs would be the same mistake this
          // increment is fixing, one level up (ADR-055「採らなかった案」).
          //
          // ⚠️ It sits directly under Metric alert rules, and the pairing is the point: the two
          // ways an alert can come into existence, side by side, named the same way. Routing used
          // to be wedged between them, which broke the pair and put "where alerts go" before the
          // second way of making one (R7 — the order is the order of the work).
          {
            labelKey: 'alerts.eventRules',
            descKey: 'descriptions.alertsEventRules',
            path: '/alerts/event-rules',
            implemented: true,
          },
          {
            labelKey: 'alerts.routing',
            descKey: 'descriptions.alertsRouting',
            path: '/alerts/routing',
            implemented: true,
          },
          {
            labelKey: 'alerts.maintenance',
            descKey: 'descriptions.alertsMaintenance',
            path: '/alerts/maintenance',
            implemented: true,
          },
          {
            labelKey: 'alerts.mutes',
            descKey: 'descriptions.alertsMutes',
            path: '/alerts/mutes',
            implemented: true,
          },
        ],
      },
    ],
  },
  {
    // Passive monitoring gets its own tab (ADR-055 決定 2). Its pipeline used to be split across
    // two: receiving and reading were under Alerts, relaying was under Settings ▸ Forwarding, and
    // nothing named the whole. The URLs had to move with it — `sectionForPath` matches on
    // `'/' + section.key`, so a screen at `/alerts/events` cannot light an `Events` tab. Old
    // addresses redirect (`MovedTo`, which keeps the query string).
    //
    // ⚠️ The name does not cover flow, which also arrives on its own and which Forwarding also
    // relays. It fits today's screens — there is no flow LIST, only the node's Flow tab and the
    // widgets — and it is chosen knowing that building one is the moment to revisit the name.
    key: 'events',
    labelKey: 'sections.events',
    path: '/events',
    groups: [
      {
        items: [
          {
            labelKey: 'events.all',
            descKey: 'descriptions.eventsAll',
            path: '/events',
            implemented: true,
          },
          {
            labelKey: 'events.webhooks',
            descKey: 'descriptions.eventsWebhooks',
            path: '/events/webhooks',
            implemented: true,
          },
          {
            labelKey: 'events.forwarding',
            descKey: 'descriptions.eventsForwarding',
            path: '/events/forwarding',
            implemented: true,
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
          {
            labelKey: 'troubleshoot.all',
            descKey: 'descriptions.troubleshootAll',
            path: '/troubleshoot',
            implemented: true,
          },
          {
            labelKey: 'troubleshoot.runs',
            descKey: 'descriptions.troubleshootRuns',
            path: '/troubleshoot/runs',
            implemented: true,
            liveBadge: 'troubleshoot-runs',
          },
          {
            labelKey: 'troubleshoot.scheduled',
            descKey: 'descriptions.troubleshootScheduled',
            path: '/troubleshoot/scheduled',
            implemented: true,
          },
          {
            labelKey: 'troubleshoot.findings',
            descKey: 'descriptions.troubleshootFindings',
            path: '/troubleshoot/findings',
            implemented: true,
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
          },
          {
            labelKey: 'settings.pollers',
            descKey: 'descriptions.settingsPollers',
            path: '/settings/pollers',
            implemented: true,
          },
          {
            labelKey: 'settings.integrations',
            descKey: 'descriptions.settingsIntegrations',
            path: '/settings/integrations',
            implemented: true,
          },
          {
            labelKey: 'settings.ai',
            descKey: 'descriptions.settingsAi',
            path: '/settings/ai',
            implemented: true,
          },
          {
            labelKey: 'settings.system',
            descKey: 'descriptions.settingsSystem',
            path: '/settings/system',
            implemented: true,
          },
          {
            labelKey: 'settings.tls',
            descKey: 'descriptions.settingsTls',
            path: '/settings/tls',
            implemented: true,
          },
          {
            labelKey: 'settings.configBundle',
            descKey: 'descriptions.settingsConfigBundle',
            path: '/settings/config-bundle',
            implemented: true,
          },
          // Split out of Yagra health by ADR-055 Inc.6: that page is read-only self-monitoring
          // anyone with View may open, this is an Admin-only write-out of the deployment's state.
          {
            labelKey: 'settings.supportBundle',
            descKey: 'descriptions.settingsSupportBundle',
            path: '/settings/support-bundle',
            implemented: true,
          },
          {
            labelKey: 'settings.upgrade',
            descKey: 'descriptions.settingsUpgrade',
            path: '/settings/upgrade',
            implemented: true,
          },
          // About sits at the end of System, not in Personal. It describes the deployment (build,
          // licence, links), which is the same subject as everything above it; `Personal` means
          // "settings that affect only me", and a version number is not one (ADR-055 決定 6).
          {
            labelKey: 'settings.about',
            descKey: 'descriptions.settingsAbout',
            path: '/settings/about',
            implemented: true,
          },
        ],
      },
      {
        // Who may sign in to Yagra and what they may do. Device credentials are NOT this — they
        // moved to Nodes ▸ Monitoring setup (ADR-055 決定 4).
        labelKey: 'groups.access',
        items: [
          {
            labelKey: 'settings.users',
            descKey: 'descriptions.settingsUsers',
            path: '/settings/users',
            implemented: true,
          },
          {
            labelKey: 'settings.roles',
            descKey: 'descriptions.settingsRoles',
            path: '/settings/roles',
            implemented: true,
          },
          {
            labelKey: 'settings.auth',
            descKey: 'descriptions.settingsAuth',
            path: '/settings/auth',
            implemented: true,
          },
          {
            labelKey: 'settings.apiTokens',
            descKey: 'descriptions.settingsApiTokens',
            path: '/settings/api-tokens',
            implemented: true,
          },
          {
            labelKey: 'settings.audit',
            descKey: 'descriptions.settingsAudit',
            path: '/settings/audit',
            implemented: true,
          },
        ],
      },
      // There is no `Personal` group. It held Preferences alone, and ADR-055 Inc.7 moved that into
      // the account badge — a shelf that is only the signed-in person's by construction, which is
      // the line 決定 6 wanted the group header to draw. The header was the weaker way to draw it.
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
