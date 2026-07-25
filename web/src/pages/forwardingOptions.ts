// SPDX-License-Identifier: AGPL-3.0-only
// Forwarding-destination option rules (ADR-034). Kept pure (no React) so the form can never offer
// a combination core would reject, and so that guarantee is unit-testable without rendering.
//
// These mirror the Rust side exactly — `yagra_forward::FilterField::{value_type, applies_to}` and
// `DestKind::{accepts, supports_verbatim}`. The backend still validates everything (it is the
// authority); this exists so the UI greys out the impossible instead of letting an admin discover
// it via a 400. If a rule changes there, change it here in the same commit.

import type {
  ForwardDestKind,
  ForwardFilterField,
  ForwardFilterOp,
  ForwardSourceKind,
} from '../types/api';

/** Operator sets by field datum type, mirroring `yagra_forward::ValueType`. */
const TEXT_OPS: ForwardFilterOp[] = [
  'contains',
  'not_contains',
  'eq',
  'ne',
  'prefix',
  'regex',
  'not_regex',
  'in_list',
];
const NUM_OPS: ForwardFilterOp[] = ['lte', 'gte', 'eq', 'ne', 'in_list'];
const IP_OPS: ForwardFilterOp[] = ['in_cidr', 'not_in_cidr', 'eq', 'ne'];
/** A varbind list: positive operators mean "any matches", negative "none matches". */
const MULTI_OPS: ForwardFilterOp[] = ['contains', 'not_contains', 'prefix', 'regex', 'not_regex'];

const FIELD_OPS: Record<ForwardFilterField, ForwardFilterOp[]> = {
  source_ip: IP_OPS,
  pool: TEXT_OPS,
  kind: TEXT_OPS,
  facility: NUM_OPS,
  severity: NUM_OPS,
  hostname: TEXT_OPS,
  app_name: TEXT_OPS,
  message: TEXT_OPS,
  trap_oid: TEXT_OPS,
  varbind: MULTI_OPS,
  src_addr: IP_OPS,
  dst_addr: IP_OPS,
  proto: NUM_OPS,
  src_port: NUM_OPS,
  dst_port: NUM_OPS,
  src_as: NUM_OPS,
  dst_as: NUM_OPS,
};

/** Fields that only ever carry a value on one stream. */
const SYSLOG_ONLY: ForwardFilterField[] = ['facility', 'severity', 'hostname', 'app_name'];
const TRAP_ONLY: ForwardFilterField[] = ['trap_oid', 'varbind'];
const FLOW_ONLY: ForwardFilterField[] = [
  'src_addr',
  'dst_addr',
  'proto',
  'src_port',
  'dst_port',
  'src_as',
  'dst_as',
];
/** A flow datagram has no message text. */
const NOT_ON_FLOW: ForwardFilterField[] = ['message'];

/** Every field, in the order the form lists them (common first, then stream-specific). */
export const FORWARD_FILTER_FIELDS: ForwardFilterField[] = [
  'source_ip',
  'pool',
  'kind',
  'message',
  'facility',
  'severity',
  'hostname',
  'app_name',
  'trap_oid',
  'varbind',
  'src_addr',
  'dst_addr',
  'proto',
  'src_port',
  'dst_port',
  'src_as',
  'dst_as',
];

/** Operators valid for `field`. */
export function opsForField(field: ForwardFilterField): ForwardFilterOp[] {
  return FIELD_OPS[field];
}

/** Whether `field` can ever carry a value on `source`. */
export function fieldAppliesTo(field: ForwardFilterField, source: ForwardSourceKind): boolean {
  if (SYSLOG_ONLY.includes(field)) return source === 'syslog';
  if (TRAP_ONLY.includes(field)) return source === 'trap';
  if (FLOW_ONLY.includes(field)) return source === 'flow';
  if (NOT_ON_FLOW.includes(field)) return source !== 'flow';
  return true;
}

/** The fields the form offers for `source`. */
export function fieldsForSource(source: ForwardSourceKind): ForwardFilterField[] {
  return FORWARD_FILTER_FIELDS.filter((f) => fieldAppliesTo(f, source));
}

/** Destination kinds that can carry `source`. A trap may be shipped to a syslog collector (as a
 *  rendered line); a syslog line has no SNMP PDU form, so the reverse is not offered. Flow is its
 *  own world among the *relay* kinds — a flow export only means anything to a flow collector —
 *  while BigQuery takes every stream, because every stream has a row shape. */
