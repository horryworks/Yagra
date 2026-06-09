import { describe, expect, it } from 'vitest';
import { severityColorVar, severityRank, stateColorVar, stateLabel } from './format';

describe('format', () => {
  it('maps severity to theme color variables', () => {
    expect(severityColorVar('critical')).toBe('var(--sev-critical)');
    expect(severityColorVar('warning')).toBe('var(--sev-warning)');
    expect(severityColorVar('info')).toBe('var(--sev-info)');
  });

  it('maps unreachable state to the critical color', () => {
    expect(stateColorVar('unreachable')).toBe('var(--sev-critical)');
    expect(stateColorVar('ok')).toBe('var(--state-ok)');
  });

  it('ranks severities for sorting', () => {
    expect(severityRank('critical')).toBeGreaterThan(severityRank('warning'));
    expect(severityRank('warning')).toBeGreaterThan(severityRank('info'));
  });

  it('capitalizes state labels', () => {
    expect(stateLabel('maintenance')).toBe('Maintenance');
  });
});
