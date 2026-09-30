// SPDX-License-Identifier: AGPL-3.0-only
// "Where do I send syslog?" — answered on the Events screen (ADR-055 decision 3).
//
// The Events tab used to promise this in a nav description (`Event sources` → "Syslog and SNMP trap
// listeners") while the screen behind it managed webhook senders and nothing else. That label is
// now `Webhook sources`, which is true — and R1 says that when you shrink a label you owe the
// question it used to promise an answer somewhere reachable. This is that answer.
//
// The data already exists and always did: a poller's heartbeat carries the listeners it has bound
// (`yagra-bus/messages.rs`), core republishes them as `PollerSummary.listeners`, and
// `Settings ▸ Pollers` renders them. Nothing new is collected here — the question and its answer
// were simply two tabs apart.
//
// Pure on purpose. Vitest runs in `environment: 'node'` over `src/**/*.test.ts`, so a `.tsx` test
// would not run at all; the parsing, grouping and kind decisions live here where they are testable
// and the page is left with concatenation.

/** One bound listening endpoint, and everyone listening on it. */
export interface ListenerBinding {
  /** The label's kind prefix, e.g. `syslog` or `trap`. */
  kind: string;
  /** The bind address exactly as the poller reported it, e.g. `0.0.0.0:1514` or `[::]:1514`. */
  bind: string;
  /** Ids of the pollers bound to it, first-seen order. Several is normal — a pool's pollers are
   *  interchangeable and a site may point its devices at any of them. */
  pollers: string[];
}

/**
 * The listener kinds whose traffic lands in **this** log.
 *
 * `flow:` is deliberately excluded even though pollers report it. Flow records go to ClickHouse and
 * are read on the node's Flow tab; listing a flow endpoint here would invite exactly the wrong
 * conclusion — "I am exporting to that address and the event log is empty, so reception is broken".
 * A kind this list does not name is dropped rather than shown under a heading that would misdescribe
 * it, which also means a newer poller advertising a kind this build has never heard of degrades to
 * silence instead of a lie.
 */
export const EVENT_LISTENER_KINDS = ['syslog', 'trap'] as const;

/** The shape this module needs out of `PollerSummary` — an id and its labels. Structural so a test
 *  can build one without the twenty other fields the fleet table cares about. */
export interface ListeningPoller {
  id: string;
  listeners: string[];
}

/** Joins the parts of a lookup key. A NUL cannot occur inside any of the parts, so two different
 *  tuples never join to the same string. Built from its code point rather than written literally:
 *  a raw NUL in the source made git treat this file as binary, so its diffs could not be read. */
const KEY_SEP = String.fromCharCode(0);

/**
 * Group the fleet's raw listener labels into endpoints.
 *
 * ⚠️ **Split on the FIRST colon only.** A bind address contains colons of its own, and an IPv6 one
 * contains several: `syslog:[::]:1514` must read as kind `syslog` bound to `[::]:1514`, not as
 * something bound to `[`. Addresses are `IpAddr`-equivalent throughout this codebase and v6 is in
 * scope from the start, so this is a real input, not a hypothetical.
 *
 * Ordering is `kinds` first, then first-seen bind, so the sentence reads the same on every refresh
 * even though the fleet list does not promise a stable poller order.
 */
export function eventListeners(
  pollers: readonly ListeningPoller[],
  kinds: readonly string[] = EVENT_LISTENER_KINDS,
): ListenerBinding[] {
  const byKey = new Map<string, ListenerBinding>();
  for (const poller of pollers) {
    for (const label of poller.listeners) {
      const at = label.indexOf(':');
      if (at <= 0 || at === label.length - 1) continue; // no kind, or no address
      const kind = label.slice(0, at);
      const bind = label.slice(at + 1);
      if (!kinds.includes(kind)) continue;
      const key = `${kind}${KEY_SEP}${bind}`;
      const found = byKey.get(key);
      if (found) {
        // A poller reporting the same endpoint twice would otherwise be named twice in the line.
        if (!found.pollers.includes(poller.id)) found.pollers.push(poller.id);
      } else {
        byKey.set(key, { kind, bind, pollers: [poller.id] });
      }
    }
  }
  const order = new Map(kinds.map((k, i) => [k, i]));
  return [...byKey.values()].sort(
    (a, b) => (order.get(a.kind) ?? 0) - (order.get(b.kind) ?? 0),
  );
}
