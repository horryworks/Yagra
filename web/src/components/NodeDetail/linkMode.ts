// SPDX-License-Identifier: AGPL-3.0-only
// The physical link's mode, as the Interfaces tab presents it (ADR-063 Inc.1).
//
// Pure, and in a `.ts` on purpose: Vitest runs in `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a rule written inside `InterfacesTab.tsx` is a rule no test
// can reach. The same split `interfaceMetrics.ts` and `tabs.ts` use.

import type { TFunction } from 'i18next';
import type { InterfaceRow } from '../../types/api';

/** Speed buckets the filter offers, coarsest question first: "which ports are not at their rate?"
 *
 *  ⚠️ A **fixed** list, not one derived from the rows on screen. Deriving filter options from the
 *  data is a trap this repository has already paid for once — filter columns rebuilt from displayed
 *  rows re-triggered the fetch that produced them. The cost of a fixed list is that most buckets
 *  read `0` on any one device; the cost of a derived one is a loop.
 *
 *  `other` catches a rate that is real but not a standard Ethernet tier — a 64 kbps dialer, a
 *  shaped WAN circuit, a rate we have no bucket for. `unknown` is a null/zero speed, which after
 *  ADR-063 decision 7 also covers a saturated `ifSpeed` the device could not express. */
/*  ⚠️ `2_5g`, not `2.5g`. These ids are interpolated into a `t()` key, and i18next reads `.` as its
 *  nesting separator — `interfaces.speed.2.5g` would look up `speed → 2 → 5g` and render the raw
 *  key inside a filter dropdown. Found by `i18nEnumKeys.test.ts` rather than in the browser, which
 *  is the whole reason that test iterates these arrays. The label says "2.5 Gbps"; only the id is
 *  constrained. */
export const SPEED_TIERS = [
  '10m',
  '100m',
  '1g',
  '2_5g',
  '5g',
  '10g',
  '25g',
  '40g',
  '100g',
  'other',
  'unknown',
] as const;
export type SpeedTier = (typeof SPEED_TIERS)[number];

/** Exact bits/sec for each standard tier. Exact, not a range: an interface reports its nominal
 *  rate, so `1 Gbps` is `1000000000` and anything near-but-not-equal is genuinely something else
 *  (a shaped circuit) and belongs in `other`. */
const TIER_BPS: ReadonlyArray<readonly [SpeedTier, number]> = [
  ['10m', 10_000_000],
  ['100m', 100_000_000],
  ['1g', 1_000_000_000],
  ['2_5g', 2_500_000_000],
  ['5g', 5_000_000_000],
  ['10g', 10_000_000_000],
  ['25g', 25_000_000_000],
  ['40g', 40_000_000_000],
  ['100g', 100_000_000_000],
];

/** An interface's speed bucket. Shared by the filter and by anything that counts them, so the
 *  dropdown and the rows can never disagree about what "1 Gbps" means — the same discipline
 *  `ifState()` applies to `oper_status`. */
export function speedTier(bps: number | null | undefined): SpeedTier {
  // A zero speed is "never advertised", not "0 bps" — the same reading the API's utilization
  // calculation takes, and the reason it returns null rather than dividing.
  if (bps == null || bps <= 0) return 'unknown';
  return TIER_BPS.find(([, v]) => v === bps)?.[0] ?? 'other';
}

/** Duplex buckets the filter offers.
 *
 *  Three, where the backend enum has two: `unknown` is the bucket for a null, exactly as
 *  `IF_STATES` carries one for an `oper_status` the poller has never had an answer for. Keeping it
 *  a UI bucket rather than a backend variant is what lets the wire enum stay closed — see the note
 *  on `Duplex` in `yagra-common/src/link_mode.rs`. */
export const DUPLEX_STATES = ['full', 'half', 'unknown'] as const;
export type DuplexState = (typeof DUPLEX_STATES)[number];

/** An interface's duplex bucket. Anything the backend did not send — or sent as a token this build
 *  does not know — reads as `unknown` rather than being dropped. */
export function duplexState(duplex: string | null | undefined): DuplexState {
  return duplex === 'full' || duplex === 'half' ? duplex : 'unknown';
}

