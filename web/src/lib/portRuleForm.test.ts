// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  PORT_RULE_BASES,
  PORT_RULE_SUBJECTS,
  PORT_SUBJECT_SPECS,
  isPortRuleReady,
  needsPortSpeed,
  newPortRuleForm,
  portRuleFrom,
  portRuleMetrics,
  portRuleToThreshold,
  boundText,
  splitRate,
  type PortRuleForm,
} from './portRuleForm';
import type { StoredThreshold } from '../types/api';

const NODE = '11111111-2222-3333-4444-555555555555';
const IFINDEX = 7;

/** A stored rule as the API serves it, from a body this form produced. */
function stored(form: PortRuleForm): StoredThreshold {
  const body = portRuleToThreshold(form, NODE, IFINDEX);
  return {
    id: 'aaaaaaaa-0000-0000-0000-000000000001',
    scope_level: body.scope_level,
    scope_ids: body.scope_ids ?? [],
    metric: body.metric,
    direction: body.direction,
    warning: body.warning ?? null,
    critical: body.critical ?? null,
    dwell_samples: body.dwell_samples ?? 3,
  } as StoredThreshold;
}

describe('portRuleForm', () => {
  it('round-trips every subject and basis', () => {
    // The property that catches a field dropped in either direction, and the only guard on this
    // being a second representation of a rule the generic dialog also writes.
    for (const subject of PORT_RULE_SUBJECTS) {
      const spec = PORT_SUBJECT_SPECS[subject];
      const bases = spec.hasBasis ? PORT_RULE_BASES : (['percent'] as const);
      for (const basis of bases) {
        const form: PortRuleForm = {
          subject,
          basis,
          warning: spec.fixedBounds ? '' : basis === 'absolute' ? '80' : '70',
          critical: spec.fixedBounds ? '' : basis === 'absolute' ? '90' : '95',
          unit: 'Mbps',
          dwell: '4',
        };
        const back = portRuleFrom(stored(form));
        expect(back, `${subject}/${basis}`).toEqual(form);
      }
    }
  });

  it('stores link state as above 1.5, never below 0.5', () => {
    // The reason this form exists. `ifOperStatus` reports up=1 and down=2 and never returns 0, so
    // the rule an operator writes by hand — "below 0.5" — cannot fire at all. This repo shipped
    // exactly that once; the subject list is what makes it unreachable.
    const body = portRuleToThreshold(newPortRuleForm('link_state'), NODE, IFINDEX);
    expect(body.metric).toBe('if_oper_status');
    expect(body.direction).toBe('above');
    expect(body.critical).toBe(1.5);
    expect(body.warning).toBeUndefined();
    // Every value ifOperStatus can report other than `up` breaches it, and `up` does not.
    for (const v of [2, 3, 4, 5, 6, 7]) expect(v > 1.5).toBe(true);
    expect(1 > 1.5).toBe(false);
  });

  it('converts an absolute rate to bits per second and back', () => {
    const form: PortRuleForm = {
      subject: 'in_traffic',
      basis: 'absolute',
      warning: '',
      critical: '800',
      unit: 'Mbps',
      dwell: '3',
    };
    const body = portRuleToThreshold(form, NODE, IFINDEX);
    expect(body.metric).toBe('if_in_bps');
    expect(body.critical).toBe(800_000_000);
    const back = portRuleFrom(stored(form));
    expect(back?.critical).toBe('800');
    expect(back?.unit).toBe('Mbps');
  });

  it('picks the largest whole unit when reading a stored rate back', () => {
    expect(splitRate(90_000_000)).toEqual({ value: 90, unit: 'Mbps' });
    expect(splitRate(2_000_000_000)).toEqual({ value: 2, unit: 'Gbps' });
    expect(splitRate(64_000)).toEqual({ value: 64, unit: 'kbps' });
    // Not a whole number of any larger unit: show the raw rate rather than a rounded lie.
    expect(splitRate(1_500_500)).toEqual({ value: 1_500_500, unit: 'bps' });
    expect(splitRate(0)).toEqual({ value: 0, unit: 'bps' });
  });

  it('targets the port it was opened from', () => {
    const body = portRuleToThreshold(newPortRuleForm(), NODE, IFINDEX);
    expect(body.scope_level).toBe('interface');
    expect(body.scope_ids).toEqual([`${NODE}:${IFINDEX}`]);
  });

  it('refuses to read back a rule it cannot say in these words', () => {
    const base = stored(newPortRuleForm());
    // A metric with no subject. It is a real rule that really applies to the port, so the caller
    // shows it — read-only, by its metric name. Coercing it to the nearest subject would retarget
    // it on the next save.
    expect(portRuleFrom({ ...base, metric: 'if_admin_status' })).toBeNull();
    expect(portRuleFrom({ ...base, metric: 'my_custom_port_metric' })).toBeNull();
    // The right metric read the wrong way: an optical level *above* a bound is a legitimate rule
    // this form has no way to express.
    expect(portRuleFrom({ ...base, metric: 'if_rx_power_dbm', direction: 'above' })).toBeNull();
    // Link state with bounds this form would not have written — someone's deliberate choice, and
    // reopening it here would silently rewrite it.
    expect(
      portRuleFrom({ ...base, metric: 'if_oper_status', direction: 'above', critical: 2.5 }),
    ).toBeNull();
  });

  it('accepts the rules it does understand', () => {
    // The other half: a test that only shows refusals passes just as well when everything is
    // refused, and then the dialog silently degrades to a metric-name list.
    const accepted = portRuleMetrics().filter((metric) => {
      const spec = Object.values(PORT_SUBJECT_SPECS).find((s) =>
        [s.metric('percent'), s.metric('absolute')].includes(metric),
      );
      const base = stored(newPortRuleForm());
      return (
        portRuleFrom({
          ...base,
          metric,
          direction: spec!.direction,
          warning: spec!.fixedBounds ? (spec!.fixedBounds.warning ?? null) : 10,
          critical: spec!.fixedBounds ? spec!.fixedBounds.critical : 20,
        }) !== null
      );
    });
    expect(accepted.sort()).toEqual(portRuleMetrics().sort());
    expect(accepted).toHaveLength(7);
  });

  it('reads a stored bound in the unit its metric is stored in', () => {
    // The rules table showed the raw stored number, so an absolute rate read as `800000000`.
    expect(boundText('if_in_bps', 800_000_000)).toBe('800 Mbps');
    expect(boundText('if_out_bps', 64_000)).toBe('64 kbps');
    expect(boundText('if_in_util_pct', 90)).toBe('90%');
    expect(boundText('if_rx_power_dbm', -20)).toBe('-20 dBm');
    // Link state has no unit — its bound is a status code, and `1.5 codes` would be nonsense.
    expect(boundText('if_oper_status', 1.5)).toBe('1.5');
    // A metric this module does not model is left exactly as stored rather than given a unit it
    // may not have.
    expect(boundText('cpu_util', 90)).toBe('90');
  });

  it('knows which subjects need the port to report its speed', () => {
    expect(needsPortSpeed({ ...newPortRuleForm('in_traffic'), basis: 'percent' })).toBe(true);
    expect(needsPortSpeed({ ...newPortRuleForm('in_traffic'), basis: 'absolute' })).toBe(false);
    // A subject with no basis has no denominator either way.
    expect(needsPortSpeed(newPortRuleForm('link_state'))).toBe(false);
  });

  it('needs a bound unless the subject fixes one', () => {
    const blank = { ...newPortRuleForm(), critical: '' };
    expect(isPortRuleReady(blank)).toBe(false);
    expect(isPortRuleReady({ ...blank, warning: '70' })).toBe(true);
    // Link state types no bounds at all, so it is ready as soon as it is chosen.
    expect(isPortRuleReady(newPortRuleForm('link_state'))).toBe(true);
  });

  it('sends an untyped bound as absent, never as zero', () => {
    const body = portRuleToThreshold({ ...newPortRuleForm(), warning: '  ' }, NODE, IFINDEX);
    expect(body.warning).toBeUndefined();
    // `Number('')` is 0, and an `above 0` warning fires on every sample forever.
    expect(body.critical).toBe(90);
  });
});
