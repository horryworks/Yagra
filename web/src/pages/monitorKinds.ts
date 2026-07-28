// SPDX-License-Identifier: AGPL-3.0-only
// The monitor kinds an operator can create by hand, as data.
//
// This was a page-local `'device' | 'url' | 'dns'` union with nine separate branches on it in
// NodesPage — a select's options, the modal title, the error message, the "is the target field
// filled" check, and the create call. Only the last is genuinely per-kind code; the rest were
// ternary chains that a fourth kind has to be threaded through by hand, in a file of 1,200 lines,
// with nothing failing if one is missed.
//
// Kept in a `.ts` beside the page so the coverage tests can iterate it: `i18nEnumKeys.test.ts`
// demands both locales have the strings, and the test next door pins the list to the backend's
// NodeKind.

import type { NodeKind } from '../types/api';

/**
 * Kinds the add-node form offers, in the order the select lists them.
 *
 * A **subset** of the backend's `NodeKind`, deliberately: a Meraki node exists because the org
 * collector discovered it, so there is nothing for a human to type. When `NodeKind` grows, the
 * test next door fails and the question "is this one hand-addable?" gets answered on purpose
 * rather than by omission.
 */
export const ADDABLE_KINDS = ['device', 'url', 'dns'] as const;

export type AddableKind = (typeof ADDABLE_KINDS)[number];

/** The target field a kind is built around — the one thing besides the name that must be filled. */
export interface MonitorTargets {
  /** Device: the IP or hostname to poll. */
  address: string;
  /** URL monitor: the endpoint to request. */
  url: string;
  /** DNS monitor: the name to resolve. */
  dnsName: string;
}

/** Per-kind strings and behaviour for the add-node form. All keys are in the `nodes` namespace. */
export interface MonitorKindSpec {
  readonly kind: AddableKind;
  /** Label in the "Monitoring type" select. */
  readonly optionKey: string;
  /** Modal title and primary-button label. */
  readonly titleKey: string;
  /** Message shown when creation fails. */
  readonly errorKey: string;
  /** Which of [`MonitorTargets`] this kind requires. */
  readonly target: keyof MonitorTargets;
}

export const MONITOR_KINDS = [
  {
    kind: 'device',
    optionKey: 'add.typeDevice',
    titleKey: 'add.node',
    errorKey: 'err.addNode',
    target: 'address',
  },
  {
    kind: 'url',
    optionKey: 'add.typeUrl',
    titleKey: 'add.urlMonitor',
    errorKey: 'err.addUrlMonitor',
    target: 'url',
  },
  {
    kind: 'dns',
    optionKey: 'add.typeDns',
    titleKey: 'add.dnsMonitor',
    errorKey: 'err.addDnsMonitor',
    target: 'dnsName',
  },
] as const satisfies readonly MonitorKindSpec[];

/** The spec for a kind. Total over [`AddableKind`], so no call site needs a fallback. */
export function monitorKind(kind: AddableKind): MonitorKindSpec {
  // The non-null assertion is safe by construction: the test next door proves the list covers
  // every member of the union it derives.
  return MONITOR_KINDS.find((k) => k.kind === kind)!;
}

/** Whether the kind's required target field has been filled in. */
export function targetFilled(kind: AddableKind, targets: MonitorTargets): boolean {
  return targets[monitorKind(kind).target].trim() !== '';
}

/** Narrowing guard for the `<select>`'s string value, so no unchecked cast reaches the form state. */
export function isAddableKind(v: string): v is AddableKind {
  return (ADDABLE_KINDS as readonly string[]).includes(v);
}

/** Compile-time proof that every addable kind is a real backend kind (never the reverse). */
const _addableIsANodeKind: readonly NodeKind[] = ADDABLE_KINDS;
void _addableIsANodeKind;