/** `ethernetCsmacd` — the only IANAifType for which duplex is a meaningful question. Mirrors
 *  `yagra_common::IF_TYPE_ETHERNET_CSMACD`, which is the only code the backend names either. */
export const IF_TYPE_ETHERNET_CSMACD = 6;

/** Whether a duplex cell means anything for this interface.
 *
 *  Lets the row distinguish **"this is `NULL0`, the question does not apply"** from **"this is
 *  `GE0/0/2` and we could not read it"**. Both render an em dash — the difference is the tooltip —
 *  but on the one device measured while building this, 4 of 16 interfaces are virtual, so the
 *  distinction covers a quarter of the list rather than a corner case.
 *
 *  A missing `ifType` answers `true`: an interface we know nothing about is one to say "could not
 *  read" about, not one to quietly excuse. That direction matters — the opposite default would hide
 *  a device that answers no MIB at all behind "not applicable" on every row.
 *
 *  ⚠️ This lives only here. The backend deliberately ships the raw integer and computes no boolean,
 *  so there is one implementation of this rule rather than two that can disagree. */
export function duplexApplies(ifType: number | null | undefined): boolean {
  return ifType == null || ifType === IF_TYPE_ETHERNET_CSMACD;
}

/** Why a duplex cell is empty, for the cell's `title`. `null` when there is a value to show.
 *
 *  ⚠️ An optical port reports `unknown`, not `notApplicable` — and that is right: an SFP port *is*
 *  `ethernetCsmacd`, so the question applies, we simply have no answer. The reason a 10G port
 *  usually has none is that IEEE 802.3 defines no half duplex above 1 Gbit/s, leaving nothing to
 *  negotiate. That belongs in the column's filter `hint`, said once, rather than in a per-row
 *  tooltip — a row claiming "optical" would be guessing, since the medium is itself often unknown. */
export function duplexEmptyReason(
  duplex: string | null | undefined,
  ifType: number | null | undefined,
): 'notApplicable' | 'unknown' | null {
  if (duplexState(duplex) !== 'unknown') return null;
  return duplexApplies(ifType) ? 'unknown' : 'notApplicable';
}

/** Whether a media cell means anything for this interface.
 *
 *  The same rule as [`duplexApplies`] and deliberately a separate function rather than an alias:
 *  they agree today because both questions are about Ethernet ports, and if either ever needs to
 *  change (a tunnel has no medium but might one day report a duplex, say) the other should not move
 *  with it silently. */
export function mediaApplies(ifType: number | null | undefined): boolean {
  return ifType == null || ifType === IF_TYPE_ETHERNET_CSMACD;
}

/** An interface's media designation for filtering, bucketed to `unknown` when absent.
 *
 *  ⚠️ Unlike speed and duplex there is **no closed set** — `dot3MauType` is an IANA registry of
 *  250-and-growing designations, so the filter's options cannot be a fixed list and its values are
 *  the designations themselves. That is why the media column filters as free **text** while its two
 *  neighbours are enums. */
export function mediaText(media: string | null | undefined): string | null {
  const trimmed = media?.trim();
  return trimmed ? trimmed : null;
}

/** Tooltip for an empty duplex cell — why there is nothing there (ADR-063).
 *
 *  `undefined` when the cell has a value, so a populated cell carries no `title` at all rather
 *  than a redundant one. */
export function duplexTitle(r: InterfaceRow, t: TFunction): string | undefined {
  const reason = duplexEmptyReason(r.if_duplex, r.if_type);
  return reason ? t(`interfaces.duplexEmpty.${reason}`) : undefined;
}

/** Tooltip for the media cell: the transceiver's part string when there is one, otherwise why the
 *  cell is empty.
 *
 *  The two never collide — a port with a resolved medium AND a known module shows the module,
 *  which is the extra fact. The empty case splits in two on purpose: "this port type has no
 *  medium to report" and "it should have one and we have not read it" send an operator to
 *  different places. */
export function mediaTitle(r: InterfaceRow, t: TFunction): string | undefined {
  if (r.transceiver_model) {
    return t('interfaces.transceiver', { model: r.transceiver_model });
  }
  if (r.if_media) return undefined;
  return mediaApplies(r.if_type)
    ? t('interfaces.mediaEmpty.unknown')
    : t('interfaces.mediaEmpty.notApplicable');
}
