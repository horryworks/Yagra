// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import en from '../locales/en/alertsConfig.json';
import ja from '../locales/ja/alertsConfig.json';
import type { ChannelTestResult, NotificationChannel } from '../types/api';
import { testVerdict, testWarnings, VERDICT_KEYS, verdictOk, type TestVerdict } from './channelTest';

function channel(over: Partial<NotificationChannel>): NotificationChannel {
  return { id: 'c1', name: 'ops', kind: 'webhook', enabled: true, template_free_layout: false, ...over };
}

function result(over: Partial<ChannelTestResult>): ChannelTestResult {
  return { delivered: true, closed: null, error: null, ...over };
}

describe('testWarnings', () => {
  it('warns that PagerDuty and JSM page someone, and nothing else does', () => {
    expect(testWarnings(channel({ kind: 'pagerduty' })).pages).toBe(true);
    expect(testWarnings(channel({ kind: 'jsm' })).pages).toBe(true);
    expect(testWarnings(channel({ kind: 'webhook' })).pages).toBe(false);
    expect(testWarnings(channel({ kind: 'email' })).pages).toBe(false);
  });

  it('says the payload is unmarked only when the BODY is templated', () => {
    expect(testWarnings(channel({ body_template: '{{ node_name }}' })).unmarkedBody).toBe(true);
    // A subject template keeps the built-in body, which carries the mark.
    expect(testWarnings(channel({ subject_template: 'x' })).unmarkedBody).toBe(false);
    expect(testWarnings(channel({ body_template: '   ' })).unmarkedBody).toBe(false);
  });

  it('notes a disabled channel', () => {
    expect(testWarnings(channel({ enabled: false })).disabled).toBe(true);
    expect(testWarnings(channel({})).disabled).toBe(false);
  });
});

describe('testVerdict', () => {
  it('reads the four outcomes', () => {
    expect(testVerdict(result({}))).toBe('delivered');
    expect(testVerdict(result({ closed: true }))).toBe('deliveredAndClosed');
    expect(testVerdict(result({ closed: false, error: '429' }))).toBe('deliveredButOpen');
    expect(testVerdict(result({ delivered: false, error: 'dns' }))).toBe('failed');
  });

  it('calls an incident left open bad news', () => {
    expect(verdictOk('deliveredButOpen')).toBe(false);
    expect(verdictOk('failed')).toBe(false);
    expect(verdictOk('deliveredAndClosed')).toBe(true);
  });

  it('has a sentence for every verdict in both languages', () => {
    const lookup = (tree: unknown, key: string) =>
      key.split('.').reduce<unknown>((o, k) => (o as Record<string, unknown> | undefined)?.[k], tree);
    for (const [verdict, key] of Object.entries(VERDICT_KEYS) as [TestVerdict, string][]) {
      expect(typeof lookup(en, key), `${verdict} EN`).toBe('string');
      expect(typeof lookup(ja, key), `${verdict} JA`).toBe('string');
    }
  });
});
