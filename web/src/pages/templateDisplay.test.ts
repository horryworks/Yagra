// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  bodyAfterTitle,
  builtinSource,
  circled,
  describeNote,
  hasOwnTemplate,
  layoutForDisplay,
  readCondition,
  readForDisplay,
  type ConditionWords,
  type Shown,
} from './templateDisplay';
import type { BuiltinSubjectTemplate } from '../types/api';

// The built-in JSM/email subject at fire, as `notify_text::node_subject_template` writes it.
const FIRE =
  '{{ node_name }}{% if node_address and node_address != node_name %} ({{ node_address }}){% endif %} is {{ state }}' +
  '{% if title %}: {{ title }}{% if if_name and ifindex is defined %} on {{ if_name }}' +
  '{% elif ifindex is defined %} on ifIndex {{ ifindex }}{% endif %}{% if row_name %} [{{ row_name }}]{% endif %}{% endif %}';

const WORDS: ConditionWords = {
  labelOf: (n) => n.toUpperCase(),
  numberOf: circled,
  present: (n) => `${n} known`,
  differs: (a, b) => `${a} not ${b}`,
  and: ' & ',
  when: (c) => `only when ${c}`,
  otherwiseWhen: (p, c) => `only when ${p} is not sent and ${c}`,
  otherwise: (p) => `only when ${p} is not sent`,
  inside: (t, p) => `${t} (inside ${p})`,
};

/** The sentence as drawn: a numbered part reads `<n>text</n>`, a kept-or-dropped line `[line]`. */
function drawn(shown: Shown[]): string {
  return shown
    .map((s) =>
      s.kind === 'text'
        ? s.text
        : s.kind === 'var'
          ? `{${s.name}}`
          : s.kind === 'raw'
            ? s.text
            : s.kind === 'part'
              ? `<${s.n}>${drawn(s.pieces)}</${s.n}>`
              : `[${drawn(s.pieces)}]`,
    )
    .join('');
}

