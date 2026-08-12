// SPDX-License-Identifier: AGPL-3.0-only
// The batching half of `useEntityNames`: which node ids to ask about, and when to ask.
//
// Extracted from `EntityName.tsx` because it was broken in a way only a test can hold shut, and a
// test beside a `.tsx` is a file nothing runs (testing.md). What it replaces:
//
//   1. The request was fired from a `useEffect` in the component that calls `useEntityNames()`,
//      while the ids are enqueued during the render of its **children** — the cells. A virtualized
//      list renders no rows on its first pass (the scroll element is not measured yet) and then
//      re-renders **by itself** once it is, so the ids arrived in a commit the owner did not take
//      part in and its effect never ran. The row sat on a raw UUID until something unrelated
//      re-rendered the owner. What usually did was `useEntityNames`' own group/profile fetches
//      landing a moment later — which is why the same page showed a name on one load and a UUID on
//      the next. Scheduling from the enqueue instead makes it independent of who rendered.
//   2. An id was marked "already asked" *before* the request, and never unmarked when it failed, so
//      one network blip pinned those rows to a raw UUID for the life of the page.
//   3. `POST /api/v1/node-names` silently truncates its input, so a batch over that cap lost its
//      tail — and those ids were marked asked, so they never recovered either. Reachable: the
//      Active-alerts search asks about every open alert's subject at once.
//
// Scheduling a fetch from render is a side effect in render, which React discourages — but the ref
// mutation this replaces already was one, and the effect it replaces provably does not run. The
// scheduler is injected so the coalescing window is the caller's choice (and the test's).

/**
 * Ids per request.
 *
 * ⚠️ Deliberately **below** the server's own cap — `NODE_NAMES_BATCH_MAX` in
 * `crates/yagra-core/src/api/nodes.rs`, currently 1000 — because that cap `truncate`s rather than
 * refusing: an over-long batch comes back `200 OK` having silently dropped its tail, which is
 * indistinguishable from those nodes having no name. Keeping the margin means the two numbers do
 * not have to agree, only stay ordered.
 */
export const NAME_BATCH_MAX = 500;

/**
 * How many times one id may be sent before it is given up on.
 *
 * ⚠️ This is what keeps "retry after a failure" from becoming a request storm. Not every failure is
 * transient: a caller that passes something the endpoint cannot parse as a UUID gets a `400` for
 * the **whole batch**, every time, and a live page re-renders often enough (SSE) to re-ask on each
 * one. Retrying twice recovers a network blip; retrying forever turns one bad id into a loop that
 * also keeps every id batched with it unresolved.
 */
export const NAME_MAX_ATTEMPTS = 3;

/** One resolved id → name. Ids with no row are simply absent (see `resolve_node_names`). */
export interface NameEntry {
  id: string;
  name: string;
}

export interface NameBatchDeps {
  /** Resolve a batch of ids. Rejects on transport failure; omits ids that have no name. */
  fetchNames: (ids: string[]) => Promise<NameEntry[]>;
  /** Run `flush` after the current work — `(fn) => setTimeout(fn, 0)` in the app. Everything
   *  enqueued before it runs goes out as one request. */
  schedule: (flush: () => void) => void;
  /** Hand resolved names back (the hook's `setState`). Never called with an empty list. */
  onResolved: (entries: NameEntry[]) => void;
}

export interface NameBatcher {
  /** Ask for `id`'s name. Idempotent, cheap, and safe to call during render. */
  request: (id: string) => void;
}

export function createNameBatcher(deps: NameBatchDeps): NameBatcher {
  /** Enqueued, not yet sent. */
  const pending = new Set<string>();
  /** Sent at least once. Kept even when the answer was "no such name": an id with no row must not
   *  be re-asked on every render, or an unresolvable reference becomes a fetch loop. */
  const requested = new Set<string>();
  /** How many times each id has been sent — the retry budget, see `NAME_MAX_ATTEMPTS`. */
  const attempts = new Map<string, number>();
  let scheduled = false;

  function send(ids: string[]): void {
    ids.forEach((id) => {
      requested.add(id);
      attempts.set(id, (attempts.get(id) ?? 0) + 1);
    });
    deps
      .fetchNames(ids)
      .then((entries) => {
        if (entries.length > 0) deps.onResolved(entries);
      })
      .catch(() => {
        // A transport failure is not an answer, so it must not count as having asked. Without this
        // one blip is permanent: the ids stay marked, no render re-enqueues them, and the only
        // recovery is a page reload. Per chunk, so one failed chunk does not re-ask the others —
        // and only while the id has budget left, so a request that can never succeed stops.
        ids.forEach((id) => {
          if ((attempts.get(id) ?? 0) < NAME_MAX_ATTEMPTS) requested.delete(id);
        });
      });
  }

  function flush(): void {
    scheduled = false;
    if (pending.size === 0) return;
    const ids = [...pending];
    pending.clear();
    for (let i = 0; i < ids.length; i += NAME_BATCH_MAX) {
      send(ids.slice(i, i + NAME_BATCH_MAX));
    }
  }

  return {
    request(id: string) {
      if (!id || requested.has(id) || pending.has(id)) return;
      pending.add(id);
      if (scheduled) return;
      scheduled = true;
      deps.schedule(flush);
    },
  };
}
