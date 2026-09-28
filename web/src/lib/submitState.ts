// SPDX-License-Identifier: AGPL-3.0-only
// What a dialog's save does to the dialog — pure, so a node-environment test can run it
// (ADR-184 increment 35).
//
// Thirty-odd dialogs each held the same `busy` + `error` pair and the same `.then(onDone)` /
// `.catch(setError; setBusy(false))`, and they had drifted on the details: some re-enabled Save for
// a moment after a success, none stopped a second click in the same tick, and two batch dialogs
// kept saying "Cancel" after the server had already applied part of what they asked. The rules
// are here once; `useSubmit.ts` is the wiring and `FormFooter.tsx` the buttons.

export type SubmitOutcome<T = void> =
  /** Everything asked for happened. The dialog is about to close. */
  | { kind: 'done'; value: T }
  /** Something happened and the dialog stays: a batch the server applied to some of its rows
   *  (`message` says which), or an action whose result is shown in the dialog itself (a
   *  download, `message` null). `refresh` asks the caller to re-read the list behind it first,
   *  when what is on screen is no longer true. */
  | { kind: 'keepOpen'; message: string | null; refresh: boolean }
  /** A step that wrote nothing — a lookup whose answer the dialog shows next (the organizations
   *  a Meraki key can see). Busy ends; Cancel stays "Cancel". */
  | { kind: 'step' };

export interface SubmitState {
  /** A request is in flight — or it succeeded and the dialog is closing. */
  busy: boolean;
  /** What to tell the operator, in the dialog. */
  error: string | null;
  /** A write has already landed, so dismissing the dialog keeps it: "Close", not "Cancel". */
  settled: boolean;
}

export type SubmitEvent =
  | { type: 'start' }
  /** Refused before anything was sent (a field the dialog itself found wrong). */
  | { type: 'refuse'; message: string }
  | { type: 'outcome'; outcome: SubmitOutcome<unknown> }
  | { type: 'fail'; message: string };

export const initialSubmitState: SubmitState = { busy: false, error: null, settled: false };

export function submitReducer(s: SubmitState, e: SubmitEvent): SubmitState {
  switch (e.type) {
    case 'start':
      return { ...s, busy: true, error: null };
    case 'refuse':
      return { ...s, busy: false, error: e.message };
    case 'fail':
      return { ...s, busy: false, error: e.message };
    case 'outcome':
      switch (e.outcome.kind) {
        // F1: busy stays true. The dialog is closing; Save must not come back for the frames
        // between the answer and the unmount.
        case 'done':
          return { ...s, busy: true, error: null };
        // F3: once part of a batch has landed it stays landed, so "Close" holds even if a retry
        // then fails outright.
        case 'keepOpen':
          return { busy: false, error: e.outcome.message, settled: true };
        case 'step':
          return { ...s, busy: false, error: null };
      }
  }
}

/** A failure the dialog has already put into words: thrown from a save whose answer was a
 *  refusal but not an exception of the API's (a two-stage save that reports which stage failed).
 *  `useSubmit` shows its message as it is. */
export class WordedFailure extends Error {}

/** See the `step` outcome. */
export function step(): SubmitOutcome<never> {
  return { kind: 'step' };
}

export function done(): SubmitOutcome<void>;
export function done<T>(value: T): SubmitOutcome<T>;
export function done<T>(value?: T): SubmitOutcome<T | undefined> {
  return { kind: 'done', value };
}

/**
 * The answer to a batch that reports how many of its rows it applied. All of them ⇒ done; fewer ⇒
 * keep the dialog open with `message`. `applied < requested` is normal, not an error — a row can
 * have been deleted, or be outside the caller's folders (ADR-124 決定 7) — so the dialog says both
 * numbers rather than claiming the count it asked for.
 */
export function partialOutcome(
  applied: number,
  requested: number,
  message: (n: { applied: number; requested: number }) => string,
  refresh: boolean,
): SubmitOutcome<void> {
  if (applied >= requested) return done();
  return { kind: 'keepOpen', message: message({ applied, requested }), refresh };
}
