// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree's per-group member cache (A-3 lazy load).
//
// The tree paints from the group skeleton plus the server's per-group health rollup, so it is
// instant at any fleet size; a group's member nodes are fetched only once that group is open. That
// policy needs five pieces of state that only make sense together — what is loaded, what is in
// flight, what came back truncated, what FAILED, and the members themselves — plus a queue, and
// three effects that must not race each other into fetching the same group twice. Kept out of
// NodesPage so the page reads as a page, and so "which groups do we want loaded" stays one question
// with three answers (visible-and-open, the selected group's subtree, and the subtree a filter
// revealed) rather than three tangled effects.
//
// ⚠️ **"Open" is not "on screen", and that is the remaining gap** (ADR-125). Collapse state
// defaults to empty ⇒ every folder open ⇒ a deployment with 500 folders asks for all 501 at once.
// The queue bounds how many of those are in flight; making the *set* itself follow the viewport is
// the next increment. Until then this file is what stops that burst from taking the server down.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../services/api';
import { revealedGroupKeys, UNGROUPED } from '../lib/nodeTree';
import type { NodeGroup, NodeSummary } from '../types/api';

/** Cap on how many groups one filter term may reveal. A one-letter term matches most folders in a
 *  large fleet and each key is one `/nodes/by-group` request — the fan-out this lazy tree exists to
 *  avoid, and here it hangs off a text input. */
const REVEAL_GROUP_CAP = 200;

/** How many `/nodes/by-group` requests may be in flight at once.
 *
 *  🚨 **The browser used to enforce this and quietly stopped** (ADR-125). Over HTTP/1.1 a browser
 *  opens at most 6 connections per origin, so a burst of 501 requests reached the server 6 at a
 *  time whether anyone had designed that or not. ADR-044 made TLS the default, the TLS listener
 *  sets `http2 on`, and HTTP/2 multiplexes — so nginx now forwards up to
 *  `http2_max_concurrent_streams` (128 by default) at once. Each one takes four PostgreSQL
 *  connections concurrently against a pool of 20, which times out, which fails, which is where the
 *  retry loop above used to start.
 *
 *  6 is chosen to match the HTTP/1.1 ceiling **so the two deployment shapes behave the same** —
 *  a plaintext deployment behind someone else's TLS terminator and a default one must not have
 *  different failure modes, which is exactly what made this "impossible to reproduce". */
const MAX_INFLIGHT = 6;

/** How many folders one batched `/nodes/by-group` may name.
 *
 *  ⚠️ **Mirrors `BY_GROUP_BATCH_MAX` in `api/nodes.rs`, where exceeding it is a 400** — the server
 *  refuses rather than trimming, because a trimmed answer is indistinguishable from a complete one.
 *  Keep the two the same, or the client starts writing requests the server rejects. */
const BY_GROUP_BATCH_MAX = 64;

export interface LazyGroupMembers {
  /** Every member loaded so far, flattened — what the tree renders in browse mode. */
  nodes: NodeSummary[];
  /** Which groups have been fetched. The tree shows a placeholder row under the others. */
  loadedGroups: Set<string>;
  /** The groups the active filter revealed (see `revealedGroupKeys`). Returned rather than derived
   *  a second time by the caller, so the set that gets FETCHED and the set the tree draws loading
   *  rows for cannot disagree. Empty while browsing. */
  revealedGroups: Set<string>;
  /** Groups whose fetch failed. They are NOT retried on their own — see `load`'s `.catch` for why
   *  that had to become explicit. The tree draws a failed row for them (with a retry control)
   *  instead of a placeholder that never resolves. */
  failedGroups: Set<string>;
  /** The term matched more groups than the cap, so some matched folders show no members. */
  revealTruncated: boolean;
  /** Whether any group's members came back capped by the server (the page says so). */
  anyTruncated: boolean;
  /** Drop everything so open groups re-fetch. Call after any write that can change membership. */
  invalidate: () => void;
  /** Clear one group's failure so it is fetched again. The ONLY automatic retry is `invalidate`;
   *  everything else goes through here, driven by the operator pressing the failed row's control. */
  retry: (key: string) => void;
}

