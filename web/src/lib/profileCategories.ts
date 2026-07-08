// Device-profile functional taxonomy — mirrors yagra_common::ProfileCategory.
// `token` is the kebab-case enum form sent over the API; the array order is the display /
// grouping order used by the Device profiles page (routers → switches → … → generic).
// Labels are i18n keys (resolved via the caller's `t`) so they follow the active language.

import type { TFunction } from 'i18next';

export interface ProfileCategoryMeta {
  token: string;
  labelKey: string;
}

export const PROFILE_CATEGORIES: ProfileCategoryMeta[] = [
  { token: 'router', labelKey: 'monitoring:categories.router' },
  { token: 'l3-switch', labelKey: 'monitoring:categories.l3-switch' },
  { token: 'l2-switch', labelKey: 'monitoring:categories.l2-switch' },
  { token: 'firewall', labelKey: 'monitoring:categories.firewall' },
  { token: 'wireless-controller', labelKey: 'monitoring:categories.wireless-controller' },
  { token: 'wireless-ap', labelKey: 'monitoring:categories.wireless-ap' },
  { token: 'load-balancer', labelKey: 'monitoring:categories.load-balancer' },
  { token: 'server', labelKey: 'monitoring:categories.server' },
  { token: 'hypervisor', labelKey: 'monitoring:categories.hypervisor' },
  { token: 'storage', labelKey: 'monitoring:categories.storage' },
  { token: 'power', labelKey: 'monitoring:categories.power' },
  { token: 'printer', labelKey: 'monitoring:categories.printer' },
  { token: 'generic-snmp', labelKey: 'monitoring:categories.generic-snmp' },
  { token: 'ping-only', labelKey: 'monitoring:categories.ping-only' },
];

/** Human label for a category token (falls back to the raw token if unknown). Pass the caller's
 *  `t` so the label follows the active interface language. */
export function categoryLabel(token: string, t: TFunction): string {
  const meta = PROFILE_CATEGORIES.find((c) => c.token === token);
  return meta ? t(meta.labelKey) : token;
}
