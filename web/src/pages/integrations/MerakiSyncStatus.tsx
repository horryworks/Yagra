// SPDX-License-Identifier: AGPL-3.0-only
// How one Meraki organization's last inventory sync went, and the button that runs one now.
//
// Shared by the organization's row on the Meraki page and by the organization's own page
// (ADR-164 Inc.4/5). It was the row's own JSX until the second screen existed; two copies of "which
// sync sentence wins, and when do the counts mean anything" would have agreed on the day they were
// written and on no day after. The judgement itself is in `merakiOrgRow.ts`, where a test reaches
// it — this file is layout over it.

import { useTranslation } from 'react-i18next';
import type { MerakiOrg } from '../../types/api';
import { Button } from '../../components/ui/Button';
import { formatTimestamp } from '../../lib/format';
import { orgCollectFailures, orgHasInventory, orgSyncSummary } from './merakiOrgRow';
import type { MerakiSync } from './useMerakiSync';
import './MerakiSyncStatus.css';

/** The status line: never / last sync / failed (+ last good), then the device counts. */
export function MerakiSyncStatus({ org, error }: { org: MerakiOrg; error?: string | null }) {
  const { t } = useTranslation('system');
  const summary = orgSyncSummary(org);
  // In the interface's language, not the browser's: a bare `toLocaleString()` put
  // `9/20/2026, 12:00:00 AM` inside a Japanese sentence on an en-US browser.
  const when = (iso: string) => formatTimestamp(Date.parse(iso));

  return (
    <div className="meraki-org-sync">
      {summary.kind === 'never' && <span className="muted">{t('meraki.sync.never')}</span>}
      {summary.kind === 'ok' && <span>{t('meraki.sync.ok', { when: when(summary.at) })}</span>}
      {summary.kind === 'failed' && (
        <>
          <span className="meraki-org-sync-failed">
            {t('meraki.sync.failed', { reason: t(`meraki.sync.reason.${summary.reason}`) })}
          </span>
          {summary.lastGoodAt && (
            <span className="muted">
              {t('meraki.sync.lastGood', { when: when(summary.lastGoodAt) })}
            </span>
          )}
        </>
      )}
      {orgHasInventory(org) && (
        <>
          <span>
            {t('meraki.sync.devices', {
              monitored: org.devices.monitored,
              seen: org.devices.seen,
            })}
          </span>
          {org.devices.new > 0 && (
            <span className="meraki-org-sync-new">
              {t('meraki.sync.new', { count: org.devices.new })}
            </span>
          )}
          {/* Marked, never acted on: the node and its alerts stay as they are (ADR-156 決定 3). */}
          {org.devices.missing > 0 && (
            <span className="meraki-org-sync-failed">
              {t('meraki.sync.missing', { count: org.devices.missing })}
            </span>
          )}
          {/* Nodes in a network the organization does not watch receive nothing, and nothing else
              on any screen says so — they keep their last state (ADR-164 決定 15). Short here; the
              organization's page has the sentence and the button. */}
          {org.devices.monitored_unwatched > 0 && (
            <span className="meraki-org-sync-failed">
              {t('meraki.sync.uncollected', { count: org.devices.monitored_unwatched })}
            </span>
          )}
        </>
      )}
      {/* A collect is not the sync above: it is a poller asking how the devices are, and it is what
          a device's state depends on. While availability fails the nodes keep their last state
          (ADR-164 決定 18) — said here because no node says it on the tree. */}
      {orgCollectFailures(org).map((f) => (
        <span className="meraki-org-sync-failed" key={f.tier}>
          {t(f.stalesNodes ? 'meraki.sync.collectFailing' : 'meraki.sync.tierFailing', {
            tier: f.listing
              ? t('meraki.sync.tierWithListing', {
                  tier: t(`meraki.tier.${f.tier}`),
                  listing: t(`meraki.listing.${f.listing}`),
                })
              : t(`meraki.tier.${f.tier}`),
            reason: t(`meraki.sync.reason.${f.reason}`),
          })}
        </span>
      ))}
      {error && <span className="meraki-org-sync-failed">{error}</span>}
    </div>
  );
}

/** "Sync now". The caller decides whether it is drawn at all — `useCan('manage_config')` and
 *  `canSyncNow` — because a button that can only be refused is not drawn (ADR-056). */
export function MerakiSyncButton({ sync }: { sync: MerakiSync }) {
  const { t } = useTranslation('system');
  return (
    <Button variant="outline" onClick={sync.run} disabled={sync.busy}>
      {sync.busy ? t('meraki.sync.running') : t('meraki.sync.now')}
    </Button>
  );
}
