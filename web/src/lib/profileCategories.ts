// Device-profile functional taxonomy — mirrors yagra_common::ProfileCategory.
// `token` is the kebab-case enum form sent over the API; the array order is the display /
// grouping order used by the Device profiles page (routers → switches → … → generic).

export interface ProfileCategoryMeta {
  token: string;
  label: string;
}

export const PROFILE_CATEGORIES: ProfileCategoryMeta[] = [
  { token: 'router', label: 'Router' },
  { token: 'l3-switch', label: 'L3 switch' },
  { token: 'l2-switch', label: 'L2 switch' },
  { token: 'firewall', label: 'Firewall' },
  { token: 'wireless-controller', label: 'Wireless controller' },
  { token: 'wireless-ap', label: 'Wireless AP' },
  { token: 'load-balancer', label: 'Load balancer' },
  { token: 'server', label: 'Server' },
  { token: 'hypervisor', label: 'Hypervisor' },
  { token: 'storage', label: 'Storage / NAS' },
  { token: 'power', label: 'Power (UPS / PDU)' },
  { token: 'printer', label: 'Printer' },
  { token: 'generic-snmp', label: 'Generic SNMP' },
  { token: 'ping-only', label: 'Ping only' },
];

/** Human label for a category token (falls back to the raw token if unknown). */
export function categoryLabel(token: string): string {
  return PROFILE_CATEGORIES.find((c) => c.token === token)?.label ?? token;
}
