// SPDX-License-Identifier: AGPL-3.0-only
// One closed-set choice held in the URL — a page's tab, a report's chip (ADR-153).
//
// A thin router binding over `readEnumParam` / `writeEnumParam`, whose rules are the point and are
// tested in `filterParams.test.ts`: an unknown value reads as the fallback (a stale bookmark opens
// the default view, never a control showing a value it does not offer), and the fallback is written
// as *no key at all*, so a bare URL is the default view.
//
// ⚠️ **One handler, one write.** The setter builds from this render's params and commits at once.
// A handler that also writes a filter must not call it beside another `setSearchParams` — the second
// write would restore what the first changed (ADR-053). Such a handler writes both into one
// `URLSearchParams` with `writeEnumParam` directly.

import { useCallback } from 'react';
import { useSearchParams } from 'react-router-dom';
import { readEnumParam, writeEnumParam } from './filterParams';

export function useEnumParam<T extends string>(
  key: string,
  allowed: readonly T[],
  fallback: T,
): [T, (next: T) => void] {
  const [params, setParams] = useSearchParams();
  const value = readEnumParam(params, key, allowed, fallback);
  const set = useCallback(
    (next: T) => {
      const p = new URLSearchParams(params);
      writeEnumParam(p, key, next, fallback);
      // `replace`: switching a tab or a chip is not a place Back should step through.
      setParams(p, { replace: true });
    },
    [params, setParams, key, fallback],
  );
  return [value, set];
}
