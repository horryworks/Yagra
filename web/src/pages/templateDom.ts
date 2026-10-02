// SPDX-License-Identifier: AGPL-3.0-only
// The editable field of the visual template editor, as DOM (ADR-039 Inc.2): drawing a row of
// segments into a `contenteditable` element, reading one back, and putting a variable tag where
// the caret is.
//
// Kept out of the component for the reason the model is: Vitest does not run a `.tsx`, and this is
// the half where the browser does surprising things - a newline typed at the end of a field needs a
// second `<br>` to show, a non-breaking space arrives where a space was typed, and a caret parked
// after a tag needs a character to sit on. `templateDom.test.ts` runs it under jsdom.
//
// A tag is a `<span contenteditable="false">` carrying its variable on `data-*`; the text inside it
// is the operator's-language label and is never read back, so a label in Japanese cannot leak into
// the saved template.

import type { Segment } from './templateModel';

/** The class a tag carries. */
export const CHIP_CLASS = 'tpl-chip';

/** A zero-width space: something for the caret to sit on after a tag. Never read back. */
const CARET_ANCHOR = '​';

export interface ChipLook {
  /** The operator's-language name of a variable. */
  labelOf: (name: string) => string;
  /** Whether some alerts lack this variable (`always_present: false`). */
  isOptional: (name: string) => boolean;
  /** The tooltip text for a tag whose variable may be missing. */
  missingNote: (seg: Extract<Segment, { kind: 'var' }>) => string;
}

/** Draw a tag's label and marks from its `data-*`. */
export function paintChip(chip: HTMLElement, look: ChipLook): void {
  const seg = chipSegment(chip);
  chip.textContent = look.labelOf(seg.name);
  chip.classList.toggle('is-optional', look.isOptional(seg.name));
  chip.classList.toggle('hides-line', seg.hideLine);
  if (look.isOptional(seg.name)) {
    const mark = chip.ownerDocument.createElement('span');
    mark.className = 'tpl-chip-mark';
    mark.setAttribute('aria-hidden', 'true');
    mark.textContent = seg.hideLine ? '⊘' : '?';
    chip.appendChild(mark);
  }
  chip.title = `{{ ${seg.name} }}${look.isOptional(seg.name) ? ` — ${look.missingNote(seg)}` : ''}`;
}

/** A tag element for one variable. */
export function makeChip(doc: Document, seg: Extract<Segment, { kind: 'var' }>, look: ChipLook): HTMLElement {
  const chip = doc.createElement('span');
  chip.className = CHIP_CLASS;
  chip.contentEditable = 'false';
  chip.setAttribute('role', 'button');
  chip.tabIndex = -1;
  writeChip(chip, seg);
  paintChip(chip, look);
  return chip;
}

/** Store a variable on a tag. */
export function writeChip(chip: HTMLElement, seg: Extract<Segment, { kind: 'var' }>): void {
  chip.dataset.var = seg.name;
  chip.dataset.fallback = seg.fallback;
  chip.dataset.hide = seg.hideLine ? '1' : '';
}

/** The variable a tag holds. */
export function chipSegment(chip: HTMLElement): Extract<Segment, { kind: 'var' }> {
  return {
    kind: 'var',
    name: chip.dataset.var ?? '',
    fallback: chip.dataset.fallback ?? '',
    hideLine: chip.dataset.hide === '1',
  };
}

function isChip(node: Node): node is HTMLElement {
  return node.nodeType === 1 && (node as HTMLElement).classList.contains(CHIP_CLASS);
}

/** Mark the trailing `<br>` a browser needs to show an empty last line, so it is not read as one. */
function padTrailingBreak(el: HTMLElement): void {
  const last = el.lastChild;
  if (last && last.nodeName === 'BR' && !(last as HTMLElement).dataset.filler) {
    const pad = el.ownerDocument.createElement('br');
    pad.dataset.filler = '1';
    el.appendChild(pad);
  }
}

