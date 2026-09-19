// SPDX-License-Identifier: AGPL-3.0-only
// `ruleToInput` is a hand-written projection of a stored rule onto the shape the form submits, and
// it is the kind of function whose failure is silent: a field missing here reads and edits fine,
// and is reset to its default the next time anyone saves that rule.
//
// So the assertion that matters is not "it copies these fields" — it is **"it copies every field
// the input type has"**, derived from an object rather than from a hand-written list, so a field
// added to `EventRuleInput` fails here instead of being quietly dropped in production.
import { describe, expect, it } from 'vitest';
import {
  EVENT_RULE_BOUNDS,
  eventRuleNumberProblem,
  ruleToInput,
  typedInteger,
} from './eventRuleForm';
import type { EventRule, EventRuleInput } from '../types/api';

/** One of every field, each with a value distinguishable from a default. */
const INPUT: Required<EventRuleInput> = {
  name: 'ssh brute force',
  enabled: false,
  source_kind: 'syslog',
  source_id: '11111111-1111-4111-8111-111111111111',
  node_id: '22222222-2222-4222-8222-222222222222',
  match_kind: 'regex',
  pattern: 'Failed password',
  clear_pattern: 'Accepted password',
  severity: 'critical',
  ttl_secs: 900,
  min_count: 5,
  window_secs: 300,
} as Required<EventRuleInput>;

const STORED = { id: 'r1', created_at: '2026-01-01T00:00:00Z', ...INPUT } as unknown as EventRule;

describe('ruleToInput', () => {
  it('round-trips every field of a stored rule', () => {
    expect(ruleToInput(STORED)).toEqual(INPUT);
  });

  it('carries no key the input type does not have, and drops none that it does', () => {
    // Both directions. The first stops the identity/timestamp columns leaking into a PUT body; the
    // second is the silent-reset failure this module exists for.
    expect(Object.keys(ruleToInput(STORED)).sort()).toEqual(Object.keys(INPUT).sort());
  });

  it('preserves the falsy values a default would swallow', () => {
    // `enabled: false` and an empty `clear_pattern` are the two an "if it is set, copy it"
    // implementation loses — and losing `enabled: false` silently re-enables a rule someone
    // deliberately turned off.
    const off = ruleToInput({ ...STORED, enabled: false, clear_pattern: '' } as EventRule);
    expect(off.enabled).toBe(false);
    expect(off.clear_pattern).toBe('');
  });

  it('keeps a null scope null rather than defaulting it to a node', () => {
    // `node_id: null` is "this rule applies fleet-wide". Turning it into anything else narrows a
    // rule the operator wrote to apply everywhere.
    const wide = ruleToInput({ ...STORED, node_id: null, source_id: null } as EventRule);
    expect(wide.node_id).toBeNull();
    expect(wide.source_id).toBeNull();
  });
});

const ok = { ttl_secs: '3600', min_count: '1', window_secs: '60' };

describe('typedInteger', () => {
  it('reads an empty box as absent, never as zero', () => {
    // The trap: `Number('')` is 0.
    expect(typedInteger('')).toBeUndefined();
    expect(typedInteger('   ')).toBeUndefined();
  });

  it('reads a whole number, and nothing that is not one', () => {
    expect(typedInteger(' 90 ')).toBe(90);
    expect(typedInteger('1.5')).toBeUndefined();
    expect(typedInteger('abc')).toBeUndefined();
  });
});

describe('eventRuleNumberProblem', () => {
  it('accepts values inside the bounds, edges included', () => {
    expect(eventRuleNumberProblem(ok)).toBeNull();
    expect(eventRuleNumberProblem({ ttl_secs: '60', min_count: '100', window_secs: '3600' })).toBeNull();
    expect(eventRuleNumberProblem({ ttl_secs: '604800', min_count: '1', window_secs: '1' })).toBeNull();
  });

  it('names a cleared field — the case that used to be sent as 0', () => {
    expect(eventRuleNumberProblem({ ...ok, ttl_secs: '' })).toBe('ttl_secs');
    expect(eventRuleNumberProblem({ ...ok, min_count: '' })).toBe('min_count');
    expect(eventRuleNumberProblem({ ...ok, window_secs: '' })).toBe('window_secs');
  });

  it('names a value outside the bounds, on either side', () => {
    expect(eventRuleNumberProblem({ ...ok, ttl_secs: '59' })).toBe('ttl_secs');
    expect(eventRuleNumberProblem({ ...ok, ttl_secs: '604801' })).toBe('ttl_secs');
    expect(eventRuleNumberProblem({ ...ok, min_count: '0' })).toBe('min_count');
    expect(eventRuleNumberProblem({ ...ok, window_secs: '3601' })).toBe('window_secs');
  });

  it('reports the first problem in form order', () => {
    expect(eventRuleNumberProblem({ ttl_secs: '', min_count: '', window_secs: '' })).toBe('ttl_secs');
  });

  it('carries the bounds the server enforces', () => {
    // A mirror of `validate_rule` in `api/events.rs`. Nothing compares the two; if the server's
    // bounds move, the server still refuses — this form just stops explaining it first.
    expect(EVENT_RULE_BOUNDS).toEqual({
      ttl_secs: { min: 60, max: 604_800 },
      min_count: { min: 1, max: 100 },
      window_secs: { min: 1, max: 3_600 },
    });
  });
});
