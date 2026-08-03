// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { NotificationChannel, TemplatePreview } from '../types/api';
import {
  draftFor,
  hasTemplate,
  isBuiltin,
  isDirty,
  previewMatches,
  previewView,
  saveBody,
  variableSnippet,
} from './channelTemplate';

function channel(over: Partial<NotificationChannel> = {}): NotificationChannel {
  return {
    id: 'c1',
    name: 'ops webhook',
    kind: 'webhook',
    enabled: true,
    ...over,
  };
}

function preview(over: Partial<TemplatePreview> = {}): TemplatePreview {
  return { subject: 'node x is critical', body: '{}', problems: [], ...over };
}

describe('draft round trip', () => {
  it('opens a channel with no override as two empty fields', () => {
    expect(draftFor(channel())).toEqual({ subject: '', body: '' });
    expect(isBuiltin(draftFor(channel()))).toBe(true);
    expect(hasTemplate(channel())).toBe(false);
  });

  it('opens a templated channel with what is stored', () => {
    const c = channel({ subject_template: '{{ node_name }}', body_template: '{}' });
    expect(draftFor(c)).toEqual({ subject: '{{ node_name }}', body: '{}' });
    expect(hasTemplate(c)).toBe(true);
  });

  // Blank must mean "use the built-in wording", not "send an empty subject" — the second is a
  // silent way for an operator to lose every notification's headline.
  it('turns a blank field into null rather than an empty template', () => {
    expect(saveBody({ subject: '   ', body: '' })).toEqual({ subject: null, body: null });
    expect(saveBody({ subject: '{{ severity }}', body: '' })).toEqual({
      subject: '{{ severity }}',
      body: null,
    });
  });

  it('preserves leading and trailing space inside a non-blank template', () => {
    // Only *entirely* blank collapses; whitespace can be deliberate inside a body.
    expect(saveBody({ subject: ' [{{ severity }}] ', body: '' }).subject).toBe(
      ' [{{ severity }}] ',
    );
  });
});

describe('dirty tracking', () => {
  it('is clean when nothing was typed', () => {
    const c = channel({ subject_template: '{{ node_name }}' });
    expect(isDirty(c, draftFor(c))).toBe(false);
  });

  it('is dirty when a field changes', () => {
    const c = channel({ subject_template: '{{ node_name }}' });
    expect(isDirty(c, { subject: '{{ node_id }}', body: '' })).toBe(true);
  });

  // Clearing a stored template is a real, savable change — not "back to unchanged".
  it('is dirty when a stored template is cleared', () => {
    const c = channel({ subject_template: '{{ node_name }}' });
    expect(isDirty(c, { subject: '', body: '' })).toBe(true);
  });

  // Typing whitespace into a field that was already empty is not an edit worth enabling Save for.
  it('is not dirty when whitespace is typed into an empty field', () => {
    expect(isDirty(channel(), { subject: '  ', body: '\n' })).toBe(false);
  });
});

describe('preview panel', () => {
  it('reads as ok when nothing fell back', () => {
    const v = previewView(preview({ subject: 'CRITICAL core-sw-01' }));
    expect(v.tone).toBe('ok');
    expect(v.subject).toBe('CRITICAL core-sw-01');
    expect(v.problems).toEqual([]);
  });

  // A preview with problems still shows output, because delivery falls back the same way — the
  // panel is showing what would actually be sent, which is the whole point.
  it('still shows the text that would be sent when a field fell back', () => {
    const v = previewView(
      preview({
        subject: 'node x is critical',
        problems: [{ field: 'subject', reason: 'render', message: 'no such filter' }],
      }),
    );
    expect(v.tone).toBe('problem');
    expect(v.subject).toBe('node x is critical');
    expect(v.problems).toEqual(['subject: no such filter']);
  });

  // The JSON verdict is the server's; the UI only displays it. Deciding it here would mirror the
  // Rust `body_must_be_json` match with nothing keeping the two in step.
  it('takes the JSON verdict from the server and leaves it null when absent', () => {
    expect(previewView(preview({ json_valid: false })).jsonValid).toBe(false);
    expect(previewView(preview({ json_valid: true })).jsonValid).toBe(true);
    expect(previewView(preview()).jsonValid).toBeNull();
  });
});

describe('stale previews', () => {
  const draft = { subject: 'a', body: 'b' };

  it('has nothing to show before the first preview', () => {
    expect(previewMatches(null, draft)).toBe(false);
  });

  it('matches the draft it was rendered for', () => {
    expect(previewMatches({ subject: 'a', body: 'b' }, draft)).toBe(true);
  });

  // A preview left on screen after an edit reads as confirmation of text that was never rendered.
  it('goes stale as soon as either field changes', () => {
    expect(previewMatches({ subject: 'a', body: 'b' }, { subject: 'a2', body: 'b' })).toBe(false);
    expect(previewMatches({ subject: 'a', body: 'b' }, { subject: 'a', body: 'b2' })).toBe(false);
  });
});

describe('variable palette', () => {
  it('inserts an always-present variable plainly', () => {
    expect(variableSnippet('node_name', true)).toBe('{{ node_name }}');
  });

  // An optional variable renders empty when absent, so the palette hands over a default — a
  // template written by clicking should not produce a blank the operator did not expect.
  it('gives an optional variable a default', () => {
    expect(variableSnippet('value', false)).toBe('{{ value | default("—") }}');
  });
});
