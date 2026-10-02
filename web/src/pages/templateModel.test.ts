// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  backToFire,
  builtinDraft,
  builtinTemplate,
  effective,
  followsFire,
  isBlank,
  jsmTitle,
  JSM_MESSAGE_MAX_CHARS,
  openTemplate,
  parseField,
  PREVIEW_SAMPLES,
  sameSegments,
  sampleForEvent,
  saveRequest,
  serializeField,
  serializeSegments,
  subjectIsBuiltin,
  visualChanged,
  withField,
  writeOwn,
  type Segment,
  type VisualTemplate,
} from './templateModel';

const text = (t: string): Segment => ({ kind: 'text', text: t });
const v = (name: string, fallback = '', hideLine = false): Segment => ({ kind: 'var', name, fallback, hideLine });

/** The server's answer for `GET /notification-channels/builtin-template`, as of ADR-039 Inc.2. */
const BUILTIN = [
  { event: 'fire', subject: 'node {{ node_id }} is {{ state }}' },
  { event: 'resolve', subject: 'resolved: node {{ node_id }} recovered' },
  { event: 'suppress', subject: 'rolled up: node {{ node_id }} suppressed under upstream' },
] as const;
const DRAFT = builtinDraft([...BUILTIN]);

function reads(src: string) {
  const r = parseField(src);
  if (!r.ok) throw new Error(`did not read: ${r.error.reason} ${r.error.snippet}`);
  return r.branches;
}

