// SPDX-License-Identifier: AGPL-3.0-only
// What the node overview says when the node's state is **not a current reading**. Two kinds of node
// are never polled themselves, and each keeps what it was last told when the thing that tells it
// stops:
//
// - **A Cisco Meraki device** (ADR-164 決定 18): what the Dashboard API says about it is all Yagra
//   knows. When the API stops answering the whole organization, one alert is raised about the
//   organization and its devices keep the last state they had — they did not fail. So a node can
//   sit at `ok` with no alert of its own while nothing at all is being collected for it, and this is
//   the one place on the node's own page that says so.
// - **A wireless access point** (ADR-064 増分 G): its controller's AP walk is what reports it. When
//   no controller has reported it lately it reads `unknown` (or stays `unreachable` if it was down
//   when last heard of), nothing is raised about the AP itself, and this says since when. The
//   controller is named by the "Wireless controller" row above it, so this line names none.
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`): which name to show, whether there is a
// reason to give, and which sentence that makes are judgement, not layout.

import { MERAKI_SYNC_FAILURES, type MerakiSyncFailure, type NodeStatus } from '../../types/api';
import { merakiOrgPath } from '../../pages/integrations/merakiOrgRow';

/** What to render. `null` means the node's state is a current one and nothing is said. */
export type CollectionFaultNotice =
  | {
      kind: 'meraki_api';
      /** The organization, by name — or by id when the server could not name it yet. */
      org: string;
      /** The organization's settings page, which says which collect is failing and offers the
       *  fix. `null` only if the server sent no organization, which it does not for this cause. */
      orgPath: string | null;
      /** `system` key of why the last collect failed, or `null` when the server gave no reason —
       *  or one this bundle has never heard of, which must not reach `t()` and render as a raw key. */
      reasonKey: string | null;
      /** When the organization's alert was raised. The state shown is older than this. */
      sinceUnixMs: number;
    }
  | {
      kind: 'wireless_controller';
      /** When a controller last reported the access point. Nothing has been heard since. */
      sinceUnixMs: number;
    };

/** Read `NodeStatus.collection_fault` into what the overview renders. */
export function collectionFaultNotice(
  status: Pick<NodeStatus, 'collection_fault'> | null | undefined,
): CollectionFaultNotice | null {
  const fault = status?.collection_fault;
  if (!fault) return null;
  const cause = fault.cause;
  switch (cause) {
    case 'meraki_api': {
      const known = (MERAKI_SYNC_FAILURES as readonly string[]).includes(fault.reason ?? '');
      return {
        kind: 'meraki_api',
        org: fault.meraki_org_name ?? fault.meraki_org ?? '',
        orgPath: fault.meraki_org ? merakiOrgPath(fault.meraki_org) : null,
        reasonKey: known ? `meraki.sync.reason.${fault.reason as MerakiSyncFailure}` : null,
        sinceUnixMs: fault.since_unix_ms,
      };
    }
    case 'wireless_controller':
      return { kind: 'wireless_controller', sinceUnixMs: fault.since_unix_ms };
    default: {
      // A cause a newer server knows and this bundle does not: say nothing rather than a sentence
      // about the wrong thing. The assignment is what makes a new cause a compile error here.
      const unhandled: never = cause;
      void unhandled;
      return null;
    }
  }
}
