// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Alert rules ▸ Events: what an event rule targets, and which rules the target tabs show
// (ADR-232). Shaped like the threshold tab's view (`thresholdQuery.ts`) so the two tabs read the
// same, but filtered in the browser: event rules are a short list served whole, with no cap.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import type { EventRule, EventRuleInput } from '../types/api';

/** What a rule targets. A rule names at most one of a node and a folder; neither is every node. */
export const EVENT_TARGET_KINDS = ['global', 'folder', 'node'] as const;
export type EventTargetKind = (typeof EVENT_TARGET_KINDS)[number];

/** The target tabs, broadest first, after the tab that shows them all. */
export const EVENT_TARGET_TABS = ['all', ...EVENT_TARGET_KINDS] as const;
export type EventTargetTab = (typeof EVENT_TARGET_TABS)[number];

/** The query keys the view owns. The filter columns own the rest; none of theirs is either. */
export const EVENT_VIEW_KEYS = { tab: 'target_level', target: 'target' } as const;

export interface EventRuleView {
  tab: EventTargetTab;
  /** A folder or node id, kept only on the tab that names that kind of thing. */
  target: string | null;
}

export function eventTargetOf(r: Pick<EventRule, 'node_id' | 'group_id'>): EventTargetKind {
  if (r.node_id) return 'node';
  if (r.group_id) return 'folder';
  return 'global';
}

/** The id a rule's target names, or `null` for a rule on every node. */
export function eventTargetId(r: Pick<EventRule, 'node_id' | 'group_id'>): string | null {
  return r.node_id ?? r.group_id ?? null;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function isTab(v: string): v is EventTargetTab {
  return (EVENT_TARGET_TABS as readonly string[]).includes(v);
}

function namesThings(tab: EventTargetTab): boolean {
  return tab === 'folder' || tab === 'node';
}

/** Read the view from the query string. Anything it does not know opens **All**. */
export function eventRuleViewFrom(params: URLSearchParams): EventRuleView {
  const raw = params.get(EVENT_VIEW_KEYS.tab) ?? '';
  const tab: EventTargetTab = isTab(raw) ? raw : 'all';
  const target = params.get(EVENT_VIEW_KEYS.target);
  return { tab, target: namesThings(tab) && target && UUID.test(target) ? target : null };
}

/** Write the view into `params`, removing what it does not set so a link stays short. */
export function writeEventRuleView(params: URLSearchParams, view: EventRuleView): void {
  const set = (k: string, v: string | null) => (v ? params.set(k, v) : params.delete(k));
  set(EVENT_VIEW_KEYS.tab, view.tab === 'all' ? null : view.tab);
  set(EVENT_VIEW_KEYS.target, namesThings(view.tab) ? view.target : null);
}

/** Whether a rule belongs on the view: its target is the tab's kind, and, when one is chosen,
 *  the chosen folder or node. */
export function inEventRuleView(
  r: Pick<EventRule, 'node_id' | 'group_id'>,
  view: EventRuleView,
): boolean {
  if (view.tab === 'all') return true;
  if (eventTargetOf(r) !== view.tab) return false;
  return view.target === null || eventTargetId(r) === view.target;
}

/** The two target fields of a request body, from the dialog's choice. Exactly one of them is set
 *  for a folder or a node; the server refuses both together. */
export function targetFields(
  kind: EventTargetKind,
  id: string,
): Pick<EventRuleInput, 'node_id' | 'group_id'> {
  switch (kind) {
    case 'global':
      return { node_id: null, group_id: null };
    case 'folder':
      return { node_id: null, group_id: id || null };
    case 'node':
      return { node_id: id || null, group_id: null };
  }
}
