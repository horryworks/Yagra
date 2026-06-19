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

/** How much backend work a widget needs today (mirrors the design handoff legend). `new`
 *  widgets are catalogued but not yet buildable, so they show disabled in the picker. */
export type Backing = 'live' | 'rollup' | 'new';

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
  span: Span;
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
