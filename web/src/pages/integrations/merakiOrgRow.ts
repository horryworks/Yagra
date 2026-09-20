// SPDX-License-Identifier: AGPL-3.0-only
// What one organization's row on the Meraki page says about its inventory sync (ADR-164).
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`), and these three functions are
// judgement, not layout: which of three sync columns wins, when a device count means anything, and
// when a button that would be refused is not drawn at all.
//
// Shaped after `netboxStatus.ts::syncSummary` on purpose — the two integrations answer "did the
// last sync work?" from the same three columns, and two vocabularies for one question is how a
// settings area starts reading as several products.

import { MERAKI_SYNC_FAILURES, type MerakiOrg, type MerakiSyncFailure } from '../../types/api';

/** How an organization's last sync should be summarised. */
export type OrgSyncSummary =
  | { kind: 'never' }
  | { kind: 'ok'; at: string }
  | {
      kind: 'failed';
      reason: MerakiSyncFailure;
      /** The last sync that *did* work, if there has been one. A failed sync does not move
       *  `last_sync_at`, so this is what the device counts on the row still describe. */
      lastGoodAt: string | null;
    };

type SyncColumns = Pick<MerakiOrg, 'last_sync_at' | 'last_sync_ok' | 'last_sync_error'>;

/** Reduce the row's three sync columns into one thing to render.
 *
 *  ⚠️ Failure is `last_sync_ok === false`, never `!last_sync_ok`: the column is null until a sync
 *  has run, and a freshly added organization would otherwise read as failing before it has done
 *  anything. Failure is checked first because it can coexist with a `last_sync_at` — the stamp of
 *  an older success — and "Last sync 09:00" over a sync that has been failing since noon is the
 *  reading this exists to prevent. */
export function orgSyncSummary(org: SyncColumns): OrgSyncSummary {
  if (org.last_sync_ok === false) {
    return {
      kind: 'failed',
      // The server only omits the reason for a row it could not read one from.
      reason: org.last_sync_error ?? 'internal',
      lastGoodAt: org.last_sync_at ?? null,
    };
  }
  if (!org.last_sync_at) return { kind: 'never' };
  return { kind: 'ok', at: org.last_sync_at };
}

/** Whether the row's device counts describe anything yet.
 *
 *  Before the first successful sync every count is zero, and "0 of 0 devices monitored" reads as a
 *  finding — an empty organization — rather than as "not looked at yet". After one, the counts
 *  stay meaningful through any number of failures, because a failed sync writes nothing. */
export function orgHasInventory(org: Pick<MerakiOrg, 'last_sync_at'>): boolean {
  return Boolean(org.last_sync_at);
}

/** The Meraki integration's own page. */
/** One collect tier the Dashboard API is not answering, as the row words it. */
export interface CollectFailureLine {
  tier: string;
  reason: MerakiSyncFailure;
  /** Whether this is the tier a device's up/down state rides on. While it fails, the
   *  organization's nodes keep the last state they had, and after three failures in a row one
   *  alert is raised about the organization. The other tiers failing costs readings only. */
  stalesNodes: boolean;
}

/** Which of an organization's collects are failing, the one that matters first (ADR-164 決定 18).
 *
 *  Separate from {@link orgSyncSummary} on purpose: that is the inventory *sync* — this server
 *  asking what the organization holds. A *collect* is a poller asking how the devices are, by
 *  another route, and either can fail while the other works. A reason this bundle has never
 *  heard of reads as `internal`, the way the server reads one — never as "not failing". */
export function orgCollectFailures(
  org: Pick<MerakiOrg, 'collect_failures'>,
): CollectFailureLine[] {
  const known = new Set<string>(MERAKI_SYNC_FAILURES);
  return [...(org.collect_failures ?? [])]
    .map((f) => ({
      tier: f.tier,
      reason: (known.has(f.reason) ? f.reason : 'internal') as MerakiSyncFailure,
      stalesNodes: f.tier === 'availability',
    }))
    .sort((a, b) => Number(b.stalesNodes) - Number(a.stalesNodes));
}

export const MERAKI_PAGE_PATH = '/settings/integrations/meraki';

/** One organization's page: its devices and how new ones are imported (ADR-164 Inc.4/5).
 *
 *  One spelling, because three places need it — the row's name, the row's "Devices" button, and
 *  the page's own way back — and a path typed three times is a link that breaks in one of them.
 *  The id is the organization's Yagra uuid (`MerakiOrg.id`), never Meraki's `org_id`: the uuid is
 *  what every `/meraki/orgs/{id}/…` endpoint the page calls is keyed by. */
export function merakiOrgPath(orgUuid: string): string {
  return `${MERAKI_PAGE_PATH}/${encodeURIComponent(orgUuid)}`;
}

/** Whether "Sync now" is drawn for this organization.
 *
 *  Both switches make the server refuse the request (409), and a button that can only be refused
 *  is a control the operator cannot use — which this UI does not draw (ADR-056). The permission
 *  half of that rule stays with the caller's `useCan('manage_config')`; this is the state half. */
export function canSyncNow(org: Pick<MerakiOrg, 'enabled'>, pollingOn: boolean): boolean {
  return pollingOn && org.enabled;
}
