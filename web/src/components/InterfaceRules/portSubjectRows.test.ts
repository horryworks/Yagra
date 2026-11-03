// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { interfaceScopeId } from '../../lib/interfaceScope';
import { PORT_RULE_SUBJECTS } from '../../lib/portRuleForm';
import type { MatchingThreshold, StoredThreshold } from '../../types/api';
import { isOpticalPort, ownRuleCount, portSubjectRows } from './portSubjectRows';

const NODE = '11111111-1111-4111-8111-111111111111';
const PORT = interfaceScopeId(NODE, 3);

let n = 0;
function row(over: Partial<StoredThreshold>, in_force = true): MatchingThreshold {
  n += 1;
  return {
    in_force,
    rule: {
      id: `r${n}`,
      metric: 'if_in_util_pct',
      scope_level: 'interface',
      scope_ids: [PORT],
      direction: 'above',
      warning: 70,
      critical: 90,
      dwell_samples: 3,
      ...over,
    } as StoredThreshold,
  };
}
const global = (over: Partial<StoredThreshold>, in_force = true) =>
  row({ scope_level: 'global', scope_ids: [], ...over }, in_force);

const rowsOf = (rows: MatchingThreshold[], optical = true) =>
  portSubjectRows(rows, NODE, 3, optical);
const subject = (rows: MatchingThreshold[], s: string, optical = true) =>
  rowsOf(rows, optical).subjects.find((r) => r.subject === s);

describe('portSubjectRows', () => {
  it('draws the five subjects, in order, on an optical port with no rules', () => {
    const got = rowsOf([]);
    expect(got.subjects.map((r) => r.subject)).toEqual([...PORT_RULE_SUBJECTS]);
    expect(got.subjects.every((r) => r.action.kind === 'set')).toBe(true);
    expect(got.other).toEqual([]);
  });

  it('leaves the light levels off a copper port unless it has a rule of its own there', () => {
    expect(rowsOf([], false).subjects.map((r) => r.subject)).not.toContain('optical_rx');
    const own = row({ metric: 'if_rx_power_dbm', direction: 'below', warning: -20, critical: -25 });
    expect(subject([own], 'optical_rx', false)).toBeDefined();
  });

  it('offers to edit the port’s own rule', () => {
    const r = subject([row({})], 'in_traffic');
    expect(r?.action.kind).toBe('edit');
    expect(r?.own).toHaveLength(1);
  });

  it('offers to override an inherited rule, starting from its bounds', () => {
    const r = subject(
      [global({ metric: 'if_rx_power_dbm', direction: 'below', warning: -20, critical: -25 })],
      'optical_rx',
    );
    expect(r?.action.kind).toBe('override');
    expect(r?.action.kind === 'override' && r.action.form.warning).toBe('-20');
    expect(r?.own).toEqual([]);
  });

  it('puts the port’s own rule first and leaves out what lost', () => {
    const lost = global({}, false);
    const mine = row({});
    const r = subject([lost, mine], 'in_traffic');
    expect(r?.inForce.map((m) => m.rule.id)).toEqual([mine.rule.id]);
  });

  it('sends a band the form cannot say to the rules screen', () => {
    const band = row({ warning_below: 10, warning_above: 90 } as Partial<StoredThreshold>);
    // A two-sided rule is not one the port form can express; it must not be rounded.
    const r = subject([{ ...band, rule: { ...band.rule, direction: 'below' } }], 'in_traffic');
    expect(r?.action.kind).toBe('elsewhere');
  });

  it('lists an in-force per-interface rule no subject covers, read-only', () => {
    const errors = global({ metric: 'if_in_errors' });
    expect(rowsOf([errors]).other).toEqual([errors]);
    expect(rowsOf([global({ metric: 'if_in_errors' }, false)]).other).toEqual([]);
  });

  it('counts only the port’s own rules for the button', () => {
    expect(ownRuleCount([row({}), global({})], NODE, 3)).toBe(1);
  });
});

describe('isOpticalPort', () => {
  const none = { transceiver_model: null, if_media: null };
  it('reads a model, a media name, or a light reading', () => {
    expect(isOpticalPort({ ...none, transceiver_model: 'SFP-10G-SR' }, null)).toBe(true);
    expect(isOpticalPort({ ...none, if_media: '10GBASE-SR' }, null)).toBe(true);
    expect(isOpticalPort(none, { rx_power_dbm: [null, -3.1], tx_power_dbm: [] })).toBe(true);
    expect(isOpticalPort(none, { rx_power_dbm: [null], tx_power_dbm: [null] })).toBe(false);
    expect(isOpticalPort(none, null)).toBe(false);
  });
});
