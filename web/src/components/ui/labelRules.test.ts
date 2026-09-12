// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import {
  LABELS_MAX,
  LABEL_MAX,
  LABEL_PROBLEMS,
  addLabel,
  labelProblem,
  labelProblems,
  labelsAreValid,
  normalizeLabel,
  removeLabel,
  splitPastedLabels,
} from './labelRules';

// The boundary rules three dialogs share (ADR-135 inc. 2). They mirror
// `api/nodes.rs::validated_labels`, and checking them here is not duplication for its own sake: it
// turns a 400 into an inline message before the save.

describe('one label', () => {
  it('trims, and refuses one that is empty once trimmed', () => {
    expect(normalizeLabel('  JAPAN  ')).toBe('JAPAN');
    expect(labelProblem('', [])).toBe('empty');
    expect(labelProblem('   ', [])).toBe('empty');
  });

  it('measures in code points, not UTF-16 units', () => {
    // `.length` would count an astral character twice, so a label of emoji or CJK would pass here
    // and be refused by the server — a disagreement the operator experiences as "Save did nothing".
    expect(labelProblem('a'.repeat(LABEL_MAX), [])).toBeNull();
    expect(labelProblem('a'.repeat(LABEL_MAX + 1), [])).toBe('tooLong');
    expect(labelProblem('🙂'.repeat(LABEL_MAX), [])).toBeNull();
  });

  it('accepts spaces and non-Latin text, which the old key rule refused', () => {
    // 🚨 The shape this replaced restricted the *key* to `[A-Za-z0-9_.:-]` and left the value free.
    // With one string the free rule is the one that survives: a label is a word a person reads off
    // a badge, so a site name in Japanese and a two-word phrase both have to work.
    expect(labelProblem('松山本社', [])).toBeNull();
    expect(labelProblem('spare parts', [])).toBeNull();
    expect(labelProblem('a_b-c.d:e', [])).toBeNull();
  });

  it('refuses a control character, which is invisible and would split two identical-looking labels', () => {
    // Built by code point rather than written as a literal: a control character in a source file is
    // invisible in a diff, and this module was authored with two of them in it on the first pass.
    expect(labelProblem('JA' + String.fromCharCode(9) + 'PAN', [])).toBe('control');
    expect(labelProblem('JAPAN' + String.fromCharCode(127), [])).toBe('control');
  });

  it('refuses one more than the cap, but not a duplicate of something already there', () => {
    const full = Array.from({ length: LABELS_MAX }, (_, i) => `l${i}`);
    expect(labelProblem('another', full)).toBe('tooMany');
    // Re-adding one that is present is not "too many" — it is a no-op, and reporting an error for
    // it would tell the operator off for asking for a state that is already true.
    expect(labelProblem('l0', full)).toBeNull();
  });

  it('names every problem the message catalogue has a sentence for', () => {
    // `ChipInput` builds `t(`field.tagErr.${problem}`)` at runtime, and `i18nEnumKeys.test.ts`
    // iterates this array to prove both locales carry every member. This pins the other direction:
    // the array is the set the rules can actually produce.
    const produced = new Set<string>([
      labelProblem('', []) ?? '',
      labelProblem('a'.repeat(LABEL_MAX + 1), []) ?? '',
      labelProblem(String.fromCharCode(1), []) ?? '',
      labelProblem('x', Array.from({ length: LABELS_MAX }, (_, i) => `l${i}`)) ?? '',
    ]);
    expect([...produced].sort()).toEqual([...LABEL_PROBLEMS].sort());
  });
});

describe('a list of labels', () => {
  it('adds, refuses and removes', () => {
    expect(addLabel([], ' JAPAN ')).toEqual(['JAPAN']);
    expect(addLabel(['JAPAN'], 'JAPAN')).toEqual(['JAPAN']);
    expect(addLabel(['JAPAN'], '  ')).toEqual(['JAPAN']);
    expect(removeLabel(['JAPAN', 'core'], 'JAPAN')).toEqual(['core']);
  });

  it('splits a paste on commas and newlines, and not on spaces', () => {
    // 🚨 Splitting on a space would silently turn `Matsuyama 本社` into two labels.
    expect(splitPastedLabels('JAPAN, core\nedge')).toEqual(['JAPAN', 'core', 'edge']);
    expect(splitPastedLabels('Matsuyama 本社')).toEqual(['Matsuyama 本社']);
    expect(splitPastedLabels(' , ,')).toEqual([]);
  });

  it('accepts a list exactly at the cap', () => {
    // ⚠️ The regression this pins: checking each label against the *whole* list rather than against
    // the list without it makes every member of a full list report `tooMany`, so a node at the cap
    // could never be saved again — not even to remove one.
    const full = Array.from({ length: LABELS_MAX }, (_, i) => `l${i}`);
    expect(labelsAreValid(full)).toBe(true);
    expect(labelsAreValid([...full, 'one-more'])).toBe(false);
  });

  it('marks only the labels that are actually unsavable', () => {
    // 🚨 The case migration 0109 creates: the old column allowed 128-character values and never
    // rejected a control character, so a node can read back carrying a label these rules refuse.
    // The dialog marks that one chip and stays saveable once it is deleted — blocking Save with no
    // indication of which chip is at fault would leave the operator with no way forward.
    const list = ['JAPAN', 'x'.repeat(LABEL_MAX + 1)];
    const marked = labelProblems(list);
    expect(marked.get('JAPAN')).toBeUndefined();
    expect(marked.get('x'.repeat(LABEL_MAX + 1))).toBe('tooLong');
    expect(labelsAreValid(list)).toBe(false);
    expect(labelsAreValid(removeLabel(list, 'x'.repeat(LABEL_MAX + 1)))).toBe(true);
  });
});
