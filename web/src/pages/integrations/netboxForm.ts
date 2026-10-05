// SPDX-License-Identifier: AGPL-3.0-only
// What the NetBox server form refuses before Save, so the field says so instead of a sentence
// under it (ADR-200).
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`).
//
// ⚠️ A second copy of a rule, and deliberately the lenient half of it. The backend is the authority:
// `netbox.rs::validate_base_url` answers `base_url_blocked` for an IP literal that
// `yagra_common::url_check::is_ssrf_blocked` refuses. This mirrors that for IP literals only and
// never resolves a hostname — the backend does not either. Anything it cannot parse it leaves to
// the backend's own validation.

/** The four numbers of a dotted IPv4 literal, or `null`. `URL` has already turned `127.1` and
 *  `0x7f000001` into `127.0.0.1`, so only the canonical spelling arrives here. */
function v4Parts(host: string): number[] | null {
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
  if (!m) return null;
  const parts = m.slice(1).map(Number);
  return parts.every((p) => p <= 255) ? parts : null;
}

function v4Blocked([a, b, c, d]: number[]): boolean {
  return (
    a === 127 || // loopback
    (a === 169 && b === 254) || // link-local, incl. the cloud metadata address
    (a === 0 && b === 0 && c === 0 && d === 0) || // unspecified
    (a >= 224 && a <= 239) || // multicast
    (a === 255 && b === 255 && c === 255 && d === 255) // broadcast
  );
}

/** The eight 16-bit groups of an IPv6 literal (no brackets), or `null`. Expects `URL`'s
 *  canonical form, where an embedded IPv4 tail has already been rewritten as two groups. */
function v6Groups(host: string): number[] | null {
  if (!/^[0-9a-f:]+$/i.test(host)) return null;
  const halves = host.split('::');
  if (halves.length > 2) return null;
  const side = (s: string) => (s === '' ? [] : s.split(':').map((g) => parseInt(g, 16)));
  const head = side(halves[0]);
  const tail = halves.length === 2 ? side(halves[1]) : [];
  const fill = 8 - head.length - tail.length;
  if (halves.length === 1 ? head.length !== 8 : fill < 0) return null;
  const groups = [...head, ...Array<number>(Math.max(fill, 0)).fill(0), ...tail];
  return groups.every((g) => Number.isInteger(g) && g >= 0 && g <= 0xffff) ? groups : null;
}

function v6Blocked(g: number[]): boolean {
  // `::ffff:a.b.c.d` is judged as the IPv4 address it carries, as the backend does.
  if (g.slice(0, 5).every((x) => x === 0) && g[5] === 0xffff) {
    return v4Blocked([g[6] >> 8, g[6] & 0xff, g[7] >> 8, g[7] & 0xff]);
  }
  const zeroHead = g.slice(0, 7).every((x) => x === 0);
  return (
    (zeroHead && (g[7] === 0 || g[7] === 1)) || // unspecified, loopback
    (g[0] & 0xff00) === 0xff00 || // multicast
    (g[0] & 0xffc0) === 0xfe80 // link-local
  );
}

/**
 * Does the backend refuse this base URL for naming a loopback, link-local, multicast or
 * unspecified IP? `false` for anything else, including a URL that does not parse — saying
 * "refused" about a typo would send the operator looking for the wrong problem.
 */
export function baseUrlRefused(raw: string): boolean {
  let host: string;
  try {
    host = new URL(raw.trim()).hostname;
  } catch {
    return false;
  }
  if (host.startsWith('[') && host.endsWith(']')) {
    const groups = v6Groups(host.slice(1, -1));
    return groups !== null && v6Blocked(groups);
  }
  const parts = v4Parts(host);
  return parts !== null && v4Blocked(parts);
}

/** Is what was pasted into the CA certificate box a private key? Only the certificate belongs
 *  there; a key would be stored and sent on every sync for nothing. */
export function pemIsPrivateKey(text: string): boolean {
  return /-----BEGIN [A-Z ]*PRIVATE KEY-----/.test(text);
}
