// Pure layout helpers for My Dashboard — no React, no I/O, no registry import (the store
// injects registry-derived predicates), so they're trivially unit-testable in the node env.
// Every mutator returns a new array (immutable update for Zustand).

import type { DashboardLayout, Span, WidgetInstance, WidgetSettings } from './types';

/** Current layout schema version. Bump when the persisted shape changes; `sanitizeLayout`
 *  then migrates/drops anything it no longer understands. */
export const DASHBOARD_VERSION = 1;

/** Registry-derived facts the pure helpers need, injected so this file stays dependency-free. */
export interface RegistryView {
  /** Is this widget type still in the catalog? (Unknown types are dropped on load.) */
  isKnownType: (type: string) => boolean;
  /** The spans a type allows (for clamping a persisted/edited span). */
  allowedSpansFor: (type: string) => Span[];
  /** The span to use when a persisted one is invalid. */
  defaultSpanFor: (type: string) => Span;
}

/** Snap `span` to the nearest allowed value for a type (defaulting if the list is empty). */
export function clampSpan(type: string, span: number, reg: RegistryView): Span {
  const allowed = reg.allowedSpansFor(type);
  if (allowed.length === 0) return reg.defaultSpanFor(type);
  if (allowed.includes(span as Span)) return span as Span;
  // Pick the closest allowed span so an out-of-range value degrades gracefully.
  return allowed.reduce((best, s) =>
    Math.abs(s - span) < Math.abs(best - span) ? s : best,
  );
}

/** Normalize an untrusted/old layout document: keep only known widget types, clamp spans,
 *  repair missing/duplicate instanceIds, and drop malformed entries. Order is preserved. A
 *  non-object or missing `widgets` yields an empty board (version-stamped). */
export function sanitizeLayout(raw: unknown, reg: RegistryView): DashboardLayout {
  const widgets: WidgetInstance[] = [];
  const seen = new Set<string>();
  const rawWidgets =
    raw && typeof raw === 'object' && Array.isArray((raw as { widgets?: unknown }).widgets)
      ? ((raw as { widgets: unknown[] }).widgets)
      : [];
  for (const entry of rawWidgets) {
    if (!entry || typeof entry !== 'object') continue;
    const e = entry as Partial<WidgetInstance>;
    if (typeof e.type !== 'string' || !reg.isKnownType(e.type)) continue;
    // A missing or colliding id is repaired so React keys and edits stay stable. On collision,
    // append an incrementing counter (`base-1`, `base-2`, …) rather than re-suffixing the same
    // length each pass, which would build absurd ids like `id-3-3-3`.
    let id =
      typeof e.instanceId === 'string' && e.instanceId ? e.instanceId : `${e.type}-${widgets.length}`;
    if (seen.has(id)) {
      const base = id;
      let n = 1;
      do {
        id = `${base}-${n++}`;
      } while (seen.has(id));
    }
    seen.add(id);
    widgets.push({
      instanceId: id,
      type: e.type,
      span: clampSpan(e.type, typeof e.span === 'number' ? e.span : reg.defaultSpanFor(e.type), reg),
      settings:
        e.settings && typeof e.settings === 'object' ? (e.settings as WidgetSettings) : undefined,
    });
  }
  return { version: DASHBOARD_VERSION, widgets };
}

/** Append a fully-formed instance. */
export function addInstance(widgets: WidgetInstance[], instance: WidgetInstance): WidgetInstance[] {
  return [...widgets, instance];
}

/** Remove the instance with `instanceId` (no-op if absent). */
export function removeInstance(widgets: WidgetInstance[], instanceId: string): WidgetInstance[] {
  return widgets.filter((w) => w.instanceId !== instanceId);
}

/** Move the item at `from` to `to` (clamped); returns a new array. */
export function moveItem(widgets: WidgetInstance[], from: number, to: number): WidgetInstance[] {
  if (from < 0 || from >= widgets.length) return widgets;
  const next = [...widgets];
  const [item] = next.splice(from, 1);
  const dest = Math.max(0, Math.min(to, next.length));
  next.splice(dest, 0, item);
  return next;
}

/** Reorder to match `orderedIds` (ids not present are dropped; unknown ids ignored). Used by the
 *  drag-end handler, which hands back the new instanceId order. */
export function reorderByIds(widgets: WidgetInstance[], orderedIds: string[]): WidgetInstance[] {
  const byId = new Map(widgets.map((w) => [w.instanceId, w]));
  const out: WidgetInstance[] = [];
  for (const id of orderedIds) {
    const w = byId.get(id);
    if (w) {
      out.push(w);
      byId.delete(id);
    }
  }
  // Anything the order list missed keeps its relative position at the end (defensive).
  for (const w of widgets) if (byId.has(w.instanceId)) out.push(w);
  return out;
}

/** Set the span of one instance, clamped to what its type allows. */
export function setSpanById(
  widgets: WidgetInstance[],
  instanceId: string,
  span: number,
  reg: RegistryView,
): WidgetInstance[] {
  return widgets.map((w) =>
    w.instanceId === instanceId ? { ...w, span: clampSpan(w.type, span, reg) } : w,
  );
}

/** Merge a settings patch into one instance. */
export function setSettingsById(
  widgets: WidgetInstance[],
  instanceId: string,
  patch: WidgetSettings,
): WidgetInstance[] {
  return widgets.map((w) =>
    w.instanceId === instanceId ? { ...w, settings: { ...w.settings, ...patch } } : w,
  );
}

/** How many instances of `type` are present (for `maxInstances` enforcement). */
export function countOfType(widgets: WidgetInstance[], type: string): number {
  return widgets.reduce((n, w) => (w.type === type ? n + 1 : n), 0);
}
