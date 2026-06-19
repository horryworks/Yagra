// Pure layout helpers for My Dashboard — no React, no I/O, no registry import (the store
// injects registry-derived predicates), so they're trivially unit-testable in the node env.
// Every mutator returns a new array (immutable update for Zustand).

import type { Board, DashboardLayout, Span, WidgetInstance, WidgetSettings } from './types';

/** Current layout schema version. Bump when the persisted shape changes; `sanitizeLayout`
 *  then migrates/drops anything it no longer understands. v2 introduced multiple boards
 *  (`{ boards }`); v1 was a flat `{ widgets }`. */
export const DASHBOARD_VERSION = 2;

/** Id/name used for the first board when migrating a v1 doc or repairing an empty one. */
const FALLBACK_BOARD_ID = 'board-1';
const FALLBACK_BOARD_NAME = 'Dashboard 1';

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

/** Normalize an untrusted/old *widget array*: keep only known widget types, clamp spans, repair
 *  missing/duplicate instanceIds, and drop malformed entries. Order is preserved. Extracted so
 *  both the v2 board path and the v1 single-board migration reuse it. */
export function sanitizeWidgets(rawWidgets: unknown, reg: RegistryView): WidgetInstance[] {
  const widgets: WidgetInstance[] = [];
  const seen = new Set<string>();
  const list = Array.isArray(rawWidgets) ? rawWidgets : [];
  for (const entry of list) {
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
  return widgets;
}

/** A single empty board (used to repair a doc that has no valid board). */
function emptyBoard(): Board {
  return { id: FALLBACK_BOARD_ID, name: FALLBACK_BOARD_NAME, widgets: [] };
}

/** Normalize an untrusted/old document into a v2 multi-board layout. Sanitizes each board's
 *  widgets, repairs missing/duplicate board ids, defaults blank names, and guarantees ≥1 board.
 *  Migrates a v1 doc (a flat `{ widgets }`) by wrapping its widgets in a single board. An
 *  unrecognized shape yields a single empty board (version-stamped). */
export function sanitizeLayout(raw: unknown, reg: RegistryView): DashboardLayout {
  const obj = raw && typeof raw === 'object' ? (raw as Record<string, unknown>) : undefined;

  // v2: a `boards` array.
  if (obj && Array.isArray(obj.boards)) {
    const boards: Board[] = [];
    const seenIds = new Set<string>();
    for (const entry of obj.boards) {
      if (!entry || typeof entry !== 'object') continue;
      const b = entry as Partial<Board>;
      let id = typeof b.id === 'string' && b.id ? b.id : `board-${boards.length + 1}`;
      if (seenIds.has(id)) {
        const base = id;
        let n = 1;
        do {
          id = `${base}-${n++}`;
        } while (seenIds.has(id));
      }
      seenIds.add(id);
      const name =
        typeof b.name === 'string' && b.name.trim() ? b.name : `Dashboard ${boards.length + 1}`;
      boards.push({ id, name, widgets: sanitizeWidgets(b.widgets, reg) });
    }
    if (boards.length === 0) boards.push(emptyBoard());
    return { version: DASHBOARD_VERSION, boards };
  }

  // v1: a flat `widgets` array ⇒ migrate into one board (an empty array stays an empty board,
  // so a user who cleared their old board keeps it cleared rather than getting the default back).
  if (obj && Array.isArray(obj.widgets)) {
    return {
      version: DASHBOARD_VERSION,
      boards: [
        { id: FALLBACK_BOARD_ID, name: FALLBACK_BOARD_NAME, widgets: sanitizeWidgets(obj.widgets, reg) },
      ],
    };
  }

  // Unrecognized ⇒ a single empty board.
  return { version: DASHBOARD_VERSION, boards: [emptyBoard()] };
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

// ── Board-level helpers (multi-board My Dashboard) ───────────────────────────
// Each returns a new boards array (immutable update for Zustand).

/** Append a board. */
export function addBoard(boards: Board[], board: Board): Board[] {
  return [...boards, board];
}

/** Remove the board with `id` — but never leave zero boards (no-op if it would, or if absent). */
export function removeBoard(boards: Board[], id: string): Board[] {
  if (boards.length <= 1) return boards;
  const next = boards.filter((b) => b.id !== id);
  return next.length === boards.length ? boards : next;
}

/** Rename one board (no-op if `id` is absent). */
export function renameBoard(boards: Board[], id: string, name: string): Board[] {
  return boards.map((b) => (b.id === id ? { ...b, name } : b));
}

/** Replace one board's widget list (no-op if `id` is absent). */
export function setBoardWidgets(boards: Board[], id: string, widgets: WidgetInstance[]): Board[] {
  return boards.map((b) => (b.id === id ? { ...b, widgets } : b));
}
