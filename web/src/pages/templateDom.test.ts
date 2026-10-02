// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import {
  chipNodes,
  chipSegment,
  insertNodes,
  paintChip,
  readSegments,
  renderSegments,
  textNodes,
  writeChip,
  type ChipLook,
} from './templateDom';
import type { Segment } from './templateModel';

const look: ChipLook = {
  labelOf: (n) => `ラベル:${n}`,
  isOptional: (n) => n === 'group' || n === 'metric',
  missingNote: () => 'may be missing',
};
const text = (t: string): Segment => ({ kind: 'text', text: t });
const v = (name: string, fallback = '', hideLine = false) =>
  ({ kind: 'var', name, fallback, hideLine }) as const;

function field(): HTMLElement {
  const el = document.createElement('div');
  el.contentEditable = 'true';
  document.body.appendChild(el);
  return el;
}

describe('a field as DOM', () => {
  it('reads back exactly the row it drew, labels never included', () => {
    const row: Segment[] = [text('Folder: '), v('group', '—'), text('\n'), v('metric', '', true), text(' end\n')];
    const el = field();
    renderSegments(el, row, look);
    expect(readSegments(el)).toEqual(row);
    expect(el.textContent).toContain('ラベル:group');
    expect(JSON.stringify(readSegments(el))).not.toContain('ラベル');
  });

  it('pads a trailing newline so it shows, and does not read the padding', () => {
    const el = field();
    renderSegments(el, [text('a\n')], look);
    expect(el.querySelectorAll('br')).toHaveLength(2);
    expect(readSegments(el)).toEqual([text('a\n')]);
  });

  it('turns non-breaking spaces back into spaces and drops caret anchors', () => {
    const el = field();
    el.appendChild(document.createTextNode('a b'));
    for (const n of chipNodes(document, v('node_name'), look)) el.appendChild(n);
    expect(readSegments(el)).toEqual([text('a b'), v('node_name')]);
  });

  it('reads a line a browser wrapped in a block of its own', () => {
    const el = field();
    el.innerHTML = 'first<div>second</div>';
    expect(readSegments(el)).toEqual([text('first\nsecond')]);
  });

  it('inserts at the caret, or before the end padding when there is no caret in the field', () => {
    const el = field();
    renderSegments(el, [text('ab')], look);
    const r = document.createRange();
    r.setStart(el.firstChild!, 1);
    r.collapse(true);
    insertNodes(el, r, chipNodes(document, v('state'), look));
    expect(readSegments(el)).toEqual([text('a'), v('state'), text('b')]);

    const tail = field();
    renderSegments(tail, [text('line\n')], look);
    insertNodes(tail, null, textNodes(document, 'x'));
    expect(readSegments(tail)).toEqual([text('line\nx')]);
  });

  it('a tag edited in place is read with its new settings and repainted', () => {
    const el = field();
    renderSegments(el, [v('group', '—')], look);
    const chip = el.querySelector<HTMLElement>('.tpl-chip')!;
    writeChip(chip, { ...chipSegment(chip), hideLine: true });
    paintChip(chip, look);
    expect(readSegments(el)).toEqual([v('group', '—', true)]);
    expect(chip.classList.contains('hides-line')).toBe(true);
    expect(chip.title).toContain('{{ group }}');
  });
});
