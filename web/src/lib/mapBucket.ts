// SPDX-License-Identifier: AGPL-3.0-only
// Appending to a Map of arrays, in O(1) rather than O(k²).
//
// The shape this replaces — `map.set(k, [...(map.get(k) ?? []), v])` — reallocates and copies the
// whole bucket on every append, so filling one bucket of k entries costs 1+2+…+k element copies.
// It appeared five times across the inventory tree and the suppression index (ADR-125), and every
// one of them rebuilds a parent→children map from *every* group, on *every* call — which the tree
// does once per arriving `/nodes/by-group` response.
//
// ⚠️ Insertion order is preserved, and callers depend on it: siblings arrive in the API's order and
// are either sorted afterwards or deliberately left in it.

/** Append `value` to `map`'s bucket for `key`, creating the bucket if this is the first one. */
export function pushInto<K, V>(map: Map<K, V[]>, key: K, value: V): void {
  const bucket = map.get(key);
  if (bucket) bucket.push(value);
  else map.set(key, [value]);
}
