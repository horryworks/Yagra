// SPDX-License-Identifier: AGPL-3.0-only
// The bounds `PUT /meraki/orgs/{id}/cadence` accepts for each interval, as data.
//
// The cadence dialog used to carry them as four string literals inside the `.tsx` (`'60–3600'`),
// which no check can see — and one of them was a working copy: the inventory floor moved from 900
// to 60 on the server (migration 0124) and the hint was brought along by hand. Outside the bounds
// the server answers `400 invalid_cadence`, so a hint that disagrees sends the operator into it.
//
// ⚠️ The second copy of a fact. The first is `crates/yagra-core/src/config.rs` (`MERAKI_FAST_*`,
// `MERAKI_TRAFFIC_*`, `MERAKI_INVENTORY_*`, `MERAKI_SWITCH_PORTS_*`, mirrored by the CHECKs in
// migrations 0038, 0124 and 0130), and
// `api/meraki.rs::the_cadence_bounds_the_webui_shows_are_the_ones_this_api_accepts` reads THIS file
// to hold the two together. It finds each bound as `export const NAME = <digits>;` — keep them
// plain numbers on one line, or that check stops seeing them and says so.

/** Availability and uplink: the two tiers that share the fast band. */
export const CADENCE_FAST_MIN_SECS = 60;
export const CADENCE_FAST_MAX_SECS = 3600;
/** Every switch port's status, speed and traffic (ADR-167): nothing finer than the Dashboard's
 *  five-minute buckets exists, and much past ten minutes a port would be drawn as stale. */
export const CADENCE_SWITCH_PORTS_MIN_SECS = 300;
export const CADENCE_SWITCH_PORTS_MAX_SECS = 600;
/** Traffic. */
export const CADENCE_TRAFFIC_MIN_SECS = 300;
export const CADENCE_TRAFFIC_MAX_SECS = 86_400;
/** The inventory sync. */
export const CADENCE_INVENTORY_MIN_SECS = 60;
export const CADENCE_INVENTORY_MAX_SECS = 604_800;

/** The intervals the dialog edits, in the order it draws them. */
export const MERAKI_CADENCE_FIELDS = [
  'availability',
  'uplink',
  'switch_ports',
  'traffic',
  'inventory',
] as const;

export type MerakiCadenceField = (typeof MERAKI_CADENCE_FIELDS)[number];

export interface CadenceBounds {
  min: number;
  max: number;
}

/** Which band each interval is held to. A `Record`, so another interval cannot be drawn without
 *  saying what it accepts. */
export const MERAKI_CADENCE_BOUNDS: Record<MerakiCadenceField, CadenceBounds> = {
  availability: { min: CADENCE_FAST_MIN_SECS, max: CADENCE_FAST_MAX_SECS },
  uplink: { min: CADENCE_FAST_MIN_SECS, max: CADENCE_FAST_MAX_SECS },
  switch_ports: { min: CADENCE_SWITCH_PORTS_MIN_SECS, max: CADENCE_SWITCH_PORTS_MAX_SECS },
  traffic: { min: CADENCE_TRAFFIC_MIN_SECS, max: CADENCE_TRAFFIC_MAX_SECS },
  inventory: { min: CADENCE_INVENTORY_MIN_SECS, max: CADENCE_INVENTORY_MAX_SECS },
};

/** What an interval's hint line reads. Built from the bounds so the two cannot disagree. */
export const cadenceRange = (field: MerakiCadenceField): string => {
  const { min, max } = MERAKI_CADENCE_BOUNDS[field];
  return `${min}–${max}`;
};