/** Replace a field's content with a row. */
export function renderSegments(el: HTMLElement, segments: readonly Segment[], look: ChipLook): void {
  const doc = el.ownerDocument;
  el.replaceChildren();
  for (const s of segments) {
    if (s.kind === 'var') {
      el.appendChild(makeChip(doc, s, look));
      continue;
    }
    s.text.split('\n').forEach((part, i) => {
      if (i > 0) el.appendChild(doc.createElement('br'));
      if (part !== '') el.appendChild(doc.createTextNode(part));
    });
  }
  padTrailingBreak(el);
}

/** Read a field back into a row. */
export function readSegments(el: HTMLElement): Segment[] {
  const out: Segment[] = [];
  const push = (text: string) => {
    if (text === '') return;
    const last = out[out.length - 1];
    if (last && last.kind === 'text') out[out.length - 1] = { kind: 'text', text: last.text + text };
    else out.push({ kind: 'text', text });
  };
  const walk = (node: Node) => {
    node.childNodes.forEach((child) => {
      if (child.nodeType === 3) {
        push((child.nodeValue ?? '').replaceAll(CARET_ANCHOR, '').replaceAll(' ', ' '));
        return;
      }
      if (child.nodeType !== 1) return;
      if (isChip(child)) {
        out.push(chipSegment(child));
        return;
      }
      const tag = child.nodeName;
      if (tag === 'BR') {
        if ((child as HTMLElement).dataset.filler && child === el.lastChild) return;
        push('\n');
        return;
      }
      // A browser that ignored the Enter handler wraps a new line in a block of its own.
      if (tag === 'DIV' || tag === 'P') {
        if (out.length > 0) push('\n');
        walk(child);
        return;
      }
      walk(child);
    });
  };
  walk(el);
  return out;
}

/** Where to insert: the given range when it is inside the field, otherwise the end of the field. */
function insertionRange(el: HTMLElement, saved: Range | null): Range {
  if (saved && el.contains(saved.startContainer)) return saved;
  const r = el.ownerDocument.createRange();
  const last = el.lastChild;
  if (last && last.nodeName === 'BR' && (last as HTMLElement).dataset.filler) {
    // Before the padding `<br>`, or the insertion lands on a line the field does not show.
    r.setStartBefore(last);
    r.collapse(true);
  } else {
    r.selectNodeContents(el);
    r.collapse(false);
  }
  return r;
}

/**
 * Put nodes at the caret (or the end), replacing any selection, and leave the caret after them.
 * Returns the range the caret now sits at, for the caller to hand to the selection.
 */
export function insertNodes(el: HTMLElement, saved: Range | null, nodes: Node[]): Range {
  const range = insertionRange(el, saved);
  range.deleteContents();
  const frag = el.ownerDocument.createDocumentFragment();
  for (const n of nodes) frag.appendChild(n);
  const last = nodes[nodes.length - 1];
  range.insertNode(frag);
  const after = el.ownerDocument.createRange();
  if (last) after.setStartAfter(last);
  else after.setStart(range.startContainer, range.startOffset);
  after.collapse(true);
  padTrailingBreak(el);
  return after;
}

/** The nodes for inserting a variable: the tag, and something for the caret to sit on after it. */
export function chipNodes(doc: Document, seg: Extract<Segment, { kind: 'var' }>, look: ChipLook): Node[] {
  return [makeChip(doc, seg, look), doc.createTextNode(CARET_ANCHOR)];
}

/** The nodes for pasted or typed text, with newlines as `<br>`. */
export function textNodes(doc: Document, text: string): Node[] {
  const nodes: Node[] = [];
  text.split(/\r?\n/).forEach((part, i) => {
    if (i > 0) nodes.push(doc.createElement('br'));
    if (part !== '') nodes.push(doc.createTextNode(part));
  });
  return nodes;
}
