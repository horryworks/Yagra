// SPDX-License-Identifier: AGPL-3.0-only
// The integrations catalogue as data, one entry per external system Yagra can read.
//
// **This is ADR-037's homework, collected by ADR-100.** The catalogue page used to hard-code
// Meraki's `<Link>` and a "more coming" placeholder in JSX, and ADR-037 wrote down that it should
// become a registry "at the point a second integration lands". NetBox is that second one, so the
// choice was: edit the JSX twice, or make adding a tile one entry. `extensibility.md` §1 says a
// set with a per-member concern is a `Record` keyed by the union — a missing key is a compile
// error, an extra key is too.
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`), and everything below is judgement:
// which tile exists, what it links to, and how a probe's answer becomes a chip.

import { api } from '../../services/api';
import type { Chip } from './chip';
import { merakiChip, type MerakiStatus } from './merakiStatus';
import { netboxChip, type NetboxStatus } from './netboxStatus';

/** Every integration with a tile. `as const` so it is iterable at runtime — the property the i18n
 *  key coverage test needs, and the reason `extensibility.md` §4 asks for this shape. */
export const INTEGRATION_IDS = ['meraki', 'netbox'] as const;
export type IntegrationId = (typeof INTEGRATION_IDS)[number];

/** One catalogue tile. */
export interface IntegrationCard {
  id: IntegrationId;
  /** Where the tile links. Must match a route in `routeGroups/settings.tsx`. */
  path: string;
  /** The vendor's name. */
  nameKey: string;
  /** One line saying what integrating it does — ADR-055 R3, at the tile rather than on hover. */
  descKey: string;
  /** Read this integration's current state and reduce it to a chip.
   *
   *  Rejections are the caller's to classify: a load failure is the *page's* concern and is
   *  identical for every tile (`chip.ts::blockedChip`). What an entry owns is the vendor-specific
   *  half — which reads answer the question, and what their answers mean. */
  probe: () => Promise<Chip>;
}

/**
 * Every tile, keyed by the union.
 *
 * ⚠️ Adding an integration means an entry here, its two locale strings, and a route. It does
 * **not** mean editing the catalogue page.
 */
export const INTEGRATIONS: Record<IntegrationId, IntegrationCard> = {
  meraki: {
    id: 'meraki',
    path: '/settings/integrations/meraki',
    nameKey: 'meraki.name',
    descKey: 'integrations.meraki.desc',
    probe: async (): Promise<Chip> => {
      const [orgs, polling] = await Promise.all([api.listMerakiOrgs(), api.getMerakiPolling()]);
      const status: MerakiStatus =
        orgs.length === 0
          ? { kind: 'not-configured' }
          : { kind: 'connected', orgs: orgs.length, pollingOn: polling.enabled };
      return merakiChip(status);
    },
  },
  netbox: {
    id: 'netbox',
    path: '/settings/integrations/netbox',
    nameKey: 'netbox.name',
    descKey: 'integrations.netbox.desc',
    probe: async (): Promise<Chip> => {
      const servers = await api.listNetboxServers();
      const status: NetboxStatus =
        servers.length === 0
          ? { kind: 'not-configured' }
          : {
              kind: 'connected',
              servers: servers.length,
              // A server nobody has enabled is configured and idle, which is a different thing
              // from configured and failing — the chip has to be able to say both.
              anyEnabled: servers.some((s) => s.enabled),
              // ⚠️ `=== false`, never `!s.last_sync_ok`: the column is null until a sync has run,
              // so the loose spelling reports a freshly added server as failing before it has done
              // anything at all.
              lastSyncFailed: servers.some((s) => s.last_sync_ok === false),
            };
      return netboxChip(status);
    },
  },
};

/** The tiles in catalogue order. */
export function integrationCards(): IntegrationCard[] {
  return INTEGRATION_IDS.map((id) => INTEGRATIONS[id]);
}
