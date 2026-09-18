// SPDX-License-Identifier: AGPL-3.0-only
// Which widgets may go on the **public** board (ADR-123 決定 8).
//
// The public board's widgets decide what an anonymous visitor can read, and a widget whose data
// needs more than `view` cannot work for one. An admin composing the board would never see that:
// their own session answers every call, so the widget renders perfectly right up until a stranger
// loads the page and gets a 403.
//
// 🚨 **This is a deny-list, and that direction is deliberate.** An allow-list would have to be
// extended for every new widget, and the failure of forgetting would be a widget silently missing
// from the catalog — which reads as "not built yet" and gets re-implemented. Forgetting to add an
// entry *here* means a widget that 403s for visitors, which the anonymous preview shows in one
// click. Neither failure is free; this one is visible.
//
// ⚠️ Widgets that depend on an **optional subsystem** are not listed. The flow widgets answer a
// typed 503 when the flow tier is off — for everyone, on every board, signed in or not. That is a
// deployment saying "not configured", not a permission the visitor lacks, and hiding them here
// would make the public board the only place a configured flow tier could not be shown.

import { REGISTRY } from './registry';
import type { WidgetDefinition } from './types';

/** Widget types that cannot work for an anonymous visitor, each with the reason. */
export const NOT_PUBLIC: Readonly<Record<string, string>> = {
  // `GET /api/v1/audit` takes `view_audit`, which no anonymous caller has and no public board can
  // grant — `Require<P>` refuses every permission but `View` on a public deployment, whatever the
  // route allow-list says. Placing it would show every visitor an empty card and put the audit
  // route on the derived allow-list for nothing.
  audit: 'reads the audit log, which needs the view_audit permission',
};

/** The catalog for a board — the whole registry, or the public-safe subset. */
export function catalogFor(publicOnly: boolean): WidgetDefinition[] {
  return publicOnly ? REGISTRY.filter((d) => !(d.type in NOT_PUBLIC)) : REGISTRY;
}

/** The route the live alert store is fed from. A widget that names it reads that store. */
export const ALERT_STREAM_ROUTE = 'GET /api/v1/stream/alerts';

/**
 * Whether any widget on this board reads the live alert store.
 *
 * The public board asks before it subscribes. An anonymous visitor may only reach the routes the
 * board's widgets declare, so on a board with no alert widget the stream is refused — and a
 * refused stream is retried every three seconds for as long as the page is open, by every
 * visitor. Derived from `reads`, never from a list of widget types: the declaration is what core
 * opens the route from, so asking the same field cannot disagree with it.
 */
export function boardReadsAlerts(types: readonly string[]): boolean {
  return types.some((type) =>
    REGISTRY.find((d) => d.type === type)?.reads.includes(ALERT_STREAM_ROUTE),
  );
}

/** Why this widget cannot be placed on the public board, or `undefined` if it can. */
export function whyNotPublic(type: string): string | undefined {
  return NOT_PUBLIC[type];
}
