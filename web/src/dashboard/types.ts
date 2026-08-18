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
 *  controls); the widget renders only its body and, optionally, its settings panel. */
export interface WidgetProps {
  /** This placed widget's instance (id, span, settings). */
  instance: WidgetInstance;
  /** Merge a patch into this instance's settings (persisted). */
  setSettings: (patch: WidgetSettings) => void;
}

/** The settings a **view-mode** control is allowed to write: how the already-chosen subject is
 *  displayed, never which subject it is (ADR-072).
 *
 *  This is a closed set on purpose. `WidgetDefinition.Actions` receives only this, so a control that
 *  picks a node, types a metric name or adds an interface **cannot compile** in the view-mode slot —
 *  excess-property checking rejects the object literal. That turns "don't put configuration in the
 *  header" from a note somebody has to remember into something the compiler asks for.
 *
 *  Adding a key here is therefore a decision: it says the thing it names is a lens on the data, not
 *  a choice of subject. The line is "does this change what the widget is about?"
 *
 *  ⚠️ It only catches literals — `setSettings(someVariable)` type-checks structurally and slips
 *  through. Every call site passes a literal today; the rendered surface is pinned by Tier1
 *  (`tests/ui/widgetCatalog.spec.ts`), which is what actually looks at the header.
 *
 *  ⚠️ A `type` alias rather than an `interface`, and that is load-bearing: only aliases get an
 *  implicit index signature, so only an alias is assignable to `WidgetSettings`
 *  (`Record<string, unknown>`). As an interface, the frame could not hand its own `setSettings` to
 *  an `Actions` component at all. Excess-property checking on literals works the same either way. */
export type ViewSettings = {
  /** Top-N window: `now` | `max_1h`. */
  agg?: string;
  /** Interface traffic: `bps` | `pps`. */
  unit?: string;
  /** Interface traffic: trailing window, in seconds. */
  rangeSecs?: number;
  /** Flow top-AS: `src` | `dst`. */
  dir?: string;
  /** Event feed: `syslog` | `trap` | `webhook`; absent means every kind. */
  kind?: string;
};

/** Props a view-mode header action receives. Same shape as {@link WidgetProps}, with the write
 *  narrowed to {@link ViewSettings} — the narrowing *is* the rule. */
export interface ViewActionProps {
  instance: WidgetInstance;
  /** Merge a view-scope patch into this instance's settings (persisted, like any other setting —
   *  ADR-072 decision 6 deliberately did not change how saving works). */
  setSettings: (patch: ViewSettings) => void;
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
  /** Optional **view-mode** header actions: a time window, a display lens, a "View all" link.
   *
   *  Not a place for configuration — the narrowed `setSettings` above says so to the compiler
   *  (ADR-072). Anything that chooses what the widget is *about* goes in {@link Settings}. */
  Actions?: FC<ViewActionProps>;
  /** Optional **Customize-mode** settings panel: what this widget is about (which node, which
   *  metric, which interfaces). The frame draws a ⚙ in the card header while the board is being
   *  edited and renders this inside its popover; a widget without one shows no ⚙. */
  Settings?: FC<WidgetProps>;
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
