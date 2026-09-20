// SPDX-License-Identifier: AGPL-3.0-only
// What the node overview says when the node's state is **not a current reading** (ADR-164 決定 18).
//
// A Cisco Meraki device is never pinged: what the Dashboard API says about it is all Yagra knows.
// When the API stops answering the whole organization, one alert is raised about the organization
// and its devices keep the last state they had — they did not fail. So a node can sit at `ok` with
// no alert of its own while nothing at all is being collected for it, and this is the one place on
// the node's own page that says so.
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`): which name to show, whether there is a
// reason to give, and which sentence that makes are judgement, not layout.

import { MERAKI_SYNC_FAILURES, type MerakiSyncFailure, type NodeStatus } from '../../types/api';
import { merakiOrgPath } from '../../pages/integrations/merakiOrgRow';

/** What to render. `null` means the node's state is a current one and nothing is said. */
export interface CollectionFaultNotice {
  /** The organization, by name — or by id when the server could not name it yet. */
  org: string;
  /** The organization's settings page, which says which collect is failing and offers the fix. */
  orgPath: string;
  /** `system` key of why the last collect failed, or `null` when the server gave no reason — or
   *  one this bundle has never heard of, which must not reach `t()` and render as a raw key. */
  reasonKey: string | null;
  /** When the organization's alert was raised. The state shown is older than this. */
  sinceUnixMs: number;
}

/** Read `NodeStatus.collection_fault` into what the overview renders. */
export function collectionFaultNotice(
  status: Pick<NodeStatus, 'collection_fault'> | null | undefined,
): CollectionFaultNotice | null {
  const fault = status?.collection_fault;
  if (!fault) return null;
  const known = (MERAKI_SYNC_FAILURES as readonly string[]).includes(fault.reason ?? '');
  return {
    org: fault.meraki_org_name ?? fault.meraki_org,
    orgPath: merakiOrgPath(fault.meraki_org),
    reasonKey: known ? `meraki.sync.reason.${fault.reason as MerakiSyncFailure}` : null,
    sinceUnixMs: fault.since_unix_ms,
  };
}
