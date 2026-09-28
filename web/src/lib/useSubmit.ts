// SPDX-License-Identifier: AGPL-3.0-only
// A dialog's save, once (ADR-184 increment 35).
//
// The judgement — what each answer does to `busy`, `error` and the Cancel/Close label — is
// `submitReducer` in `submitState.ts`, where a node-environment test runs. This file is the wiring:
// one request at a time, the server's message or the fallback, and who hears about the result.
// What stays in the dialog is what is genuinely per-dialog: its fields, whether they are valid,
// and which call to make.
import { useCallback, useEffect, useReducer, useRef } from 'react';
import { errMsg } from '../services/api';
import {
  initialSubmitState,
  submitReducer,
  WordedFailure,
  type SubmitOutcome,
  type SubmitState,
} from './submitState';

export interface SubmitOptions<T> {
  /** Shown when the failure carries no message of the server's (a dropped connection). */
  errorFallback: string;
  /** A failure this dialog words itself (a 409 it can explain better than the server). `null`
   *  falls through to the server's message or `errorFallback`. */
  describeError?: (e: unknown) => string | null;
  /** Everything asked for happened. Usually: close the dialog and re-read the list. */
  onDone: (value: T) => void;
  /** Part of it happened and the outcome asked for a refresh: re-read the list, keep the dialog. */
  onSaved?: () => void;
}

export interface Submit<T> extends SubmitState {
  /** Send. Ignored while a request is in flight (F2), so a double click sends once.
   *  `errorFallback` overrides the hook's for this call — a dialog with two actions (issue and
   *  revoke) that fail in different words. */
  submit: (run: () => Promise<SubmitOutcome<T>>, errorFallback?: string) => void;
  /** Show a message without sending — a field the dialog itself found wrong. */
  refuse: (message: string) => void;
}

export function useSubmit<T = void>(opts: SubmitOptions<T>): Submit<T> {
  const [state, dispatch] = useReducer(submitReducer, initialSubmitState);
  // A ref, not `state.busy`: two clicks in one tick both read the same render's state.
  const inFlight = useRef(false);
  const latest = useRef(opts);
  useEffect(() => {
    latest.current = opts;
  });

  const submit = useCallback((run: () => Promise<SubmitOutcome<T>>, fallback?: string) => {
    if (inFlight.current) return;
    inFlight.current = true;
    dispatch({ type: 'start' });
    let started: Promise<SubmitOutcome<T>>;
    try {
      started = run();
    } catch (e) {
      started = Promise.reject(e);
    }
    started.then(
      (outcome) => {
        dispatch({ type: 'outcome', outcome });
        if (outcome.kind === 'done') {
          // Left set: the dialog is closing, and a click in the meantime must not send again.
          latest.current.onDone(outcome.value);
          return;
        }
        inFlight.current = false;
        if (outcome.kind === 'keepOpen' && outcome.refresh) latest.current.onSaved?.();
      },
      (e: unknown) => {
        inFlight.current = false;
        const { describeError, errorFallback } = latest.current;
        const message =
          e instanceof WordedFailure
            ? e.message
            : (describeError?.(e) ?? errMsg(e, fallback ?? errorFallback));
        dispatch({ type: 'fail', message });
      },
    );
  }, []);

  const refuse = useCallback((message: string) => dispatch({ type: 'refuse', message }), []);

  return { ...state, submit, refuse };
}
