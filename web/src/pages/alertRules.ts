// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Alert rules: the two ways an alert comes into existence, as tabs of one page (ADR-232).
// A tab is a path, as in Collection setup (ADR-231): each screen keeps its own URL filters, and the
// threshold tab is the hub's bare path, so every link into it written before stays valid.

/** The hub's path, and the threshold tab's: a menu item's path has to be a screen of its own. */
export const ALERT_RULES_PATH = '/alerts/rules';

/** Thresholds first: they are what a fresh deployment already has, and what every other screen
 *  links into (ADR-230). */
export const ALERT_RULE_TABS = ['thresholds', 'events'] as const;
export type AlertRuleTab = (typeof ALERT_RULE_TABS)[number];

export const ALERT_RULE_TAB_LABEL_KEYS: Record<AlertRuleTab, string> = {
  thresholds: 'alertsConfig:alertRules.tabs.thresholds',
  events: 'alertsConfig:alertRules.tabs.events',
};

/** How the rules on a tab combine, said once above the list rather than in prose (ADR-232
 *  decision 4): the narrowest threshold rule wins, every matching event rule applies. */
export const ALERT_RULE_HOW_KEYS: Record<AlertRuleTab, string> = {
  thresholds: 'alertsConfig:alertRules.how.thresholds',
  events: 'alertsConfig:alertRules.how.events',
};

/** Where the event tab lived before ADR-232. A `MovedTo`, so a bookmark keeps its query. */
export const EVENT_RULES_MOVED_FROM = '/alerts/event-rules';

export function alertRuleTabPath(tab: AlertRuleTab): string {
  return tab === 'thresholds' ? ALERT_RULES_PATH : `${ALERT_RULES_PATH}/${tab}`;
}

/** The tab a route is on; the hub itself and anything it does not know are the threshold tab. */
export function alertRuleTabOf(pathname: string): AlertRuleTab {
  return pathname === alertRuleTabPath('events') ||
    pathname.startsWith(alertRuleTabPath('events') + '/')
    ? 'events'
    : 'thresholds';
}