describe('writing and reading one row', () => {
  it('round-trips every shape the editor writes', () => {
    const rows: Segment[][] = [
      [v('severity'), text(' '), v('subject_name'), text(' is '), v('state')],
      [text('Folder: '), v('group', '—')],
      [text('Tags: '), v('tags')],
      [text('a\n'), v('metric', '', true), text(' = '), v('value', '', true), text('\nend')],
      [text('quote " and \\ and 日本語: '), v('profile', 'なし "x"')],
      [text('literal {{ braces }} and {% this %} and {# that #} and {{{')],
    ];
    for (const row of rows) {
      const src = serializeSegments(row);
      expect(sameSegments(reads(src).fire, row), src).toBe(true);
      // And the text is stable: writing what was read gives the same text back.
      expect(serializeSegments(reads(src).fire)).toBe(src);
    }
  });

  it('writes the template text an operator would have typed', () => {
    expect(serializeSegments([v('severity'), text(' '), v('subject_name')])).toBe(
      '{{ severity }} {{ subject_name }}',
    );
    expect(serializeSegments([v('group', '—')])).toBe('{{ group | default("—") }}');
    expect(serializeSegments([v('tags')])).toBe('{{ tags | join(", ") }}');
  });

  it('wraps a hidden line with its newline, so leaving it out leaves no blank line', () => {
    const src = serializeSegments([text('a\n'), v('metric', '—', true), text(' high\nb')]);
    expect(src).toBe('a\n{% if metric is defined %}{{ metric }} high\n{% endif %}b');
    // The last line has no newline to take with it.
    expect(serializeSegments([text('a\n'), v('metric', '', true)])).toBe(
      'a\n{% if metric is defined %}{{ metric }}{% endif %}',
    );
  });

  // A `{` the operator typed at the end of a piece used to meet the `{` of whatever the serializer
  // wrote next - a variable, `{% endif %}`, `{% else %}` - and make an opener: `Alert {{{ x }}`.
  it('keeps a brace at the end of a piece text, whatever follows it', () => {
    const rows: Segment[][] = [
      [text('Alert {'), v('node_name')],
      [text('a {'), v('metric', '', true)],
      [text('{'), v('severity'), text('{')],
    ];
    for (const row of rows) {
      const src = serializeSegments(row);
      expect(src).not.toMatch(/\{\{\{|\{\{%|\{\{#/);
      expect(sameSegments(reads(src).fire, row), src).toBe(true);
    }
    const model: VisualTemplate = {
      fire: { subject: [text('fire {')], body: [] },
      resolve: { subject: [text('resolve {')], body: null },
      suppress: { subject: null, body: null },
    };
    const src = serializeField(model, 'subject')!;
    expect(src).not.toMatch(/\{\{%/);
    const b = reads(src);
    expect(b.fire).toEqual([text('fire {')]);
    expect(b.resolve).toEqual([text('resolve {')]);
  });

  it('reads both quote styles and the joined list in either spelling', () => {
    expect(reads(`{{ group | default('n/a') }}`).fire).toEqual([v('group', 'n/a')]);
    expect(reads(`{{ tags | join(', ') }}`).fire).toEqual([v('tags')]);
  });
});

describe('what cannot be shown as tags opens as code, with the reason', () => {
  const cases: [string, string][] = [
    ['{% for t in tags %}{{ t }}{% endfor %}', 'statement'],
    ['{{ node_name | upper }}', 'expression'],
    ['{{ tags }}', 'expression'],
    ['{{ nonsense }}', 'unknownVariable'],
    ['{# note #}hello', 'comment'],
    ['{{- node_name }}', 'whitespaceControl'],
    ['{{ node_name', 'unclosed'],
    ['x {% if metric is defined %}{{ metric }}{% endif %}', 'lineCondition'],
    ['{% if metric is defined %}a\nb\n{% endif %}', 'lineCondition'],
    ['{% if metric is defined %}{{ value }}\n{% endif %}', 'lineCondition'],
    ['{% if event == "resolve" %}a{% endif %}', 'eventBranch'],
    ['{% if event == "resolve" %}a{% else %}b{% endif %} trailing', 'eventBranch'],
    ['a {% if event == "resolve" %}b{% else %}c{% endif %}', 'eventBranch'],
  ];
  for (const [src, reason] of cases) {
    it(`${reason}: ${src}`, () => {
      const r = parseField(src);
      expect(r.ok).toBe(false);
      if (!r.ok) expect(r.error.reason).toBe(reason);
    });
  }
});

describe('points in the alert life', () => {
  const model: VisualTemplate = {
    fire: { subject: [v('subject_name'), text(' down')], body: [text('fire body')] },
    resolve: { subject: [v('subject_name'), text(' up')], body: [text('fire body')] },
    suppress: { subject: null, body: null },
  };

  it('branches only where a row differs from fire', () => {
    expect(serializeField(model, 'subject')).toBe(
      '{% if event == "resolve" %}{{ subject_name }} up{% else %}{{ subject_name }} down{% endif %}',
    );
    // The body is the same at resolve, so it gets no branch.
    expect(serializeField(model, 'body')).toBe('fire body');
  });

  it('reads a branched field back into the same tabs', () => {
    const opened = openTemplate(
      { subject: serializeField(model, 'subject'), body: serializeField(model, 'body') },
      DRAFT,
    );
    expect(opened.ok).toBe(true);
    if (!opened.ok) return;
    expect(followsFire(opened.model, 'suppress')).toBe(true);
    expect(sameSegments(effective(opened.model, 'resolve', 'subject'), model.resolve.subject!)).toBe(true);
    // The body never branched, so at resolve it follows fire rather than holding a copy.
    expect(opened.model.resolve.body).toBeNull();
    expect(sameSegments(effective(opened.model, 'resolve', 'body'), model.fire.body)).toBe(true);
  });

  it('reads all three arms, with a hidden line inside one', () => {
    const src =
      '{% if event == "resolve" %}R{% elif event == "suppress" %}S {{ root_cause_name | default("?") }}' +
      '{% else %}{% if metric is defined %}{{ metric }}\n{% endif %}F{% endif %}';
    const b = reads(src);
    expect(b.resolve).toEqual([text('R')]);
    expect(b.suppress).toEqual([text('S '), v('root_cause_name', '?')]);
    expect(b.fire).toEqual([v('metric', '', true), text('\nF')]);
  });

  it('writing own text copies fire, and going back drops it', () => {
    const own = writeOwn(model, 'suppress');
    expect(own.suppress?.subject).toEqual(model.fire.subject);
    expect(backToFire(own, 'suppress').suppress).toEqual({ subject: null, body: null });
    expect(backToFire(own, 'suppress', 'body').suppress.subject).toEqual(model.fire.subject);
    // Editing one field of a tab that followed fire gives that field, and only it, text of its own.
    const edited = withField(model, 'suppress', 'subject', [text('x')]);
    expect(edited.suppress).toEqual({ subject: [text('x')], body: null });
    expect(model.suppress).toEqual({ subject: null, body: null });
  });
});

describe("the built-in draft", () => {
  it('reads the server built-in subject into tags', () => {
    expect(DRAFT?.fire).toEqual([text('node '), v('node_id'), text(' is '), v('state')]);
  });

  it('opens a channel with no template on the draft subject and an empty body', () => {
    const opened = openTemplate({ subject: null, body: null }, DRAFT);
    expect(opened.ok).toBe(true);
    if (!opened.ok) return;
    expect(opened.model.fire.subject).toEqual(DRAFT!.fire);
    expect(opened.model.resolve.subject).toEqual(DRAFT!.resolve);
    expect(opened.model.resolve.body).toBeNull();
    expect(isBlank(opened.model.fire.body)).toBe(true);
  });

  it('saves an untouched draft as the built-in, not as its text', () => {
    const model = builtinTemplate(DRAFT);
    expect(subjectIsBuiltin(model, DRAFT)).toBe(true);
    expect(saveRequest(model, DRAFT)).toEqual({ subject: null, body: null });
    // Editing one tab makes the whole subject a template, every point in the alert's life included.
    const edited = withField(model, 'fire', 'subject', [text('Down: '), v('subject_name')]);
    expect(saveRequest(edited, DRAFT)).toEqual({
      subject:
        '{% if event == "resolve" %}resolved: node {{ node_id }} recovered' +
        '{% elif event == "suppress" %}rolled up: node {{ node_id }} suppressed under upstream' +
        '{% else %}Down: {{ subject_name }}{% endif %}',
      body: null,
    });
  });

  // Found by the Tier1 walk: the draft subject differs at every point in the alert's life, and the
  // body used to be copied into resolve and suppress along with it - so text typed into fire's body
  // saved with two empty branches beside it, and recovered alerts went out with no body.
  it('a body typed at fire on the draft is the body at every point in the alert life', () => {
    const typed = withField(builtinTemplate(DRAFT), 'fire', 'body', [text('Down: '), v('subject_name')]);
    expect(saveRequest(typed, DRAFT)).toEqual({ subject: null, body: 'Down: {{ subject_name }}' });
    for (const e of ['resolve', 'suppress'] as const) {
      expect(effective(typed, e, 'body')).toEqual(typed.fire.body);
    }
  });

  it('keeps a stored body when the subject is still the built-in', () => {
    const opened = openTemplate({ subject: null, body: 'hello {{ node_name }}' }, DRAFT);
    expect(opened.ok && saveRequest(opened.model, DRAFT)).toEqual({ subject: null, body: 'hello {{ node_name }}' });
  });

  // Opening a template the editor would spell differently must not count as an edit: Save would
  // be offered straight away and would rewrite the operator's text.
  it('a stored template the editor only spells differently is not a change', () => {
    const stored = { subject: `{{node_name}} {{ group | default('n/a') }}`, body: null };
    const opened = openTemplate(stored, DRAFT);
    expect(opened.ok).toBe(true);
    if (!opened.ok) return;
    expect(visualChanged(stored, opened.model, DRAFT)).toBe(false);
    expect(visualChanged(stored, withField(opened.model, 'fire', 'subject', [text('x')]), DRAFT)).toBe(true);
    // Fire's built-in subject stored as text is not a change either.
    const builtin = { subject: BUILTIN[0].subject, body: null };
    const b = openTemplate(builtin, DRAFT);
    expect(b.ok && visualChanged(builtin, b.model, DRAFT)).toBe(false);
    // Nothing stored, nothing touched.
    expect(visualChanged({ subject: null, body: null }, builtinTemplate(DRAFT), DRAFT)).toBe(false);
  });

  it('without a draft from the server, opens empty and saves an empty subject as built-in', () => {
    expect(builtinDraft(null)).toBeNull();
    const opened = openTemplate({ subject: null, body: null }, null);
    expect(opened.ok && isBlank(opened.model.fire.subject)).toBe(true);
    expect(opened.ok && saveRequest(opened.model, null)).toEqual({ subject: null, body: null });
  });
});

describe('preview samples', () => {
  it('a tab keeps its sample when it already matches, otherwise takes the first for that event', () => {
    expect(sampleForEvent('fire', 'threshold')).toBe('threshold');
    expect(sampleForEvent('resolve', 'threshold')).toBe('recovered');
    expect(sampleForEvent('suppress', 'nodeDown')).toBe('rolledUp');
    for (const s of PREVIEW_SAMPLES) expect(sampleForEvent(s.event, s.id)).toBe(s.id);
  });

  it('splits a JSM title where JSM cuts it, counting characters rather than bytes', () => {
    const long = 'あ'.repeat(JSM_MESSAGE_MAX_CHARS + 5);
    const t = jsmTitle(long);
    expect(Array.from(t.kept)).toHaveLength(JSM_MESSAGE_MAX_CHARS);
    expect(t.cut).toBe('あああああ');
    expect(t.length).toBe(JSM_MESSAGE_MAX_CHARS + 5);
    expect(jsmTitle('short').cut).toBe('');
  });
});
