// Navigation IA — the single source of truth for the site map (.claude/docs/design-system.md §2.2,
// decision log §6). The top bar renders SECTIONS (text-only tabs); the sidebar renders the
// selected section's ITEMS. `implemented: false` items route to the ComingSoon placeholder
// (their backend endpoints don't exist yet) but still appear so the information architecture
// stays whole and screens can be slotted in later without renumbering the nav.
//
// Labels are i18n keys into the `nav` namespace (see locales/<lng>/nav.json), resolved with `t()`
// at render time — never store the display string here, or a language switch wouldn't re-render.

export interface NavItem {
  /** i18n key into the `nav` namespace (e.g. 'nodes.all'). Resolved via `t()` at render. */
  labelKey: string;
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

export interface NavSection {
  key: string;
  /** i18n key into the `nav` namespace (e.g. 'sections.nodes'). Resolved via `t()` at render. */
  labelKey: string;
  /** Where the top-bar tab navigates (its first/landing child). */
  path: string;
  items: NavItem[];
}

export const NAV: NavSection[] = [
  {
    key: 'dashboard',
    labelKey: 'sections.dashboard',
    path: '/dashboard',
    items: [
      { labelKey: 'dashboard.shared', path: '/dashboard', implemented: true, mono: 'Sh' },
      { labelKey: 'dashboard.my', path: '/dashboard/my', implemented: true, mono: 'My' },
      { labelKey: 'dashboard.reports', path: '/dashboard/reports', implemented: true, mono: 'Rp' },
    ],
  },
  {
    key: 'nodes',
    labelKey: 'sections.nodes',
    path: '/nodes',
    items: [
      { labelKey: 'nodes.all', path: '/nodes', implemented: true, mono: 'Al' },
      { labelKey: 'nodes.discovery', path: '/nodes/discovery', implemented: true, mono: 'Di' },
      {
        labelKey: 'nodes.dependencies',
        path: '/nodes/dependencies',
        implemented: false,
        mono: 'Dp',
      },
      { labelKey: 'nodes.profiles', path: '/nodes/profiles', implemented: true, mono: 'Pr' },
      {
        labelKey: 'nodes.classificationRules',
        path: '/nodes/classification-rules',
        implemented: true,
        mono: 'Cl',
      },
      {
        labelKey: 'nodes.collectionTemplates',
        path: '/nodes/collection-templates',
        implemented: true,
        mono: 'Ct',
      },
      { labelKey: 'nodes.mib', path: '/nodes/mib', implemented: true, mono: 'Mb' },
    ],
  },
  {
    key: 'topology',
    labelKey: 'sections.topology',
    path: '/topology/map',
    items: [
      { labelKey: 'topology.map', path: '/topology/map', implemented: true, mono: 'Nm' },
      {
        labelKey: 'topology.dependency',
        path: '/topology/dependency',
        implemented: true,
        mono: 'Dv',
      },
      { labelKey: 'topology.geo', path: '/topology/geo', implemented: false, mono: 'Ge' },
    ],
  },
  {
    key: 'alerts',
    labelKey: 'sections.alerts',
    path: '/alerts',
    items: [
      { labelKey: 'alerts.active', path: '/alerts', implemented: true, mono: 'Ac' },
      { labelKey: 'alerts.history', path: '/alerts/history', implemented: true, mono: 'Hi' },
      { labelKey: 'alerts.rules', path: '/alerts/rules', implemented: true, mono: 'Ru' },
      { labelKey: 'alerts.routing', path: '/alerts/routing', implemented: true, mono: 'Nt' },
      { labelKey: 'alerts.events', path: '/alerts/events', implemented: true, mono: 'Ev' },
      { labelKey: 'alerts.eventRules', path: '/alerts/event-rules', implemented: true, mono: 'Er' },
      {
        labelKey: 'alerts.eventSources',
        path: '/alerts/event-sources',
        implemented: true,
        mono: 'Es',
      },
      {
        labelKey: 'alerts.maintenance',
        path: '/alerts/maintenance',
        implemented: true,
        mono: 'Mw',
      },
      { labelKey: 'alerts.mutes', path: '/alerts/mutes', implemented: true, mono: 'Mu' },
    ],
  },
  {
    key: 'troubleshoot',
    labelKey: 'sections.troubleshoot',
    path: '/troubleshoot',
    items: [
      { labelKey: 'troubleshoot.all', path: '/troubleshoot', implemented: true, mono: 'At' },
      {
        labelKey: 'troubleshoot.runs',
        path: '/troubleshoot/runs',
        implemented: true,
        mono: 'Ar',
        liveBadge: 'troubleshoot-runs',
      },
      {
        labelKey: 'troubleshoot.scheduled',
        path: '/troubleshoot/scheduled',
        implemented: false,
        mono: 'Sc',
      },
      {
        labelKey: 'troubleshoot.findings',
        path: '/troubleshoot/findings',
        implemented: false,
        mono: 'Sf',
      },
    ],
  },
  {
    key: 'settings',
    labelKey: 'sections.settings',
    path: '/settings/credentials',
    items: [
      {
        labelKey: 'settings.systemHealth',
        path: '/settings/system-health',
        implemented: true,
        mono: 'Sh',
      },
      { labelKey: 'settings.pollers', path: '/settings/pollers', implemented: true, mono: 'Po' },
      {
        labelKey: 'settings.integrations',
        path: '/settings/integrations',
        implemented: true,
        mono: 'In',
      },
      {
        labelKey: 'settings.credentials',
        path: '/settings/credentials',
        implemented: true,
        mono: 'Cr',
      },
      { labelKey: 'settings.users', path: '/settings/users', implemented: true, mono: 'Us' },
      { labelKey: 'settings.roles', path: '/settings/roles', implemented: true, mono: 'Rl' },
      { labelKey: 'settings.auth', path: '/settings/auth', implemented: false, mono: 'Au' },
      { labelKey: 'settings.audit', path: '/settings/audit', implemented: true, mono: 'Ad' },
      { labelKey: 'settings.system', path: '/settings/system', implemented: true, mono: 'Sy' },
      {
        labelKey: 'settings.preferences',
        path: '/settings/preferences',
        implemented: true,
        mono: 'Pf',
      },
      { labelKey: 'settings.about', path: '/settings/about', implemented: true, mono: 'Ab' },
    ],
  },
];

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
