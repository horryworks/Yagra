// SPDX-License-Identifier: AGPL-3.0-only
// Turning the add-node form's fields into the right create call.
//
// The form holds every kind's fields at once — the operator can type an address, switch to "URL
// monitor", and type a URL — so "which fields go in the body" is a real decision and not a
// formality. Pure and in a `.ts` so it is unit-tested; the component beside it only renders.

import { api } from '../../services/api';
import type { AddableKind } from '../../pages/monitorKinds';
import type { DnsRecordType } from '../../types/api';

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
