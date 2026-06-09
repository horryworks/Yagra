import { describe, expect, it } from 'vitest';
import { parseAlertEvent } from './sse';

describe('parseAlertEvent', () => {
  it('parses a well-formed alert payload', () => {
    const data = JSON.stringify({
      node: 'n1',
      check: 'c1',
      severity: 'critical',
      state: 'unreachable',
      at_unix_ms: 1000,
      root_cause: null,
      flapping: false,
    });
    const alert = parseAlertEvent(data);
    expect(alert?.node).toBe('n1');
    expect(alert?.severity).toBe('critical');
  });

  it('returns null on malformed JSON', () => {
    expect(parseAlertEvent('{not json')).toBeNull();
  });

  it('returns null when required fields are missing', () => {
    expect(parseAlertEvent(JSON.stringify({ state: 'ok' }))).toBeNull();
  });
});
