// SPDX-License-Identifier: AGPL-3.0-only
// `ruleToInput` is a hand-written projection of a stored rule onto the shape the form submits, and
// it is the kind of function whose failure is silent: a field missing here reads and edits fine,
// and is reset to its default the next time anyone saves that rule.
//
// So the assertion that matters is not "it copies these fields" — it is **"it copies every field
// the input type has"**, derived from an object rather than from a hand-written list, so a field
// added to `EventRuleInput` fails here instead of being quietly dropped in production.
import { describe, expect, it } from 'vitest';
import { ruleToInput } from './eventRuleForm';
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
