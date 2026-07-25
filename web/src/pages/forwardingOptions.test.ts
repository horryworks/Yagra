// SPDX-License-Identifier: AGPL-3.0-only
// These rules mirror core's validation (yagra_forward). The point of the tests is that the form
// cannot offer a combination the API would reject with a 400 — so they assert the same invariants
// the Rust-side tests assert, from the UI's end.

import { describe, expect, it } from 'vitest';
import {
  destKindsForSource,
  fieldAppliesTo,
  fieldsForSource,
  opsForField,
  reconcileDraft,
  filtersWholeDatagram,
  supportsRendered,
  supportsVerbatim,
  usesCommunity,
  usesHostPort,
  usesServiceAccount,
  usesTls,
} from './forwardingOptions';
import type { ForwardFilterField, ForwardSourceKind } from '../types/api';

const STREAMS: ForwardSourceKind[] = ['syslog', 'trap', 'flow'];

describe('forwardingOptions', () => {
  it('scopes stream-specific fields to their stream', () => {
    expect(fieldAppliesTo('facility', 'syslog')).toBe(true);
    expect(fieldAppliesTo('facility', 'trap')).toBe(false);
    expect(fieldAppliesTo('trap_oid', 'trap')).toBe(true);
    expect(fieldAppliesTo('trap_oid', 'syslog')).toBe(false);
    for (const f of ['source_ip', 'pool', 'kind'] as ForwardFilterField[]) {
      for (const s of STREAMS) expect(fieldAppliesTo(f, s)).toBe(true);
    }
    expect(fieldsForSource('trap')).not.toContain('hostname');
    expect(fieldsForSource('syslog')).not.toContain('varbind');
  });

  it('scopes flow record fields to the flow stream', () => {
    for (const f of ['src_addr', 'dst_addr', 'proto', 'src_port', 'dst_port', 'src_as', 'dst_as'] as ForwardFilterField[]) {
      expect(fieldAppliesTo(f, 'flow')).toBe(true);
      expect(fieldAppliesTo(f, 'syslog')).toBe(false);
      expect(fieldAppliesTo(f, 'trap')).toBe(false);
    }
    // A flow datagram carries no message text.
    expect(fieldAppliesTo('message', 'flow')).toBe(false);
    expect(fieldsForSource('flow')).not.toContain('severity');
    expect(fieldsForSource('flow')).toContain('src_addr');
  });

  it('offers only operators the field type accepts', () => {
    // A regex over a numeric field is exactly what core rejects.
    expect(opsForField('severity')).not.toContain('regex');
    expect(opsForField('severity')).toContain('lte');
    expect(opsForField('source_ip')).toEqual(['in_cidr', 'not_in_cidr', 'eq', 'ne']);
    expect(opsForField('message')).toContain('regex');
    expect(opsForField('message')).not.toContain('in_cidr');
    // Varbinds are a list: equality has no meaning, containment does.
    expect(opsForField('varbind')).not.toContain('eq');
    expect(opsForField('varbind')).toContain('contains');
    // Flow addresses are addresses; ports and AS numbers are numbers.
    expect(opsForField('dst_addr')).toContain('in_cidr');
    expect(opsForField('dst_port')).not.toContain('regex');
    // Every field must offer at least one operator, or the form would render an empty select.
    for (const s of STREAMS) {
      for (const f of fieldsForSource(s)) expect(opsForField(f).length).toBeGreaterThan(0);
    }
  });

  it('pairs destination kinds with the streams they can carry', () => {
    expect(destKindsForSource('syslog')).toEqual([
      'syslog_udp',
      'syslog_tcp',
      'syslog_tls',
      'bigquery',
    ]);
    // A trap may be shipped to a syslog collector; a syslog line has no SNMP PDU form.
    expect(destKindsForSource('trap')).toContain('snmp_trap_udp');
    expect(destKindsForSource('trap')).toContain('syslog_udp');
    expect(destKindsForSource('syslog')).not.toContain('snmp_trap_udp');
    // Flow is isolated among the *relay* kinds, in both directions.
    expect(destKindsForSource('flow')).toEqual(['flow_udp', 'bigquery']);
    expect(destKindsForSource('syslog')).not.toContain('flow_udp');
    expect(destKindsForSource('trap')).not.toContain('flow_udp');
    // ...but BigQuery takes every stream, because every stream has a row shape.
    for (const s of STREAMS) expect(destKindsForSource(s)).toContain('bigquery');
  });

  it('treats BigQuery as rows-only, addressed by table rather than host:port', () => {
    for (const s of STREAMS) {
      // A table row is never "the original bytes" — and it always has a derived form, which is
      // what lets a *flow* stream reach BigQuery when `flow_udp` is the only other option.
      expect(supportsVerbatim(s, 'bigquery')).toBe(false);
      expect(supportsRendered(s, 'bigquery')).toBe(true);
    }
    expect(usesHostPort('bigquery')).toBe(false);
    expect(usesHostPort('flow_udp')).toBe(true);
    expect(usesServiceAccount('bigquery')).toBe(true);
    expect(usesServiceAccount('syslog_tls')).toBe(false);
    // HTTPS, but to a fixed Google endpoint — nothing for an operator to pin.
    expect(usesTls('bigquery')).toBe(false);
    expect(usesCommunity('bigquery')).toBe(false);
  });

  it('warns about datagram-wide filtering only for the flow relay', () => {
    // The one behavioural difference an operator must be told about: `flow_udp` cannot drop a
    // record from a bundle, so non-matching records ride along. BigQuery filters exactly.
    expect(filtersWholeDatagram('flow', 'flow_udp')).toBe(true);
    expect(filtersWholeDatagram('flow', 'bigquery')).toBe(false);
    expect(filtersWholeDatagram('syslog', 'syslog_udp')).toBe(false);
  });

  it('drops a service-account key off a non-BigQuery destination', () => {
    const bq = reconcileDraft({
      source_kind: 'syslog',
      dest_kind: 'bigquery',
      verbatim: true, // impossible for BigQuery — must be cleared, not sent to a 400
      ca_cert: '',
      service_account_json: '{"client_email":"a@b"}',
      conditions: [],
    });
    expect(bq.verbatim).toBe(false);
    expect(bq.service_account_json).toBe('{"client_email":"a@b"}');
    const relay = reconcileDraft({ ...bq, dest_kind: 'syslog_udp' });
    expect(relay.service_account_json).toBe('');
  });

  it('allows byte-exact relay only where the wire form matches', () => {
    expect(supportsVerbatim('syslog', 'syslog_udp')).toBe(true);
    expect(supportsVerbatim('syslog', 'syslog_tls')).toBe(true);
    expect(supportsVerbatim('trap', 'snmp_trap_udp')).toBe(true);
    // A trap PDU on a syslog collector's port would be undecodable.
    expect(supportsVerbatim('trap', 'syslog_udp')).toBe(false);
    expect(supportsVerbatim('syslog', 'snmp_trap_udp')).toBe(false);
    expect(usesCommunity('snmp_trap_udp')).toBe(true);
    expect(usesCommunity('syslog_udp')).toBe(false);
    expect(usesTls('syslog_tls')).toBe(true);
    expect(usesTls('syslog_tcp')).toBe(false);
  });

  it('treats flow as byte-exact only — there is no rendered form', () => {
    expect(supportsVerbatim('flow', 'flow_udp')).toBe(true);
    expect(supportsRendered('flow', 'flow_udp')).toBe(false);
    expect(supportsRendered('syslog', 'syslog_udp')).toBe(true);
    expect(supportsRendered('trap', 'snmp_trap_udp')).toBe(true);
  });

  it('repairs a draft when the source kind changes', () => {
    const repaired = reconcileDraft({
      source_kind: 'trap',
      dest_kind: 'syslog_udp',
      verbatim: true,
      ca_cert: '',
      service_account_json: '',
      conditions: [
        { field: 'facility', op: 'lte', value: '4' }, // syslog-only: must be dropped
        { field: 'trap_oid', op: 'prefix', value: '1.3.6' }, // valid on this stream
      ],
    });
    expect(repaired.conditions).toEqual([{ field: 'trap_oid', op: 'prefix', value: '1.3.6' }]);
    // trap → syslog_udp cannot carry the original bytes, so the flag is cleared rather than sent
    // to an API that would reject it.
    expect(repaired.verbatim).toBe(false);
    expect(repaired.dest_kind).toBe('syslog_udp');
  });

  it('falls back to a valid destination kind when the current one becomes impossible', () => {
    const repaired = reconcileDraft({
      source_kind: 'syslog',
      dest_kind: 'snmp_trap_udp', // not offered for syslog
      verbatim: true,
      ca_cert: '',
      service_account_json: '',
      conditions: [],
    });
    expect(repaired.dest_kind).toBe('syslog_udp');
    // The fallback pairing *does* support verbatim, so the flag survives.
    expect(repaired.verbatim).toBe(true);
  });

  it('switching to flow forces verbatim and drops event-only conditions', () => {
    const repaired = reconcileDraft({
      source_kind: 'flow',
      dest_kind: 'syslog_tcp', // impossible for flow
      verbatim: false, // impossible for flow — must be forced back on, not merely left alone
      ca_cert: 'PEM',
      service_account_json: '',
      conditions: [
        { field: 'severity', op: 'lte', value: '4' },
        { field: 'dst_port', op: 'eq', value: '443' },
      ],
    });
    expect(repaired.dest_kind).toBe('flow_udp');
    expect(repaired.verbatim).toBe(true);
    expect(repaired.conditions).toEqual([{ field: 'dst_port', op: 'eq', value: '443' }]);
    // A CA certificate is meaningless off a TLS destination, and core rejects it.
    expect(repaired.ca_cert).toBe('');
  });

  it('keeps a CA certificate only on a TLS destination', () => {
    const tls = reconcileDraft({
      source_kind: 'syslog',
      dest_kind: 'syslog_tls',
      verbatim: true,
      ca_cert: 'PEM',
      service_account_json: '',
      conditions: [],
    });
    expect(tls.ca_cert).toBe('PEM');
    const plain = reconcileDraft({ ...tls, dest_kind: 'syslog_udp' });
    expect(plain.ca_cert).toBe('');
  });

  it('rewrites an operator the new field type cannot take', () => {
    const repaired = reconcileDraft({
      source_kind: 'syslog',
      dest_kind: 'syslog_udp',
      verbatim: true,
      ca_cert: '',
      service_account_json: '',
      conditions: [{ field: 'severity', op: 'regex', value: '4' }],
    });
    expect(repaired.conditions[0].op).toBe(opsForField('severity')[0]);
  });
});