export function destKindsForSource(source: ForwardSourceKind): ForwardDestKind[] {
  if (source === 'flow') return ['flow_udp', 'bigquery'];
  const syslog: ForwardDestKind[] = ['syslog_udp', 'syslog_tcp', 'syslog_tls'];
  return source === 'trap'
    ? ['snmp_trap_udp', ...syslog, 'bigquery']
    : [...syslog, 'bigquery'];
}

/** Whether the original datagram can be relayed byte-for-byte for this pairing. When false the
 *  form forces (and explains) the derived output — core rejects `verbatim` on it outright. */
export function supportsVerbatim(source: ForwardSourceKind, dest: ForwardDestKind): boolean {
  // A table row is never "the original bytes", and deliberately has no raw-payload column.
  if (dest === 'bigquery') return false;
  if (dest === 'flow_udp') return source === 'flow';
  if (dest === 'snmp_trap_udp') return source === 'trap';
  return source === 'syslog';
}

/** Whether a **derived** form exists for this pairing. `flow_udp` has none — a template-bound binary
 *  export cannot be rebuilt from decoded records — so a flow *relay* is byte-exact or nothing.
 *  BigQuery is the opposite: rows only, for every stream. */
export function supportsRendered(source: ForwardSourceKind, dest: ForwardDestKind): boolean {
  if (dest === 'bigquery') return true;
  return source !== 'flow' && dest !== 'flow_udp';
}

/** Whether a community applies (only a re-encoded SNMP trap uses one). */
export function usesCommunity(dest: ForwardDestKind): boolean {
  return dest === 'snmp_trap_udp';
}

/** Whether the destination speaks TLS, and so takes an optional CA certificate. BigQuery is HTTPS
 *  but to a fixed Google endpoint, so there is nothing for an operator to pin. */
export function usesTls(dest: ForwardDestKind): boolean {
  return dest === 'syslog_tls';
}

/** Whether the destination is addressed as `host:port`. BigQuery names a table instead. */
export function usesHostPort(dest: ForwardDestKind): boolean {
  return dest !== 'bigquery';
}

/** Whether the destination takes a Google service-account key (optional — omitting it selects
 *  Workload Identity). */
export function usesServiceAccount(dest: ForwardDestKind): boolean {
  return dest === 'bigquery';
}

/** Whether a filter on this destination applies to whole datagrams rather than individual records.
 *  True only for the flow **relay**: records cannot be removed from a template-bound bundle, so one
 *  matching record carries the lot. A BigQuery flow destination writes rows and filters exactly. */
export function filtersWholeDatagram(
  source: ForwardSourceKind,
  dest: ForwardDestKind,
): boolean {
  return source === 'flow' && dest === 'flow_udp';
}

/**
 * Repair a draft after the source or destination kind changes, so the form is never left holding a
 * combination the API would reject: an unusable destination kind falls back to the first valid one,
 * `verbatim` is forced where it is the only option and cleared where it is impossible, a CA
 * certificate is dropped off a non-TLS destination, a service-account key is dropped off a
 * non-BigQuery one, and conditions on fields the new stream never carries are dropped.
 */
export function reconcileDraft<
  T extends {
    source_kind: ForwardSourceKind;
    dest_kind: ForwardDestKind;
    verbatim: boolean;
    ca_cert: string;
    service_account_json: string;
    conditions: { field: ForwardFilterField; op: ForwardFilterOp; value: string }[];
  },
>(draft: T): T {
  const kinds = destKindsForSource(draft.source_kind);
  const dest_kind = kinds.includes(draft.dest_kind) ? draft.dest_kind : kinds[0];
  const canRender = supportsRendered(draft.source_kind, dest_kind);
  return {
    ...draft,
    dest_kind,
    // Not `&&`: when the derived form is impossible, verbatim is not merely allowed — it is the only
    // option, and leaving the draft on "rendered" would post a body core rejects.
    verbatim: canRender ? draft.verbatim && supportsVerbatim(draft.source_kind, dest_kind) : true,
    ca_cert: usesTls(dest_kind) ? draft.ca_cert : '',
    service_account_json: usesServiceAccount(dest_kind) ? draft.service_account_json : '',
    conditions: draft.conditions
      .filter((c) => fieldAppliesTo(c.field, draft.source_kind))
      .map((c) => (opsForField(c.field).includes(c.op) ? c : { ...c, op: opsForField(c.field)[0] })),
  };
}
