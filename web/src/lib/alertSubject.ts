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

import type { components } from '../api/schema';

/** Which kind of thing an alert is about — generated from the backend's `SubjectKind`, so a kind
 *  added there stops this file compiling until {@link alertSubject} decides what it is. */
export type SubjectKind = components['schemas']['SubjectKind'];

/** Prefix of a Meraki organization's flat subject form (`yagra-alert`'s `MERAKI_ORG_PREFIX`). */
const MERAKI_ORG_PREFIX = 'meraki_org:';

/** The subject fields every alert-bearing response and SSE frame carries. */
export interface HasSubject {
  /** Flat subject form: a node UUID, `pool:<name>`, or `meraki_org:<id>`. */
  node?: string | null;
  subject_kind?: SubjectKind;
  subject_name?: string | null;
}

/** A subject resolved into the shapes a UI can render. */
export type AlertSubject =
  | { kind: 'node'; nodeId: string }
  /** `name` is the pool's name — already human-readable, so nothing needs resolving. */
  | { kind: 'pool'; name: string }
  /** A Cisco Meraki organization the Dashboard API is not answering (ADR-164 決定 18). `orgId` is
   *  the organization's row id — what its settings page is addressed by. `name` is `null` for an
   *  organization the server could not name yet (added since its last config generation). */
  | { kind: 'meraki_org'; orgId: string; name: string | null };

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
  const kind: SubjectKind = a.subject_kind ?? 'node';
  // A switch with a `never` default, not an if-chain: the if-chain let a third kind fall through
  // to "node" without a word from the compiler, and an `EntityName` handed `meraki_org:<id>`
  // renders it as a node that cannot be found.
  switch (kind) {
    case 'node':
      return { kind: 'node', nodeId: a.node ?? '' };
    case 'pool':
      return { kind: 'pool', name: a.subject_name ?? '?' };
    case 'meraki_org': {
      const flat = a.node ?? '';
      return {
        kind: 'meraki_org',
        orgId: flat.startsWith(MERAKI_ORG_PREFIX) ? flat.slice(MERAKI_ORG_PREFIX.length) : flat,
        name: a.subject_name ?? null,
      };
    }
    default: {
      // A kind a newer core wrote, reaching an older bundle mid-upgrade. It is not a node — but
      // every such frame so far has been one, and a row that renders is better than none.
      const unknown: never = kind;
      void unknown;
      return { kind: 'node', nodeId: a.node ?? '' };
    }
  }
}

/** What to call a subject in plain text — a search haystack, a sort key — when it is not a node.
 *  A node is named through the inventory, which this module cannot see. */
export function subjectText(subject: Exclude<AlertSubject, { kind: 'node' }>): string {
  switch (subject.kind) {
    case 'pool':
      return subject.name;
    case 'meraki_org':
      return subject.name ?? subject.orgId;
    default: {
      const unknown: never = subject;
      return unknown;
    }
  }
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

/** How an alert's `root_cause` should read to an operator. */
export type RootCause =
  | { kind: 'none' }
  /** Part of this alert's *own* node's outage — there is no other node to name (ADR-087). */
  | { kind: 'self' }
  /** Rolled up under a different node that is down. */
  | { kind: 'upstream'; nodeId: string };

/**
 * Read an alert's `root_cause` the way the UI must render it.
 *
 * 🚨 **`root_cause` can point at the alert's own node.** ADR-087 widened it from "an *upstream*
 * node" to "the node whose outage this alert is part of": when a device falls over, its `snmp_up`
 * alert is attributed to the device itself so that one outage opens one incident instead of two.
 * Rendering that with the arrow every other case uses produces `sim-panos ← sim-panos`, which
 * reads as a bug even though it is the correct answer — so the two cases need different words,
 * and the branch lives here rather than in the components because `.tsx` tests are never run
 * (`testing.md`).
 */
export function rootCause(a: HasSubject & { root_cause?: string | null }): RootCause {
  if (!a.root_cause) return { kind: 'none' };
  return a.root_cause === subjectNodeId(a)
    ? { kind: 'self' }
    : { kind: 'upstream', nodeId: a.root_cause };
}

/**
 * A React key for one row of the active-alert list.
 *
 * ⚠️ All three parts are needed. One node can be alerting on several checks at once, and the same
 * check can be present at two severities during a transition — a key that dropped either would
 * make React reuse one row's DOM for another alert, which on a virtualized list shows as a row
 * that keeps the previous alert's expanded state.
 */
export const alertRowKey = (a: { node: string; check: string; severity: string }) =>
  `${a.node}|${a.check}|${a.severity}`;
