// Navigation IA — the single source of truth for the site map (.claude/docs/design-system.md §2.2,
// decision log §6). The top bar renders SECTIONS (text-only tabs); the sidebar renders the
// selected section's ITEMS. `implemented: false` items route to the ComingSoon placeholder
// (their backend endpoints don't exist yet) but still appear so the information architecture
// stays whole and screens can be slotted in later without renumbering the nav.

export interface NavItem {
  label: string;
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
  label: string;
  /** Where the top-bar tab navigates (its first/landing child). */
  path: string;
  items: NavItem[];
}

export const NAV: NavSection[] = [
  {
    key: 'dashboard',
    label: 'Dashboard',
    path: '/dashboard',
    items: [
      { label: 'Overview', path: '/dashboard', implemented: true, mono: 'Ov' },
      { label: 'My dashboard', path: '/dashboard/my', implemented: true, mono: 'My' },
      { label: 'Templates', path: '/dashboard/templates', implemented: false, mono: 'Tp' },
      { label: 'Reports', path: '/dashboard/reports', implemented: false, mono: 'Rp' },
    ],
  },
  {
    key: 'nodes',
    label: 'Nodes',
    path: '/nodes',
    items: [
      { label: 'All nodes', path: '/nodes', implemented: true, mono: 'Al' },
      { label: 'Discovery', path: '/nodes/discovery', implemented: true, mono: 'Di' },
      { label: 'Dependencies', path: '/nodes/dependencies', implemented: false, mono: 'Dp' },
      { label: 'Device profiles', path: '/nodes/profiles', implemented: true, mono: 'Pr' },
      {
        label: 'Classification rules',
        path: '/nodes/classification-rules',
        implemented: true,
        mono: 'Cl',
      },
      {
        label: 'Collection templates',
        path: '/nodes/collection-templates',
        implemented: true,
        mono: 'Ct',
      },
      { label: 'MIB repository', path: '/nodes/mib', implemented: true, mono: 'Mb' },
    ],
  },
  {
    key: 'topology',
    label: 'Topology',
    path: '/topology/map',
    items: [
      { label: 'Network map', path: '/topology/map', implemented: false, mono: 'Nm' },
      { label: 'Dependency view', path: '/topology/dependency', implemented: false, mono: 'Dv' },
      { label: 'Geo map', path: '/topology/geo', implemented: false, mono: 'Ge' },
    ],
  },
  {
    key: 'alerts',
    label: 'Alerts',
    path: '/alerts',
    items: [
      { label: 'Active alerts', path: '/alerts', implemented: true, mono: 'Ac' },
      { label: 'History', path: '/alerts/history', implemented: true, mono: 'Hi' },
      { label: 'Rules & thresholds', path: '/alerts/rules', implemented: true, mono: 'Ru' },
      { label: 'Routing & notifications', path: '/alerts/routing', implemented: true, mono: 'Rt' },
      { label: 'Maintenance windows', path: '/alerts/maintenance', implemented: true, mono: 'Mw' },
      { label: 'Mutes', path: '/alerts/mutes', implemented: true, mono: 'Mu' },
    ],
  },
  {
    key: 'troubleshoot',
    label: 'Troubleshoot',
    path: '/troubleshoot',
    items: [
      { label: 'All tools', path: '/troubleshoot', implemented: true, mono: 'At' },
      {
        label: 'Analysis runs',
        path: '/troubleshoot/runs',
        implemented: true,
        mono: 'Ar',
        liveBadge: 'troubleshoot-runs',
      },
      { label: 'Scheduled', path: '/troubleshoot/scheduled', implemented: false, mono: 'Sc' },
      { label: 'Saved findings', path: '/troubleshoot/findings', implemented: false, mono: 'Sf' },
    ],
  },
  {
    key: 'settings',
    label: 'Settings',
    path: '/settings/credentials',
    items: [
      { label: 'Pollers', path: '/settings/pollers', implemented: false, mono: 'Po' },
      { label: 'Integrations', path: '/settings/integrations', implemented: false, mono: 'In' },
      {
        label: 'Credentials & secrets',
        path: '/settings/credentials',
        implemented: true,
        mono: 'Cr',
      },
      { label: 'Users & roles', path: '/settings/users', implemented: true, mono: 'Us' },
      { label: 'Roles & privileges', path: '/settings/roles', implemented: true, mono: 'Rl' },
      { label: 'Authentication', path: '/settings/auth', implemented: false, mono: 'Au' },
      { label: 'Audit log', path: '/settings/audit', implemented: true, mono: 'Ad' },
      {
        label: 'System settings',
        path: '/settings/system',
        implemented: true,
        mono: 'Sy',
      },
      { label: 'Preferences', path: '/settings/preferences', implemented: true, mono: 'Pf' },
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
