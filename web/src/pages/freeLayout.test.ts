// SPDX-License-Identifier: AGPL-3.0-only
// Free layout (ADR-199): a template whose line breaks and indentation around its tags are layout.
// What the server does with such a template is `notify_render.rs::lay_out`; these pin what the
// editor sends, when it refuses the tags view, and the laid-out copy of the built-in text.
import { describe, expect, it } from 'vitest';
import type { NotificationChannel } from '../types/api';
import { draftFor, isDirty, saveBody, withFreeLayout } from './channelTemplate';
import { builtinSource, laidOutBuiltinSource } from './templateDisplay';
import { openTemplate } from './templateModel';

const channel = (over: Partial<NotificationChannel> = {}): NotificationChannel => ({
  id: 'c1',
  name: 'ops jsm',
  kind: 'jsm',
  enabled: true,
  template_free_layout: false,
  ...over,
});

const LINES = '\n\nSeverity:  {{ severity }}\n';
const BUILTIN = [
  { event: 'fire', subject: 'F', body: `F${LINES}` },
  { event: 'resolve', subject: 'R', body: `R${LINES}` },
  { event: 'suppress', subject: 'S', body: `S${LINES}` },
] as const;

describe('what the editor sends', () => {
  it('sends free layout only with text to lay out', () => {
    expect(saveBody({ subject: 'x', body: '', freeLayout: true })).toEqual({ subject: 'x', body: null, free_layout: true });
    expect(saveBody({ subject: ' ', body: '', freeLayout: true })).toEqual({ subject: null, body: null });
    expect(saveBody({ subject: 'x', body: '' })).toEqual({ subject: 'x', body: null });
  });

  it('reads the switch from the channel and counts turning it as an edit', () => {
    const c = channel({ subject_template: 'x', template_free_layout: true });
    expect(draftFor(c)).toEqual({ subject: 'x', body: '', freeLayout: true });
    expect(isDirty(c, draftFor(c))).toBe(false);
    expect(isDirty(c, { subject: 'x', body: '' })).toBe(true);
  });
});

describe('the tags view', () => {
  it('does not open a laid-out template, and says why', () => {
    const r = openTemplate({ subject: '{{ state }}', body: null, free_layout: true }, null);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error.reason).toBe('freeLayout');
    expect(openTemplate({ subject: '{{ state }}', body: null, free_layout: false }, null).ok).toBe(true);
    // Nothing stored is the built-in, whatever the switch says.
    expect(openTemplate({ subject: null, body: null, free_layout: true }, null).ok).toBe(true);
  });
});

describe('the laid-out copy of the built-in text', () => {
  it('puts each event tag on its own line and indents the text under it', () => {
    const laid = laidOutBuiltinSource([...BUILTIN])!;
    expect(laid.subject).toBe(
      ['{% if event == "resolve" %}', '  R', '{% elif event == "suppress" %}', '  S', '{% else %}', '  F', '{% endif %}'].join(
        '\n',
      ),
    );
    // The shared lines follow once, giving up one leading line break to the subject's own.
    expect(laid.body).toBe(`${laid.subject}\n${LINES.slice(1)}`);
  });

  it('turning the switch swaps the copy for its other spelling, and leaves typed text alone', () => {
    const copy = builtinSource([...BUILTIN]);
    const laid = laidOutBuiltinSource([...BUILTIN]);
    const on = withFreeLayout({ ...copy! }, true, copy, laid);
    expect(on).toEqual({ ...laid, freeLayout: true });
    expect(withFreeLayout(on, false, copy, laid)).toEqual(copy);
    expect(withFreeLayout({ subject: 'mine', body: '' }, true, copy, laid)).toEqual({
      subject: 'mine',
      body: '',
      freeLayout: true,
    });
  });
});
