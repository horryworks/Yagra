// SPDX-License-Identifier: AGPL-3.0-only
// Whether an edit to a NetBox server's address needs its token typed again (ADR-178 決定 3).
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`).
//
// ⚠️ A second copy of a rule, and deliberately the lenient half of it. The backend
// (`api/netbox.rs::update_netbox_server`, comparing `netbox::origin_of`) is the authority and
// answers 400 `token_required_for_new_address`; this exists only so the form says so before Save
// rather than after. Anything it cannot parse it leaves to the backend's own validation.

/** The scheme, host and port of a typed URL, or `null` when it is not an absolute URL.
 *
 *  `URL.origin` lower-cases the host and drops a default port, which is what the backend's
 *  `url::Url` does too — so `HTTP://NetBox:80/x` and `http://netbox` are the same address here and
 *  there. */
function originOf(raw: string): string | null {
  try {
    const u = new URL(raw.trim());
    return u.protocol === 'http:' || u.protocol === 'https:' ? u.origin : null;
  } catch {
    return null;
  }
}

/**
 * Does saving `typed` send the stored token to a different scheme, host or port than `stored`?
 * When it does, the form requires the token to be typed again. A different path on the same host
 * does not count — that is the same server moved under a `BASE_PATH`.
 */
export function addressChangeNeedsToken(stored: string, typed: string): boolean {
  const before = originOf(stored);
  const after = originOf(typed);
  return before !== null && after !== null && before !== after;
}
