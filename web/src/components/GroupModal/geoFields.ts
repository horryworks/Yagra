// SPDX-License-Identifier: AGPL-3.0-only
// The map-pin fields of the group dialog, as pure functions.
//
// Split out because `web/vitest.config.ts` runs only `src/**/*.test.ts` in a `node` environment —
// a test written next to the `.tsx` is a file nothing runs. The judgement worth testing is the
// both-or-neither rule and the coordinate ranges, which the server enforces too
// (`PUT /api/v1/node-groups/{id}/geo` returns 400 `invalid_coordinates`). Checking here as well is
// not duplication for its own sake: it turns a round trip into an inline message.

import type { NodeGroup } from '../../types/api';

/** The two inputs, held as strings so a half-typed coordinate is representable. */
export interface GeoDraft {
  latitude: string;
  longitude: string;
}

/** An existing group's pin as editable text. A null coordinate becomes an empty field. */
export function geoDraftFrom(group: NodeGroup | undefined): GeoDraft {
  return {
    latitude: group?.latitude == null ? '' : String(group.latitude),
    longitude: group?.longitude == null ? '' : String(group.longitude),
  };
}

/** Whether the draft differs from what the group already has, so an unchanged dialog does not
 *  issue a pointless second request (the pin is saved by its own endpoint, after the group body). */
export function geoChanged(draft: GeoDraft, group: NodeGroup | undefined): boolean {
  const before = geoDraftFrom(group);
  return before.latitude !== draft.latitude.trim() || before.longitude !== draft.longitude.trim();
}

/** The id of the ancestor whose pin this folder already sits on, or null if there is nothing
 *  useful to say. Answers the question an operator has when they open a rack folder, see two empty
 *  coordinate boxes, and cannot tell whether the rack is missing from the map or already covered
 *  by its site.
 *
 *  It reports the **server's** answer (`geo_source`/`geo_group`), never a fresh walk — the whole
 *  point of resolving inheritance in one place is that the client does not get to have a second
 *  opinion about where a folder is. That is also why a pending parent change suppresses it: the
 *  server answered for the stored parent, and a stale "inherited from Tokyo" while the operator is
 *  mid-move to Osaka would be worse than saying nothing.
 *
 *  Suppressed too once the draft carries its own coordinates, since the folder is then its own pin.
 */
export function inheritedPin(
  group: NodeGroup | undefined,
  draft: GeoDraft,
  pendingParent: string | null,
): string | null {
  if (!group || group.geo_source !== 'inherited' || !group.geo_group) return null;
  if (draft.latitude.trim() !== '' || draft.longitude.trim() !== '') return null;
  if (pendingParent !== (group.parent_id ?? null)) return null;
  return group.geo_group;
}

/** Every reason a coordinate pair is refused. `as const` so the i18n coverage test can walk it: the
 *  dialog renders `t(`err.${pin.error}`)` with no fallback. */
export const GEO_PROBLEMS = ['geoPair', 'geoNumber', 'geoRange'] as const;

/** Why a coordinate pair cannot be saved. */
export type GeoProblem = (typeof GEO_PROBLEMS)[number];

/** The draft → the `PUT` body, or the first reason it cannot be sent.
 *
 *  Both fields or neither: half a coordinate pair is not a location. Clearing both is meaningful —
 *  it removes the pin — so it is a valid body rather than an error. */
export function geoBodyFrom(
  draft: GeoDraft,
): { body: { latitude: number | null; longitude: number | null } } | { error: GeoProblem } {
  const lat = draft.latitude.trim();
  const lon = draft.longitude.trim();
  if (lat === '' && lon === '') return { body: { latitude: null, longitude: null } };
  if (lat === '' || lon === '') return { error: 'geoPair' };

  const latitude = Number(lat);
  const longitude = Number(lon);
  // `Number('')` is 0 and `Number('12abc')` is NaN — the empty case is already handled above, so
  // only the NaN guard is left. `Number.isFinite` also rejects Infinity, which parses from 'Inf'.
  if (!Number.isFinite(latitude) || !Number.isFinite(longitude)) return { error: 'geoNumber' };
  if (latitude < -90 || latitude > 90 || longitude < -180 || longitude > 180) {
    return { error: 'geoRange' };
  }
  return { body: { latitude, longitude } };
}
