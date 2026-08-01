// SPDX-License-Identifier: AGPL-3.0-only
// The forwarding-destination form's round trip. A destination relays log bodies — which routinely
// carry credentials — off box, and three different fields each encode a different meaning for
// "the box is blank". Getting one wrong either wipes a stored secret or silently keeps one the
// operator meant to clear.

import { describe, expect, it } from 'vitest';
import { draftFrom, emptyDraft, toInput, type Draft } from './forwardingDraft';
import type { ForwardDestination } from '../types/api';

const dest = (over: Partial<ForwardDestination> = {}): ForwardDestination =>
  ({
    id: 'd1',
    name: 'siem',
    enabled: true,
    source_kind: 'syslog',
    dest_kind: 'syslog_tls',
    target: 'siem.example.test:6514',
    pool: null,
    verbatim: true,
    filter: null,
    rate_limit_per_sec: null,
    ca_cert: null,
    ...over,
  }) as unknown as ForwardDestination;

const draft = (over: Partial<Draft> = {}): Draft => ({ ...emptyDraft(), ...over });

describe('draftFrom', () => {
  it('never pre-fills a secret, because the API does not return one', () => {
    // A pre-filled box would be a lie about what is stored, and re-saving would send it back as
    // if the operator had re-typed it.
    const d = draftFrom(dest());
    expect(d.community).toBe('');
    expect(d.service_account_json).toBe('');
  });

  it('turns a null pool and CA into empty boxes the form can control', () => {
    const d = draftFrom(dest({ pool: null, ca_cert: null }));
    expect(d.pool).toBe('');
    expect(d.ca_cert).toBe('');
  });

  it('defaults an absent filter to match-all with no conditions', () => {
    const d = draftFrom(dest({ filter: undefined } as unknown as Partial<ForwardDestination>));
    expect(d.mode).toBe('all');
    expect(d.conditions).toEqual([]);
  });

  it('gives every condition a controlled string value', () => {
    // `Condition.value` is optional on the wire; an undefined would uncontrol the text input.
    const d = draftFrom(
      dest({
        filter: {
          mode: 'any',
          conditions: [{ field: 'source_ip', op: 'eq', value: undefined }],
        },
      } as unknown as Partial<ForwardDestination>),
    );
    expect(d.mode).toBe('any');
    expect(d.conditions[0].value).toBe('');
  });

  it('renders an absent rate limit as an empty box, not "null"', () => {
    expect(draftFrom(dest({ rate_limit_per_sec: null })).rate_limit).toBe('');
    expect(draftFrom(dest({ rate_limit_per_sec: 500 })).rate_limit).toBe('500');
  });
});

describe('toInput', () => {
  it('trims the text fields an operator types', () => {
    const body = toInput(draft({ name: '  siem  ', target: '  host:514  ', pool: '  edge  ' }));
    expect(body.name).toBe('siem');
    expect(body.target).toBe('host:514');
    expect(body.pool).toBe('edge');
  });

  it('sends a blank pool as null so the destination is not bound to a pool named ""', () => {
    expect(toInput(draft({ pool: '   ' })).pool).toBeNull();
  });

  it('sends a blank rate limit as null rather than zero', () => {
    // Zero would mean "forward nothing", which is the opposite of "no limit".
    expect(toInput(draft({ rate_limit: '' })).rate_limit_per_sec).toBeNull();
    expect(toInput(draft({ rate_limit: ' 250 ' })).rate_limit_per_sec).toBe(250);
  });

  it('always sends the CA certificate, so clearing the box removes it', () => {
    // Unlike a secret, a CA certificate comes back from the API — the form holds its real value,
    // so an omitted field would be indistinguishable from "unchanged" and it could never be
    // cleared.
    const tls = draft({ dest_kind: 'syslog_tls', ca_cert: '-----BEGIN CERTIFICATE-----' });
    expect(toInput(tls).ca_cert).toBe('-----BEGIN CERTIFICATE-----');
    expect(toInput({ ...tls, ca_cert: '   ' }).ca_cert).toBeNull();
  });

  it('drops the CA certificate for a destination that does not speak TLS', () => {
    const plain = draft({ dest_kind: 'syslog_udp', ca_cert: 'stale' });
    expect(toInput(plain).ca_cert).toBeNull();
  });

  it('omits a blank secret entirely, so core keeps the stored one', () => {
    // The distinction that matters: `community: ""` would overwrite a working community with an
    // empty one; omitting the key leaves it alone.
    const trap = draft({ dest_kind: 'snmp_trap_udp', community: '' });
    expect('community' in toInput(trap)).toBe(false);
    const withSecret = toInput(draft({ dest_kind: 'snmp_trap_udp', community: ' public ' }));
    expect(withSecret.community).toBe('public');
  });

  it('omits a blank Google key, which on a new destination selects Workload Identity', () => {
    const bq = draft({ dest_kind: 'bigquery', service_account_json: '' });
    expect('service_account_json' in toInput(bq)).toBe(false);
    const keyed = toInput(draft({ dest_kind: 'bigquery', service_account_json: ' {"k":1} ' }));
    expect(keyed.service_account_json).toBe('{"k":1}');
  });

  it('never sends a secret to a destination kind that has no use for one', () => {
    const udp = draft({ dest_kind: 'syslog_udp', community: 'public', service_account_json: '{}' });
    const body = toInput(udp);
    expect('community' in body).toBe(false);
    expect('service_account_json' in body).toBe(false);
  });

  it('round-trips a stored destination unchanged apart from the secrets it cannot see', () => {
    const stored = dest({
      pool: 'edge',
      rate_limit_per_sec: 100,
      ca_cert: 'CERT',
      filter: { mode: 'any', conditions: [{ field: 'source_ip', op: 'eq', value: '10.0.0.1' }] },
    } as unknown as Partial<ForwardDestination>);
    const body = toInput(draftFrom(stored));
    expect(body).toMatchObject({
      name: 'siem',
      source_kind: 'syslog',
      dest_kind: 'syslog_tls',
      target: 'siem.example.test:6514',
      pool: 'edge',
      verbatim: true,
      rate_limit_per_sec: 100,
      ca_cert: 'CERT',
    });
    expect(body.filter).toEqual({
      mode: 'any',
      conditions: [{ field: 'source_ip', op: 'eq', value: '10.0.0.1' }],
    });
  });
});
