// SPDX-License-Identifier: AGPL-3.0-only
// Pure layout helpers for My Dashboard — no React, no I/O, no registry import (the store
// injects registry-derived predicates), so they're trivially unit-testable in the node env.
// Every mutator returns a new array (immutable update for Zustand).

import type { Board, DashboardLayout, RowSpan, Span, WidgetInstance, WidgetSettings } from './types';

/** Current layout schema version. Bump when the persisted shape changes; `sanitizeLayout`
 *  then migrates/drops anything it no longer understands. v3 added a per-widget `rowSpan`
 *  (stepped height); v2 introduced multiple boards (`{ boards }`); v1 was a flat `{ widgets }`.
 *  `rowSpan` is additive/optional, so v2 docs load unchanged (an absent rowSpan means standard). */
export const DASHBOARD_VERSION = 3;

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
  /** The row spans (heights) a type allows. A fixed-height widget returns `[1]`. */
  allowedRowSpansFor: (type: string) => RowSpan[];
  /** The row span to use when a persisted one is invalid (usually `1`). */
  defaultRowSpanFor: (type: string) => RowSpan;
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

/** Snap `rowSpan` to the nearest height the type allows (defaulting if the list is empty). Mirrors
 *  {@link clampSpan}: an out-of-range or removed-option height degrades to the closest allowed one. */
export function clampRowSpan(type: string, rowSpan: number, reg: RegistryView): RowSpan {
  const allowed = reg.allowedRowSpansFor(type);
  if (allowed.length === 0) return reg.defaultRowSpanFor(type);
  if (allowed.includes(rowSpan as RowSpan)) return rowSpan as RowSpan;
  return allowed.reduce((best, r) => (Math.abs(r - rowSpan) < Math.abs(best - rowSpan) ? r : best));
}

/** Return `w` carrying `rowSpan`, or with the field stripped when it's standard (1). Keeping the
 *  field absent for standard height means untouched/old widgets serialize exactly as before. */
function withRowSpan(w: WidgetInstance, rowSpan: RowSpan): WidgetInstance {
  if (rowSpan <= 1) {
    const next = { ...w };
    delete next.rowSpan;
    return next;
  }
  return { ...w, rowSpan };
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
    // Clamp the persisted height to what the type currently allows; a standard (1) height is kept
    // as an absent field so the serialized shape stays lean and pre-height docs round-trip cleanly.
    const rowSpan = clampRowSpan(
      e.type,
      typeof e.rowSpan === 'number' ? e.rowSpan : reg.defaultRowSpanFor(e.type),
      reg,
    );
    widgets.push({
      instanceId: id,
      type: e.type,
      span: clampSpan(e.type, typeof e.span === 'number' ? e.span : reg.defaultSpanFor(e.type), reg),
      ...(rowSpan > 1 ? { rowSpan } : {}),
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

/** Set both the width (span) and height (row span) of one instance in a single update, each clamped
 *  to what its type allows. A standard height (1) drops the `rowSpan` field (see {@link withRowSpan})
 *  so untouched/short widgets stay lean. Used by the corner drag-to-resize handle. */
export function setSizeById(
  widgets: WidgetInstance[],
  instanceId: string,
  span: number,
  rowSpan: number,
  reg: RegistryView,
): WidgetInstance[] {
  return widgets.map((w) =>
    w.instanceId === instanceId
      ? withRowSpan({ ...w, span: clampSpan(w.type, span, reg) }, clampRowSpan(w.type, rowSpan, reg))
      : w,
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
