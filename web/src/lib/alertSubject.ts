// SPDX-License-Identifier: AGPL-3.0-only
// What an alert is *about*, for every surface that renders one (ADR-009 Increment 2).
//
// Almost every alert is about a monitored node. Yagra also alerts on its own polling coverage — a
// poller pool that still has nodes but no live poller means those nodes have silently stopped being
// monitored — and that has no node to resolve a name from.
//
// The wire keeps the field called `node` and it is always a string: a node's UUID, or `pool:<name>`.
// `subject_kind` is what says which. Reading `node` without checking the kind is the mistake this
// module exists to make impossible — an `EntityName` handed `pool:tokyo` renders it as an
// unresolvable id, which reads to an operator as a broken row rather than as Yagra's own outage.
//
// Lives in `.ts` rather than beside the components on purpose: `.tsx` tests are never executed by
// this repo's Vitest config, so the judgement goes where a test can reach it (`testing.md`).

/** The subject fields every alert-bearing response and SSE frame carries. */
export interface HasSubject {
  /** Flat subject form: a node UUID, or `pool:<name>`. */
  node?: string | null;
  subject_kind?: 'node' | 'pool';
  subject_name?: string | null;
}

/** A subject resolved into the two shapes a UI can render. */
export type AlertSubject =
  | { kind: 'node'; nodeId: string }
  /** `name` is the pool's name — already human-readable, so nothing needs resolving. */
  | { kind: 'pool'; name: string };

/**
 * Resolve an alert or history row's subject.
 *
 * `subject_kind` is absent only from a frame produced before it existed, which could still be in
 * flight from an older core during a rolling upgrade; those were all node alerts, so that is the
 * default. A `pool` row with no name would be a contradiction the backend cannot produce — it is
 * reported as `?` rather than silently rendered as a node, because a made-up node id is the one
 * answer an operator would act on.
 */
export function alertSubject(a: HasSubject): AlertSubject {
  if (a.subject_kind === 'pool') {
    return { kind: 'pool', name: a.subject_name ?? '?' };
  }
  return { kind: 'node', nodeId: a.node ?? '' };
}

/**
 * The node this alert is about, or `null` when it is about something else.
 *
 * `null` is also the gate for the per-alert node actions (Mute, Explain): both are node-scoped
 * server-side — a mute names a node and RCA takes a node id — so offering them on a pool alert
 * would render a control that can only fail, and `ui-conventions` treats a permanently broken
 * affordance as a promise the UI cannot keep. Callers need the id anyway, so they branch on this
 * rather than on a separate boolean.
 */
export function subjectNodeId(a: HasSubject): string | null {
  const s = alertSubject(a);
  return s.kind === 'node' ? s.nodeId : null;
}
