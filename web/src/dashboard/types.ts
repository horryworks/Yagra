// SPDX-License-Identifier: AGPL-3.0-only
// My Dashboard — shared types for the customizable widget board.
//
// A *definition* describes a kind of widget (its title, grid span, backing tag, and the React
// component that renders it). An *instance* is one placed widget on a user's board: it carries
// its own id, a span override, and optional per-instance settings (e.g. which node an RTT chart
// is pinned to). Multiple instances of one definition are allowed — the `instanceId` keeps them
// distinct. The ordered list of instances + a schema version is the persisted layout document.

import type { FC } from 'react';

/** Grid span (of a 12-column grid). A widget declares which spans it allows. */
export type Span = 4 | 6 | 8 | 12;

/** Row span (stepped widget height): 1 = standard (content height), 2/3 = taller. A widget opts
 *  into taller heights by declaring `allowedRowSpans`; otherwise its height is fixed at 1. */
export type RowSpan = 1 | 2 | 3;

/** How much backend work a widget needs today (mirrors the design handoff legend). `new`
 *  widgets are catalogued but not yet buildable, so they show disabled in the picker.
 *
 *  `as const` because the catalog modal badges each widget with `t(`catalog.backing.${backing}`)`
 *  and has no fallback — a fourth tag added without strings would put a raw key on every card in
 *  the picker, in both locales (extensibility.md §4). */
export const BACKINGS = ['live', 'rollup', 'new'] as const;
export type Backing = (typeof BACKINGS)[number];

/** Per-instance settings bag (widget-specific; e.g. `{ nodeId }` for the RTT chart). Opaque to
 *  everything but the owning widget. */
export type WidgetSettings = Record<string, unknown>;

/** Props every widget component receives. The frame owns the Card chrome (title + edit
 *  controls); the widget renders only its body and, optionally, view-mode header actions. */
export interface WidgetProps {
  /** This placed widget's instance (id, span, settings). */
  instance: WidgetInstance;
  /** Merge a patch into this instance's settings (persisted). */
  setSettings: (patch: WidgetSettings) => void;
}

/** A kind of widget available in the catalog. */
export interface WidgetDefinition {
  /** Stable identifier persisted in the layout (e.g. `status-summary`). Never renumber. */
  type: string;
  /** Card title. */
  title: string;
  /** Catalog section heading (e.g. `01 · Fleet status`). */
  section: string;
  /** One-line description shown in the catalog picker. */
  blurb: string;
  /** Backend-readiness tag. */
  backing: Backing;
  /** Span used when first added. */
  defaultSpan: Span;
  /** Spans the user may pick for this widget. */
  allowedSpans: Span[];
  /** Row heights (stepped) the user may pick. Omit ⇒ fixed height; no height control is shown. */
  allowedRowSpans?: RowSpan[];
  /** Row height when first added. Omit ⇒ 1 (standard). */
  defaultRowSpan?: RowSpan;
  /** The body renderer. */
  Component: FC<WidgetProps>;
  /** Optional view-mode header actions (e.g. a node selector or a "View all" link). */
  Actions?: FC<WidgetProps>;
  /** Cap on how many instances may be added (omit = unlimited). */
  maxInstances?: number;
}

/** One placed widget on a board. Array position is its order. */
export interface WidgetInstance {
  instanceId: string;
  type: string;
  /** Operator-given name for THIS card, shown instead of the definition's title (ADR-071).
   *
   *  On the instance rather than in `settings` on purpose: the bag below is documented as opaque to
   *  everything but the owning widget, and the title is rendered by the *frame*. Putting it there
   *  would make the frame read a bag it is told not to read, and would leave 48 widgets agreeing on
   *  a key name by convention instead of by type.
   *
   *  Absent when unnamed, so a board nobody has renamed serializes exactly as before — the same
   *  reason `rowSpan` is stripped at its default. Clearing the field is how a rename is undone;
   *  there is deliberately no separate reset. */
  title?: string;
  span: Span;
  /** Stepped height. Absent ⇒ 1 (standard). Only present when the user picked a taller size, so a
   *  standard-height widget serializes lean and old (pre-height) docs load unchanged. */
  rowSpan?: RowSpan;
  settings?: WidgetSettings;
}

/** A named board: an ordered list of widget instances. My Dashboard holds 1..N of these (the user
 *  switches/adds/removes them); the Shared Dashboard holds exactly one. */
export interface Board {
  /** Stable id (persisted; used as the React key and the active-board selector). */
  id: string;
  /** User-facing tab label. */
  name: string;
  widgets: WidgetInstance[];
}

/** The persisted dashboard document (schema v2). `version` lets the client migrate older shapes
 *  (v1 was a flat `{ widgets }`). The *active* board is an ephemeral UI selection and is
 *  deliberately not persisted in the document. */
export interface DashboardLayout {
  version: number;
  boards: Board[];
}
