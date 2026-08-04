// SPDX-License-Identifier: AGPL-3.0-only
// Validation for the Settings ▸ System settings ▸ Discovery walks card (ADR-038 / ADR-043).
//
// Lives beside `retentionSettings.ts` and for the same reason: Vitest only runs
// `src/**/*.test.ts`, so judgement written inside a .tsx is untested by construction (testing.md).
//
// The card started as one toggle for CDP/LLDP adjacency and is now three walks that differ only in
// which pair of API fields they read. That is the shape `extensibility.md` §1 names: a registry keyed
// so the compiler demands an entry per member, not three near-identical blocks of JSX. Adding a
// fourth walk is one row in `DISCOVERY_WALKS` plus its two locale strings — and the `Record` type
// makes forgetting either a compile error rather than a blank control.
//
// It also closes a real gap: the interface-address walk (ADR-043 Increment 1) has had settings on
// the API since it shipped and no UI at all, because the card only ever knew about adjacency.

import type { NeighborConfig } from '../types/api';

/** The cadence band the server enforces. Mirrored here so the form can refuse before a round trip;
 *  the server re-validates and is authoritative, and `GET /api/v1/settings/neighbors` reports the
 *  band it actually enforces so a UI can prefer that over this fallback. */
export const MIN_NEIGHBOR_INTERVAL_SECS = 300;
export const MAX_NEIGHBOR_INTERVAL_SECS = 86400;

/** The discovery walks this deployment can issue, in the order the card renders them.
 *
 *  `as const` because the UI iterates it at runtime to build `t()` keys (extensibility §4) —
 *  `i18nEnumKeys.test.ts` is what proves every member has strings in both locales. */
export const DISCOVERY_WALKS = ['neighbors', 'l3', 'arp', 'routing'] as const;
export type DiscoveryWalk = (typeof DISCOVERY_WALKS)[number];

/** Which pair of `NeighborConfig` fields each walk reads and writes.
 *
 *  The API's field names are asymmetric — the adjacency pair is the bare `enabled`/`interval_secs`
 *  because it predates the others, and renaming it would break every existing client for no gain.
 *  This map is the one place that asymmetry is written down. */
const FIELDS: Record<DiscoveryWalk, { enabled: keyof NeighborConfig; interval: keyof NeighborConfig }> =
  {
    neighbors: { enabled: 'enabled', interval: 'interval_secs' },
    l3: { enabled: 'l3_enabled', interval: 'l3_interval_secs' },
    arp: { enabled: 'arp_enabled', interval: 'arp_interval_secs' },
    routing: { enabled: 'routing_enabled', interval: 'routing_interval_secs' },
  };

/** One walk's editable state. The cadence is a string because that is what an input holds — a number
 *  would force a parse on every keystroke and make an in-progress "3" mean 3 seconds. */
export interface WalkForm {
  enabled: boolean;
  intervalSecs: string;
}

/** The whole card's state: every walk, always present. */
export type DiscoveryForm = Record<DiscoveryWalk, WalkForm>;

/** The body `PUT /api/v1/settings/neighbors` accepts. Every field is sent, so a save can never
 *  half-apply — the server treats an absent field as "leave it", which is right for an old client
 *  and wrong for this one. */
export interface DiscoverySettingsBody {
  enabled: boolean;
  interval_secs: number;
  l3_enabled: boolean;
  l3_interval_secs: number;
  arp_enabled: boolean;
  arp_interval_secs: number;
  routing_enabled: boolean;
  routing_interval_secs: number;
}

export type DiscoveryParse =
  | { ok: true; values: DiscoverySettingsBody }
  | { ok: false; walk: DiscoveryWalk; min: number; max: number };

/** What the server most recently reported, per walk. */
function savedWalk(cfg: NeighborConfig, walk: DiscoveryWalk): WalkForm {
  const f = FIELDS[walk];
  return {
    // A `null` from a server that predates a walk reads as off, which is the safe direction: the
    // control renders unchecked rather than claiming a walk is running that the server never heard of.
    enabled: (cfg[f.enabled] as boolean | null | undefined) ?? false,
    intervalSecs: String((cfg[f.interval] as number | null | undefined) ?? MIN_NEIGHBOR_INTERVAL_SECS),
  };
}

/** Build the card's state from a server response. */
export function discoveryFormFrom(cfg: NeighborConfig): DiscoveryForm {
  return {
    neighbors: savedWalk(cfg, 'neighbors'),
    l3: savedWalk(cfg, 'l3'),
    arp: savedWalk(cfg, 'arp'),
    routing: savedWalk(cfg, 'routing'),
  };
}

/** Validate every walk against the band the server reported (falling back to the compiled mirror).
 *
 *  A cadence is checked even when its walk is switched **off**: the value is still stored, and saving
 *  an out-of-range number with the toggle off would fail server-side with an error that looks
 *  unrelated to the control the operator was actually using.
 *
 *  Reports the **first** offending walk rather than a list, because the card shows one error line and
 *  a list would name walks whose controls the operator can see are fine.
 */
export function parseDiscoveryForm(
  form: DiscoveryForm,
  band?: { min?: number | null; max?: number | null },
): DiscoveryParse {
  const min = band?.min != null && band.min > 0 ? band.min : MIN_NEIGHBOR_INTERVAL_SECS;
  const max = band?.max != null && band.max > 0 ? band.max : MAX_NEIGHBOR_INTERVAL_SECS;
  const secs: Partial<Record<DiscoveryWalk, number>> = {};
  for (const walk of DISCOVERY_WALKS) {
    const n = Number(form[walk].intervalSecs.trim());
    if (!Number.isInteger(n) || n < min || n > max) return { ok: false, walk, min, max };
    secs[walk] = n;
  }
  return {
    ok: true,
    values: {
      enabled: form.neighbors.enabled,
      interval_secs: secs.neighbors as number,
      l3_enabled: form.l3.enabled,
      l3_interval_secs: secs.l3 as number,
      arp_enabled: form.arp.enabled,
      arp_interval_secs: secs.arp as number,
      routing_enabled: form.routing.enabled,
      routing_interval_secs: secs.routing as number,
    },
  };
}

/** Whether any walk differs from what the server last reported — drives the Save button. */
export function isDiscoveryDirty(form: DiscoveryForm, saved: NeighborConfig): boolean {
  const was = discoveryFormFrom(saved);
  return DISCOVERY_WALKS.some(
    (w) =>
      form[w].enabled !== was[w].enabled ||
      form[w].intervalSecs.trim() !== was[w].intervalSecs.trim(),
  );
}

/** The cadence rendered for a human: seconds are what the API speaks, but "3600" is not a duration
 *  anyone reads at a glance. Whole hours and whole minutes get their own form; anything else stays
 *  in seconds rather than being rounded into a lie. */
export function describeCadence(
  secs: number,
  t: (key: string, opts?: Record<string, unknown>) => string,
): string {
  if (!Number.isFinite(secs) || secs <= 0) return t('settings.neighbors.cadence.seconds', { n: 0 });
  if (secs % 3600 === 0) return t('settings.neighbors.cadence.hours', { n: secs / 3600 });
  if (secs % 60 === 0) return t('settings.neighbors.cadence.minutes', { n: secs / 60 });
  return t('settings.neighbors.cadence.seconds', { n: secs });
}
