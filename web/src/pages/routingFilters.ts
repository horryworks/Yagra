// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the two tables on Alerts ▸ Routing show — notification channels, and the rules that
// point at them.
//
// Client-side: both lists are bounded by what an operator configured, not by fleet size
// (ui-conventions, "scale-aware lists"). In a `.ts` so a test can reach the judgement — Vitest
// never executes a `.tsx` (testing.md).

import { isFiltered as isFilteredAgainst, matchesEnabled, textMatch } from '../lib/filterQuery';
import type { ChannelKind, NotificationChannel, RoutingRule, Severity } from '../types/api';
import type { EnabledState } from '../lib/filterQuery';

export interface ChannelFilters {
  kind: ChannelKind | '';
  enabled: EnabledState | '';
  /** Free text over the channel's name. */
  q: string;
}

export const DEFAULT_CHANNEL_FILTERS: ChannelFilters = { kind: '', enabled: '', q: '' };

export function matchesChannel(c: NotificationChannel, f: ChannelFilters): boolean {
  if (f.kind && c.kind !== f.kind) return false;
  if (!matchesEnabled(f.enabled, c.enabled)) return false;
  return textMatch(f.q, c.name);
}

export function isChannelFiltered(f: ChannelFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_CHANNEL_FILTERS);
}

export interface RoutingFilters {
  /** `''` is "any severity"; a rule with no severity of its own routes every severity. */
  severity: Severity | '';
  enabled: EnabledState | '';
  /** Free text over the rule's name. */
  q: string;
}

export const DEFAULT_ROUTING_FILTERS: RoutingFilters = { severity: '', enabled: '', q: '' };

/**
 * Whether one routing rule survives the filter.
 *
 * ⚠️ A rule whose own `severity` is null routes **every** severity, so it must match a severity
 * filter rather than be hidden by it. Treating null as "no severity" would answer the operator's
 * question — "what would page me for a critical?" — by omitting the rules that always do.
 */
export function matchesRoutingRule(r: RoutingRule, f: RoutingFilters): boolean {
  if (f.severity && r.severity != null && r.severity !== f.severity) return false;
  if (!matchesEnabled(f.enabled, r.enabled)) return false;
  return textMatch(f.q, r.name);
}

export function isRoutingFiltered(f: RoutingFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_ROUTING_FILTERS);
}
