// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { CHANNEL_KINDS } from '../types/api';
import { JSON_SKELETON, prettyJson, TEMPLATE_FORMS } from './templateForm';

const text = (tokens: ReturnType<typeof prettyJson>) => (tokens ?? []).map((t) => t.text).join('');

describe('the template form per channel kind (ADR-197 Inc.2)', () => {
  it('gives a webhook no subject, and PagerDuty a template only when an alert fires', () => {
    expect(Object.keys(TEMPLATE_FORMS).sort()).toEqual([...CHANNEL_KINDS].sort());
    expect(TEMPLATE_FORMS.webhook.subject).toBeNull();
    expect(TEMPLATE_FORMS.pagerduty).toMatchObject({ subject: 'summary', body: 'customDetails', json: true });
    expect(TEMPLATE_FORMS.pagerduty.unusedAt).toEqual(['resolve', 'suppress']);
    for (const k of ['jsm', 'email'] as const) {
      expect(TEMPLATE_FORMS[k]).toMatchObject({ body: 'text', json: false, unusedAt: [] });
    }
  });
});

describe('JSON laid out one value per line (ADR-197 Inc.2)', () => {
  it('re-indents the document without changing a character of a value', () => {
    const src = '{"node":"a","breach":{"value":94.2,"threshold":90.0},"tags":[],"flapping":false,"root_cause":null}';
    const tokens = prettyJson(src);
    expect(text(tokens)).toBe(
      '{\n  "node": "a",\n  "breach": {\n    "value": 94.2,\n    "threshold": 90.0\n  },\n  "tags": [],\n  "flapping": false,\n  "root_cause": null\n}',
    );
    // What it is coloured as.
    expect(tokens?.find((t) => t.text === '"node"')?.kind).toBe('key');
    expect(tokens?.find((t) => t.text === '"a"')?.kind).toBe('string');
    expect(tokens?.find((t) => t.text === '90.0')?.kind).toBe('number');
    expect(tokens?.find((t) => t.text === 'null')?.kind).toBe('literal');
    // Re-reading the laid-out text gives the same document.
    expect(JSON.parse(text(tokens))).toEqual(JSON.parse(src));
  });

  it('keeps a key-like string value a value, and a colon inside a string a character', () => {
    const tokens = prettyJson('["a:b", {"k": "v"}]');
    expect(text(tokens)).toBe('[\n  "a:b",\n  {\n    "k": "v"\n  }\n]');
    expect(tokens?.find((t) => t.text === '"a:b"')?.kind).toBe('string');
  });

  it('says it is not JSON rather than guessing', () => {
    expect(prettyJson('core-sw-01 is down')).toBeNull();
    expect(prettyJson('{"a":')).toBeNull();
  });

  it('the skeleton is a JSON object once its variables are filled', () => {
    const filled = JSON_SKELETON.replace(/\{\{[^}]*\}\}/g, '"x"');
    expect(Object.keys(JSON.parse(filled) as object)).toEqual([
      'node',
      'address',
      'alert',
      'event',
      'state',
      'severity',
      'at',
      'dedup_key',
    ]);
  });
});