export function useLazyGroupMembers(opts: {
  groups: NodeGroup[];
  /** False while the group skeleton is still loading: there is no tree to walk yet. */
  ready: boolean;
  /** False in filter mode, where a server-side search owns the tree instead of the cache. */
  browsing: boolean;
  /** The folders currently on screen and still waiting for members (`pendingGroupKeys`, published
   *  by the tree). This is the browse-mode fetch set: it follows the viewport, so a fleet with
   *  hundreds of folders asks for the few the operator can see rather than all of them.
   *
   *  ⚠️ **Its identity must be stable when the contents are**, because it is in this hook's effect
   *  deps. The tree gets that from debouncing the keys as a joined string: the settle it needs
   *  anyway (so a momentum scroll queues where it lands, not everything it passes) compares content
   *  for free, and only publishes when the answer actually changed. */
  visibleGroupKeys: string[];
  /** The selected group, if any. Its direct members load so the detail pane can list them —
   *  independent of `browsing`, since a group stays selected while the operator types a filter. */
  selectedGroupId: string | null;
  /** The APPLIED (debounced) inventory filter. A term matching a group's NAME reveals that group's
   *  whole subtree: its members load so the operator sees the folder's contents rather than an empty
   *  folder. Loaded independently of `browsing`, like the selected subtree — filter mode is exactly
   *  when it matters, because the server search page only carries nodes that matched. A plain string
   *  (not a precomputed array) so a re-render cannot churn the effect on identity alone. */
  filterTerm: string;
}): LazyGroupMembers {
  const { groups, ready, browsing, visibleGroupKeys, selectedGroupId, filterTerm } = opts;

  const [loadedNodes, setLoadedNodes] = useState<Record<string, NodeSummary[]>>({});
  const [loadedGroups, setLoadedGroups] = useState<Set<string>>(new Set());
  const [loadingGroups, setLoadingGroups] = useState<Set<string>>(new Set());
  /** Whether any answer came back capped by the server. A boolean, not a set: the batch form
   *  reports truncation for the whole answer, so naming a folder would be inventing an attribution
   *  the server did not give. The page only ever asked whether ANY was capped. */
  const [anyTruncated, setAnyTruncated] = useState(false);
  const [failedGroups, setFailedGroups] = useState<Set<string>>(new Set());

  /** The fetch queue: keys waiting for a slot, and the keys occupying one.
   *
   *  🚨 **In a ref, not state.** A queue change that re-rendered would change `loadMissing`'s
   *  identity and re-run all three effects — the very churn the failed-set fix exists to stop.
   *  Nothing on screen is derived from it (`loadingGroups` is the state the tree reads), so it has
   *  no business causing a render. */
  const queue = useRef<{ waiting: string[]; inflight: Set<string> }>({
    waiting: [],
    inflight: new Set(),
  });

  /** Whether this core understands the batch form. Flipped to `false` the first time an answer
   *  comes back without its `answered` echo — see `settle` for why that is the only safe reading. */
  const batchSupported = useRef(true);

  /** Start as many queued fetches as the concurrency budget allows, and again as each settles.
   *
   *  A named function expression so it can call itself from `.finally` without a `useCallback`
   *  cycle. It closes over nothing but state setters (stable) and two refs, so `[]` is honest. */
  const pump = useCallback(function pump() {
    const q = queue.current;
    while (q.inflight.size < MAX_INFLIGHT && q.waiting.length > 0) {
      // ⚠️ **The ungrouped bucket can never ride in a batch.** The batch matches
      // `group_id = ANY($2)`, and SQL `NULL` is not a value `ANY` can match — which is precisely
      // why the single-group query keeps `IS NOT DISTINCT FROM`. Take it out and send it alone.
      const ungroupedAt = q.waiting.indexOf(UNGROUPED);
      const keys =
        ungroupedAt >= 0
          ? q.waiting.splice(ungroupedAt, 1)
          : q.waiting.splice(0, batchSupported.current ? BY_GROUP_BATCH_MAX : 1);
      for (const k of keys) q.inflight.add(k);

      // One folder goes through the single-group endpoint, which every core has always understood.
      // Only a real batch needs the echo, and only a real batch can be misread without it.
      const request =
        keys.length === 1
          ? api.getGroupNodes(keys[0] === UNGROUPED ? null : keys[0])
          : api.getGroupNodesBatch(keys);

      request
        .then((res) => {
          if (keys.length === 1) {
            const key = keys[0];
            setLoadedNodes((prev) => ({ ...prev, [key]: res.nodes }));
            setLoadedGroups((prev) => new Set(prev).add(key));
            if (res.truncated) setAnyTruncated(true);
            return;
          }
          // 🚨 **An answer with no `answered` is not an empty answer — it is a DIFFERENT answer.**
          // A core older than ADR-125 ignores the unknown `groups=`, finds no `group=` either, and
          // returns the UNGROUPED bucket with an ordinary 200. Filing those nodes under the folders
          // we asked about would be silently, confidently wrong. So: believe nothing, stop using
          // the batch form, and put the keys back for the one-at-a-time path.
          if (!res.answered) {
            batchSupported.current = false;
            q.waiting.unshift(...keys);
            return;
          }
          const covered = new Set(res.answered);
          const byGroup: Record<string, NodeSummary[]> = {};
          for (const k of keys) if (covered.has(k)) byGroup[k] = [];
          for (const n of res.nodes) {
            if (n.group_id && byGroup[n.group_id]) byGroup[n.group_id].push(n);
          }
          setLoadedNodes((prev) => ({ ...prev, ...byGroup }));
          setLoadedGroups((prev) => {
            const next = new Set(prev);
            for (const k of keys) if (covered.has(k)) next.add(k);
            return next;
          });
          // 🚨 **A folder we asked about that the echo does not claim is a FAILURE, not a wait.**
          // Without this it is in neither `loadedGroups` nor `failedGroups`, so the effects queue
          // it again on the next render, forever — the exact loop this whole change began with,
          // arriving through the door the fix for it opened. Found by the browser suite, whose
          // generated mock echoes a set unrelated to the request: 179 requests and climbing.
          const missing = keys.filter((k) => !covered.has(k));
          if (missing.length > 0) {
            setFailedGroups((prev) => {
              const next = new Set(prev);
              for (const k of missing) next.add(k);
              return next;
            });
          }
          if (res.truncated) setAnyTruncated(true);
        })
        .catch(() => {
          // 🚨 **Recording the failure is what stops an unbounded retry loop, and the loop was
          // real** (ADR-125). This used to be an empty catch whose comment said the group would be
          // "retried when it is next opened/selected". It was not: `.finally` below publishes a
          // fresh `loadingGroups` Set, that changes `loadMissing`'s identity, all three effects
          // re-run, and the key is then in NEITHER the loaded nor the in-flight set — so it fetched
          // again immediately, at whatever speed the server returned failures. With 501 folders in
          // flight the pool times out, the timeouts become failures, and the failures become more
          // load. Measured: the loop saturates the microtask queue so completely that a
          // `setTimeout(50)` never fires (`useLazyGroupMembers.test.ts`).
          //
          // Leaving it out of `loadedGroups` is still right — marking it loaded would show an empty
          // folder forever, which reads as "this folder has no nodes". What was missing is the third
          // set, so the retry becomes a decision rather than a side effect of a Set identity.
          setFailedGroups((prev) => {
            const next = new Set(prev);
            for (const k of keys) next.add(k);
            return next;
          });
        })
        .finally(() => {
          for (const k of keys) q.inflight.delete(k);
          setLoadingGroups((prev) => {
            const next = new Set(prev);
            for (const k of keys) next.delete(k);
            return next;
          });
          pump();
        });
    }
  }, []);

  /** Queue one group's (or the ungrouped bucket's) direct members for fetching.
   *
   *  🚨 **Queues only — it does NOT pump.** The pump is what turns the queue into requests, and
   *  batching only works if there is something to batch: draining after each enqueue leaves exactly
   *  one key in the queue every time, so every folder went out as its own request and the batch form
   *  was never reached. `loadMissing` drains once, after the whole set is in.
   *
   *  ⚠️ **The queue's own membership is what de-duplicates within a single render.** `loadingGroups`
   *  is state, so two effects running in the same commit both see it un-updated and both ask for
   *  the same key — the older comment claimed the in-flight *set* prevented that, and across
   *  renders it does, but not within one. The ref is read and written synchronously, so it does. */
  const enqueue = useCallback((key: string) => {
    const q = queue.current;
    if (q.inflight.has(key) || q.waiting.includes(key)) return;
    q.waiting.push(key);
    setLoadingGroups((prev) => new Set(prev).add(key));
  }, []);

  const loadMissing = useCallback(
    (keys: string[]) => {
      for (const key of keys) {
        if (!loadedGroups.has(key) && !loadingGroups.has(key) && !failedGroups.has(key)) {
          enqueue(key);
        }
      }
      // Drain once, with the whole set queued — see `enqueue`'s note. This is what lets a screenful
      // of folders leave as one request instead of one each.
      pump();
    },
    // ⚠️ All three sets belong here, and `failedGroups` is what makes the effects CONVERGE rather
    // than churn: adding a key changes this identity and re-runs them, but the key is now refused,
    // so nothing fires. Removing one (`retry` / `invalidate`) re-runs them and the key fires once.
    [loadedGroups, loadingGroups, failedGroups, enqueue, pump],
  );

  // The folders the operator can actually SEE waiting for members (ADR-125). The tree derives this
  // from the rows the virtualizer is showing (`pendingGroupKeys`), so the fetch follows the
  // viewport the way the rendering already does.
  //
  // 🚨 **This replaced `visibleOpenGroupKeys(groups, collapsed)`, whose "visible" meant "no
  // collapsed ancestor" rather than "on screen".** Collapse state defaults to empty, so on a
  // deployment with 500 folders that set was all 501 keys on first paint. Nothing about the screen
  // changes: a folder row is drawn from the server rollup (`/fleet/group-summary`) whether or not
  // its members are loaded, which is why this can follow the viewport without the tree going blank.
  useEffect(() => {
    if (!ready || !browsing) return;
    // The ungrouped bucket has no folder row and therefore never appears in `visibleGroupKeys`,
    // but its header always does — and it counts from what is LOADED, not from the server rollup
    // (`/fleet/group-summary` skips ungrouped nodes deliberately). Leaving it to the viewport would
    // show "0" for as long as that header is off screen, which is worse than what this replaced.
    loadMissing([UNGROUPED, ...visibleGroupKeys]);
  }, [ready, browsing, visibleGroupKeys, loadMissing]);

  // The selected group, so the detail pane can show its direct members.
  //
  // ⚠️ **Its subtree used to load too** — `subtreeGroupIds(groups, selectedGroupId)` — which meant
  // selecting a root folder fetched every folder under it (500 requests on the deployment this was
  // measured on). That existed only because the detail pane rolled its tally up from LOADED
  // members; since ADR-125 it reads the same server counts the tree row does, so there is nothing
  // left that needs the descendants.
  useEffect(() => {
    if (!ready || !selectedGroupId) return;
    loadMissing([selectedGroupId]);
  }, [ready, selectedGroupId, loadMissing]);

  const revealedGroups = useMemo(
    () => new Set(revealedGroupKeys(groups, filterTerm, REVEAL_GROUP_CAP)),
    [groups, filterTerm],
  );

  // The subtree a filter revealed by matching a group's own name. Independent of `browsing` for the
  // same reason as the selected subtree: filter mode is when it is needed. The in-flight set is what
  // keeps this from racing the other two into a duplicate request per group.
  useEffect(() => {
    if (!ready || revealedGroups.size === 0) return;
    loadMissing([...revealedGroups]);
  }, [ready, revealedGroups, loadMissing]);

  const invalidate = useCallback(() => {
    setLoadedNodes({});
    setLoadedGroups(new Set());
    setLoadingGroups(new Set());
    setAnyTruncated(false);
    // ⚠️ The failures go too, or a folder that failed once stays failed until the page is
    // reloaded — including across the write that might have fixed it.
    setFailedGroups(new Set());
  }, []);

  const retry = useCallback((key: string) => {
    setFailedGroups((prev) => {
      if (!prev.has(key)) return prev;
      const next = new Set(prev);
      next.delete(key);
      return next;
    });
  }, []);

  const nodes = useMemo(() => Object.values(loadedNodes).flat(), [loadedNodes]);

  return {
    nodes,
    loadedGroups,
    revealedGroups,
    failedGroups,
    revealTruncated: revealedGroups.size >= REVEAL_GROUP_CAP,
    anyTruncated,
    invalidate,
    retry,
  };
}
