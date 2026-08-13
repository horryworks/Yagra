// SPDX-License-Identifier: AGPL-3.0-only
// URL-param codecs for list filters. Moved here from `components/EventLog/eventFilters.ts` and
// generalized over the key: the Events page's node filter was never event-specific, and Alert
// history wants the same two functions for `node_id` and `group_id`. Writing them a second time
// would have been the third copy extensibility.md §3 warns about.
//
// **When a filter belongs in the URL.** Generalizing the argument at `NodeDetail/RangeControl.tsx`:
// the URL carries a filter only when the URL is that filter's single source of truth. If a store or
// a hook already holds it, a second copy in the query string is a second source of truth, and the
// two will disagree on any navigation that sets only one. Nothing else holds a list's filter state,
// so filters go here; the chart `Range` (backed by `useRangeStore`) deliberately does not.
//
// Callers must write with `setSearchParams(params, { replace: true })` — otherwise every settled
// keystroke pushes a history entry and Back stops meaning "the previous screen".

/** Read an open-valued filter: a trimmed, non-empty value, or `null` when absent or blank.
 *
 *  Named for the first two callers, which were ids, but the codec is about the *shape* — an opaque
 *  string with no closed set to validate against — so a free-text search term uses it too. Anything
 *  with a fixed vocabulary belongs in `readEnumParam` instead, where an unknown value falls back. */
export function readIdParam(params: URLSearchParams, key: string): string | null {
  const v = params.get(key)?.trim();
  return v ? v : null;
}

/** Write (or clear) an open-valued filter. Clearing **deletes the key** rather than writing an empty
 *  value, so the URL stays clean and "is anything set" can be read off the params. */
export function writeIdParam(params: URLSearchParams, key: string, id: string | null): void {
  if (id) params.set(key, id);
  else params.delete(key);
}

/**
 * Read a filter whose values are a closed set, falling back to `fallback` for anything unknown.
 *
 * Unknown means unknown for any reason — a stale bookmark from an older release, a hand-edited URL,
 * a typo. A list screen recovers by showing the default view; it must never render a filter control
 * whose value is not one of its own options, which is what makes this different from the API edge,
 * where an unknown enum token is a 400 (a typo there must not silently widen a search).
 */
export function readEnumParam<T extends string>(
  params: URLSearchParams,
  key: string,
  allowed: readonly T[],
  fallback: T,
): T {
  const v = params.get(key)?.trim();
  return allowed.includes(v as T) ? (v as T) : fallback;
}

/** Write (or clear) a closed-set filter. A value equal to `fallback` **deletes the key**: the
 *  default view has no query string, so `?` in the URL always means "something is narrowing this". */
export function writeEnumParam<T extends string>(
  params: URLSearchParams,
  key: string,
  value: T,
  fallback: T,
): void {
  if (value === fallback) params.delete(key);
  else params.set(key, value);
}

/**
 * Read a filter that names **several** values of a closed set, as one comma-joined string.
 *
 * Returns the tokens re-joined in `allowed`'s order so the same selection always produces the same
 * string — the property `encodeSet` exists for, and it matters here twice: the value is a `useEffect`
 * dependency key, and a shared link must compare equal to the same view reached by clicking.
 *
 * Unknown tokens are dropped rather than rejected, which is [`readEnumParam`]'s rule and for the
 * same reason: a stale bookmark from a release with one more severity should show the rest of the
 * filter, not a blank screen. ⚠️ The **API edge does the opposite** and 400s an unknown token — a
 * dropped token there would widen the answer while the operator reads it as the narrower one.
 * These two rules disagree on purpose; do not "make them consistent".
 */
export function readSetParam<T extends string>(
  params: URLSearchParams,
  key: string,
  allowed: readonly T[],
): string {
  const want = new Set(
    (params.get(key) ?? '')
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean),
  );
  return allowed.filter((v) => want.has(v)).join(',');
}

/** Write (or clear) a multi-valued closed-set filter. An empty selection **deletes the key**. */
export function writeSetParam(params: URLSearchParams, key: string, value: string): void {
  if (value) params.set(key, value);
  else params.delete(key);
}
