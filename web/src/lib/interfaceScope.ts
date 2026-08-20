// SPDX-License-Identifier: AGPL-3.0-only
/** The `scope_id` of an interface-scoped threshold rule: `<node-uuid>:<ifindex>` (ADR-076).
 *
 *  ⚠️ **This is a mirror of `yagra_common::thresholds`** (`interface_scope_id` /
 *  `parse_interface_scope_id`), and nothing makes the two agree automatically — the format is a
 *  string, not a type the contract can generate. `interfaceScope.test.ts` pins the cases that
 *  matter, and the Rust side has the same table; if you change the shape, change both in one
 *  commit. The failure it prevents is quiet: a UI that composes an id the server parses
 *  differently creates a rule that is stored, listed, and matches no port.
 *
 *  The port is validated as digits rather than by `Number()`, because the server refuses anything
 *  that is not the canonical spelling. `Number('+7')` is 7 and `Number(' 7')` is 7, so a lenient
 *  reader here would show a rule the server would have refused, and would let the UI compose one
 *  spelling while the engine keyed on another. */

/** Build the scope id for one port of one node. */
export function interfaceScopeId(nodeId: string, ifindex: number): string {
  return `${nodeId}:${ifindex}`;
}

/** Split a scope id into `[nodeId, ifindex]`. `ifindex` is `null` when the value is not exactly
 *  the canonical shape — the caller then shows what it has rather than inventing a port. */
export function splitInterfaceScopeId(scopeId: string): [string, number | null] {
  const at = scopeId.indexOf(':');
  if (at < 0) return [scopeId, null];
  const node = scopeId.slice(0, at);
  const port = scopeId.slice(at + 1);
  if (port === '' || !/^\d+$/.test(port)) return [node, null];
  const n = Number(port);
  // Above 2^32-1 there is no ifIndex to name; the server refuses it, so neither does this invent one.
  if (!Number.isSafeInteger(n) || n > 0xffffffff) return [node, null];
  return [node, n];
}

/** Whether `scopeId` is a well-formed interface scope id — the same question the server's
 *  `invalid_scope_id` check asks, so a form can disable Save before the round trip. */
export function isInterfaceScopeId(scopeId: string): boolean {
  const [node, port] = splitInterfaceScopeId(scopeId);
  return port !== null && UUID_RE.test(node);
}

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
