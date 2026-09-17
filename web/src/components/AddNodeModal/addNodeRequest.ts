// SPDX-License-Identifier: AGPL-3.0-only
// Turning the add-node form's fields into the right create call.
//
// The form holds every kind's fields at once — the operator can type an address, switch to "URL
// monitor", and type a URL — so "which fields go in the body" is a real decision and not a
// formality. Pure and in a `.ts` so it is unit-tested; the component beside it only renders.

import { api } from '../../services/api';
import type { AddableKind } from '../../pages/monitorKinds';
import { NODE_KINDS } from '../../types/api';
import type { DnsRecordType, NodeKind } from '../../types/api';

/** Every field the add-node form collects, for all kinds at once. */
export interface AddNodeForm {
  name: string;
  /** Poll-pool; blank ⇒ inherit from the folder the node lands in (or the default pool). */
  pool: string;
  /** Optional parent node, for topology-based dependency suppression. */
  parentId: string;
  // Device.
  address: string;
  profileId: string;
  credentialId: string;
  vendor: string;
  model: string;
  // URL monitor.
  url: string;
  urlMethod: 'GET' | 'HEAD' | 'POST';
  verifyTls: boolean;
  // DNS monitor.
  dnsName: string;
  dnsRecordType: DnsRecordType;
  dnsResolver: string;
}

/** A blank form — also what "reset" means, though the modal gets that from unmounting. */
export const EMPTY_ADD_NODE_FORM: AddNodeForm = {
  name: '',
  pool: '',
  parentId: '',
  address: '',
  profileId: '',
  credentialId: '',
  vendor: '',
  model: '',
  url: '',
  urlMethod: 'GET',
  verifyTls: true,
  dnsName: '',
  dnsRecordType: 'A',
  dnsResolver: '',
};

// Pinned to the API client rather than restated: a change to a create body's shape has to be
// answered here instead of silently leaving a field unsent.
type DeviceBody = Parameters<typeof api.createNode>[0];
type UrlBody = Parameters<typeof api.createUrlMonitor>[0];
type DnsBody = Parameters<typeof api.createDnsMonitor>[0];

/** Which endpoint to call and with what. */
export type CreateRequest =
  | { kind: 'device'; body: DeviceBody }
  | { kind: 'url'; body: UrlBody }
  | { kind: 'dns'; body: DnsBody };

/**
 * Build the create call for the kind currently selected.
 *
 * Exhaustive over [`AddableKind`] with a declared return type, so a fourth kind fails to compile
 * here — the one place in the add flow that genuinely needs per-kind code.
 *
 * Blank optional fields are omitted, not sent empty: `pool: ''` would name a pool called "" rather
 * than inherit, and `resolver: ''` would ask the poller to resolve against no server at all.
 */
export function createRequest<K extends AddableKind>(
  kind: K,
  f: AddNodeForm,
): Extract<CreateRequest, { kind: K }>;
export function createRequest(kind: AddableKind, f: AddNodeForm): CreateRequest {
  const pool = f.pool.trim() || undefined;
  const parent_id = f.parentId || undefined;
  switch (kind) {
    case 'url':
      return {
        kind,
        body: {
          name: f.name,
          url: f.url,
          method: f.urlMethod,
          verify_tls: f.verifyTls,
          parent_id,
          pool,
        },
      };
    case 'dns':
      return {
        kind,
        body: {
          name: f.name,
          dns_name: f.dnsName,
          record_type: f.dnsRecordType,
          // Blank ⇒ the poller's system resolver.
          resolver: f.dnsResolver.trim() || undefined,
          parent_id,
          pool,
        },
      };
    case 'device':
      return {
        kind,
        body: {
          name: f.name,
          address: f.address,
          profile_id: f.profileId || undefined,
          credential_id: f.credentialId || undefined,
          parent_id,
          vendor: f.vendor.trim() || undefined,
          model: f.model.trim() || undefined,
          pool,
        },
      };
  }
}

/** Issue the request. Split from {@link createRequest} so the body rules can be tested without a
 *  network fake, and so the kind→endpoint mapping stays exhaustive in one place too. */
export function sendCreate(req: CreateRequest): Promise<{ id: string }> {
  switch (req.kind) {
    case 'device':
      return api.createNode(req.body);
    case 'url':
      return api.createUrlMonitor(req.body);
    case 'dns':
      return api.createDnsMonitor(req.body);
  }
}

/**
 * The node kinds that mean "a device is already monitored at this address" (ADR-139 決定 1).
 *
 * URL and DNS monitors store an address too — the host a URL resolved to, the resolver asked — and a
 * router that merely serves a monitored web page is not already monitored. A Meraki device is. A
 * `Record` over every kind, so a new kind is a compile error here rather than silently not counted.
 */
const MEANS_A_DEVICE_AT_THE_ADDRESS: Record<NodeKind, boolean> = {
  device: true,
  meraki: true,
  // An AP is a device at its address — while the controller reports one. An AP it reports no
  // address for sits at 0.0.0.0, which no operator types into this form.
  wireless_ap: true,
  url: false,
  dns: false,
};

/** The `kind` filter the duplicate-address lookup sends with `address`. */
export const DUPLICATE_ADDRESS_KINDS: readonly NodeKind[] = NODE_KINDS.filter(
  (k) => MEANS_A_DEVICE_AT_THE_ADDRESS[k],
);

/** A node already monitored at the address being added. */
export interface SameAddressNode {
  id: string;
  name: string;
}

/**
 * Whether to ask what is already monitored at the address before creating: only for a device, only
 * for an address that is not blank, and never again for the address the operator already confirmed.
 * The same three conditions {@link duplicateWarning} applies to the answer, so the question is not
 * sent when the answer could not stop the create.
 */
export function needsAddressLookup(
  kind: AddableKind,
  address: string,
  confirmedAddress: string | null,
): boolean {
  const trimmed = address.trim();
  return kind === 'device' && trimmed !== '' && confirmedAddress !== trimmed;
}

/**
 * Whether adding a device must stop and warn first (ADR-139 増分 2 決定 12): the nodes to name, or
 * `null` to go ahead.
 *
 * The warning never refuses. An operator who has read it passes `confirmedAddress`, and that address
 * then goes through; a different address typed after confirming is a new question.
 *
 * ⚠️ `found` is `null` when the lookup failed, and that goes ahead too. The warning is an aid, and a
 * read that failed must not stop a create the API would have accepted.
 */
export function duplicateWarning(
  kind: AddableKind,
  address: string,
  found: readonly SameAddressNode[] | null,
  confirmedAddress: string | null,
): SameAddressNode[] | null {
  if (kind !== 'device') return null;
  if (!found || found.length === 0) return null;
  if (confirmedAddress !== null && confirmedAddress === address.trim()) return null;
  return [...found];
}