describe('the built-in text, read for showing (ADR-197)', () => {
  it('draws variables as tags and each conditional part as a branch, nested where the template nests', () => {
    const pieces = readForDisplay(FIRE);
    expect(pieces.map((p) => p.kind)).toEqual(['var', 'cond', 'text', 'var', 'cond']);
    const address = pieces[1];
    if (address.kind !== 'cond') throw new Error('not a condition');
    expect(address.branches[0].when).toEqual({
      terms: [{ kind: 'differs', name: 'node_address', other: 'node_name' }],
    });
    expect(address.branches[0].pieces).toEqual([
      { kind: 'text', text: ' (' },
      { kind: 'var', name: 'node_address' },
      { kind: 'text', text: ')' },
    ]);

    const title = pieces[4];
    if (title.kind !== 'cond') throw new Error('not a condition');
    const inside = title.branches[0].pieces;
    expect(inside.map((p) => p.kind)).toEqual(['text', 'var', 'cond', 'cond']);
  });

  it('numbers each conditional part in reading order and says its condition once, below (Inc.2)', () => {
    const { shown, legend } = layoutForDisplay(readForDisplay(FIRE));
    expect(drawn(shown)).toBe(
      '{node_name}<1> ({node_address})</1> is {state}<2>: {title}<3> on {if_name}</3><4> on ifIndex {ifindex}</4><5> [{row_name}]</5></2>',
    );
    expect(legend.map((l) => `${circled(l.n)} ${describeNote(l.note, WORDS)}`)).toEqual([
      '① only when NODE_ADDRESS not NODE_NAME',
      '② only when TITLE known',
      '③ only when IF_NAME known & IFINDEX known (inside ②)',
      '④ only when ③ is not sent and IFINDEX known (inside ②)',
      '⑤ only when ROW_NAME known (inside ②)',
    ]);
  });

  it('says a whole kept-or-dropped line at the end of the line, with no number', () => {
    const src =
      'Node: {{ node_name }}\n{% if group is defined %}Folder: {{ group }}\n{% endif %}' +
      '{% if ifindex is defined %}Port: {% if if_name is defined %}{{ if_name }}{% else %}#{{ ifindex }}{% endif %}\n{% endif %}End\n';
    const { shown, legend } = layoutForDisplay(readForDisplay(src));
    expect(drawn(shown)).toBe('Node: {node_name}\n[Folder: {group}\n][Port: <1>{if_name}</1><2>#{ifindex}</2>\n]End\n');
    const folder = shown.find((s) => s.kind === 'line');
    if (folder?.kind !== 'line') throw new Error('no line');
    expect(describeNote(folder.note, WORDS)).toBe('only when GROUP known');
    expect(legend.map((l) => describeNote(l.note, WORDS))).toEqual([
      'only when IF_NAME known',
      'only when ① is not sent',
    ]);
  });

  it('shows a filter-bearing variable as its tag, and what it cannot name as written', () => {
    expect(readForDisplay('{{ threshold | number }} {{ tags | join(", ") }}')).toEqual([
      { kind: 'var', name: 'threshold' },
      { kind: 'text', text: ' ' },
      { kind: 'var', name: 'tags' },
    ]);
    expect(readForDisplay('{{ secret }}{% for x in tags %}')).toEqual([
      { kind: 'raw', text: '{{ secret }}' },
      { kind: 'raw', text: '{% for x in tags %}' },
    ]);
    expect(readCondition('value > 3')).toEqual({ raw: 'value > 3' });
    expect(circled(3)).toBe('③');
    expect(circled(21)).toBe('(21)');
  });

  it('never throws on a template it cannot balance', () => {
    expect(() => readForDisplay('{% endif %}{% if group %}x')).not.toThrow();
    expect(readForDisplay('{{ broken')).toEqual([{ kind: 'text', text: '{{ broken' }]);
  });

  it('draws the body without the title sentence it repeats, and leaves another body whole', () => {
    expect(bodyAfterTitle('T is down\n\nNode: x\n', 'T is down')).toBe('\n\nNode: x\n');
    expect(bodyAfterTitle('Other\n', 'T is down')).toBe('Other\n');
  });

  it('a channel has its own template only when a field holds text', () => {
    expect(hasOwnTemplate({ subject_template: null, body_template: null })).toBe(false);
    expect(hasOwnTemplate({ subject_template: '  ', body_template: undefined })).toBe(false);
    expect(hasOwnTemplate({ subject_template: null, body_template: 'x' })).toBe(true);
  });
});

describe('the copy "Edit a copy of this text" puts in the code editor (ADR-197)', () => {
  const tpl = (subject: string, body: string | null): BuiltinSubjectTemplate[] => [
    { event: 'fire', subject: `${subject}F`, body: body === null ? null : `${subject}F${body}` },
    { event: 'resolve', subject: `${subject}R`, body: body === null ? null : `${subject}R${body}` },
    { event: 'suppress', subject: `${subject}S`, body: body === null ? null : `${subject}S${body}` },
  ];

  it('branches the subject on the event, and the body only on its first line', () => {
    expect(builtinSource(tpl('s', '\n\nState: {{ state }}\n\n'))).toEqual({
      subject: '{% if event == "resolve" %}sR{% elif event == "suppress" %}sS{% else %}sF{% endif %}',
      body: '{% if event == "resolve" %}sR{% elif event == "suppress" %}sS{% else %}sF{% endif %}\n\nState: {{ state }}\n\n',
    });
  });

  it('leaves the body empty when the built-in body is JSON, so it stays the built-in', () => {
    expect(builtinSource(tpl('s', null))?.body).toBe('');
  });

  it('branches the whole body when the events do not share their lines, dropping the newline the server drops', () => {
    const t: BuiltinSubjectTemplate[] = [
      { event: 'fire', subject: 'a', body: 'one\n' },
      { event: 'resolve', subject: 'a', body: 'two\n' },
      { event: 'suppress', subject: 'a', body: 'one\n' },
    ];
    expect(builtinSource(t)).toEqual({
      subject: 'a',
      body: '{% if event == "resolve" %}two{% else %}one{% endif %}',
    });
  });

  it('has nothing to copy when a point in the alert life is missing', () => {
    expect(builtinSource([{ event: 'fire', subject: 'x' }])).toBeNull();
  });
});
