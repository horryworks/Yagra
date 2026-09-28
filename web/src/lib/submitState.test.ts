// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { codeOnly, readSources } from '../testSupport/sources';
import {
  done,
  initialSubmitState,
  partialOutcome,
  step,
  submitReducer,
  type SubmitEvent,
  type SubmitState,
} from './submitState';

const run = (events: SubmitEvent[], from: SubmitState = initialSubmitState) =>
  events.reduce(submitReducer, from);

describe('submitReducer', () => {
  it('stays busy after a success, so Save cannot come back while the dialog closes (F1)', () => {
    const s = run([{ type: 'start' }, { type: 'outcome', outcome: done() }]);
    expect(s).toEqual({ busy: true, error: null, settled: false });
  });

  it('a failure frees both buttons and says why; the next attempt clears the message', () => {
    const failed = run([{ type: 'start' }, { type: 'fail', message: 'refused' }]);
    expect(failed).toEqual({ busy: false, error: 'refused', settled: false });
    expect(run([{ type: 'start' }], failed)).toEqual({ busy: true, error: null, settled: false });
  });

  it('a partial batch keeps the dialog open and turns Cancel into Close for good (F3)', () => {
    const partial = run([
      { type: 'start' },
      { type: 'outcome', outcome: { kind: 'keepOpen', message: '1 of 3', refresh: false } },
    ]);
    expect(partial).toEqual({ busy: false, error: '1 of 3', settled: true });
    // A retry that then fails outright does not un-land the first write.
    const retried = run([{ type: 'start' }, { type: 'fail', message: 'down' }], partial);
    expect(retried.settled).toBe(true);
  });

  it('a step that wrote nothing frees the dialog and leaves Cancel as it was', () => {
    expect(run([{ type: 'start' }, { type: 'outcome', outcome: step() }])).toEqual({
      busy: false,
      error: null,
      settled: false,
    });
  });

  it('a refusal the dialog makes itself sends nothing and says why', () => {
    expect(run([{ type: 'refuse', message: 'name required' }])).toEqual({
      busy: false,
      error: 'name required',
      settled: false,
    });
  });
});

describe('partialOutcome', () => {
  const words = ({ applied, requested }: { applied: number; requested: number }) =>
    `${applied} of ${requested}`;

  it('is done when every row was applied, and names both numbers when not', () => {
    expect(partialOutcome(3, 3, words, true)).toEqual(done());
    expect(partialOutcome(1, 3, words, true)).toEqual({
      kind: 'keepOpen',
      message: '1 of 3',
      refresh: true,
    });
  });
});

/**
 * ADR-184 increment 35: a dialog's Cancel + submit pair is drawn by `FormFooter`, and the save
 * that drives it is `useSubmit`. Thirty-three files wrote the pair by hand (increments 35-39).
 *
 * The needle is Cancel wired to close and disabled on the busy flag — the one line every hand copy
 * shares. Not `setBusy(true)`: ten page-level forms set that too, and they are a different
 * contract (the page stays, so busy must be released after a success).
 *
 * ⚠️ The pattern is assembled at runtime and read from code lines only, so this file and a doc
 * comment quoting a footer cannot match.
 */
describe('a dialog saves through useSubmit and draws its footer through FormFooter', () => {
  const NEEDLE = new RegExp(
    ['onClick=\\{(onClose|onCancel)\\}', 'disabled=\\{(busy|saving)\\b'].join('\\s+'),
  );
  const HOME = 'components/ui/FormFooter.tsx';

  /** Dialogs that keep a footer of their own, on purpose. */
  const PERMANENT: Record<string, string> = {
    'components/AddNodeModal/AddNodeModal.tsx':
      'two steps — the probe, then the confirmation of what it found — each with its own buttons',
    'components/MoveByPrefixModal/MoveByPrefixModal.tsx':
      'runs a loop over prefixes and ends on a summary of what moved, not on closing',
  };

  const sources = readSources().map(([p, src]) => [p, codeOnly(src)] as const);
  const handWritten = sources.filter(([, code]) => NEEDLE.test(code)).map(([p]) => p);

  it('no other dialog writes its footer by hand', () => {
    const offenders = handWritten.filter((p) => p !== HOME && !PERMANENT[p]);
    expect(
      offenders,
      `these files write a dialog's Cancel + submit by hand. Use useSubmit and FormFooter ` +
        `(components/ui/FormFooter.tsx):\n  ` +
        offenders.join('\n  '),
    ).toEqual([]);
  });

  it('every listed exception still is one, so the list cannot go stale', () => {
    const stale = Object.keys(PERMANENT).filter((p) => !handWritten.includes(p));
    expect(stale, 'listed, but no longer writes its footer by hand — take it off').toEqual([]);
  });

  it('finds the dialogs it is supposed to be reading', () => {
    // Counts footers written by hand **or** saves through the shared hook, so a walk that found
    // nothing cannot pass — and a dialog that drops its save altogether shows up as a lower
    // number.
    const HOOK = `${'useSubmit'}(`;
    const dialogs = sources.filter(
      ([p, code]) => p !== 'lib/useSubmit.ts' && (NEEDLE.test(code) || code.includes(HOOK)),
    );
    expect(dialogs.length).toBeGreaterThanOrEqual(28);
  });
});
