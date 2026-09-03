// SPDX-License-Identifier: AGPL-3.0-only
// The integrations catalogue registry (ADR-100, collecting ADR-037's homework).
//
// What is worth testing here is not that a `Record` has keys — the compiler does that — but the
// things a `Record` cannot say: that every tile links somewhere a route serves, that no two tiles
// claim the same path, and that the placeholder tile really is gone rather than moved.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { INTEGRATION_IDS, INTEGRATIONS, integrationCards } from './registry';
import { CHIP_TONES, blockedChip, chipLabel, loadingChip } from './chip';
import { merakiChip } from './merakiStatus';
import { netboxChip, syncFailed, syncSummary } from './netboxStatus';
import type { TFunction } from 'i18next';

/** i18n stand-in: the key, plus its interpolations when there are any. */
const t = ((key: string, opts?: Record<string, unknown>) =>
  opts && Object.keys(opts).length
    ? `${key}(${JSON.stringify(opts)})`
    : key) as unknown as TFunction;

describe('the integrations registry', () => {
  it('gives every id an entry that agrees with its own key', () => {
    for (const id of INTEGRATION_IDS) {
      expect(INTEGRATIONS[id].id, `${id}'s entry names a different id`).toBe(id);
    }
    expect(integrationCards()).toHaveLength(INTEGRATION_IDS.length);
  });

  it('links every tile at a route the settings group actually serves', () => {
    // A tile that links nowhere renders as a card that does nothing when clicked — no compile
    // error, no runtime error, and nothing else in the tree compares these two lists.
    const routes = readFileSync('src/routeGroups/settings.tsx', 'utf8');
    for (const card of integrationCards()) {
      const relative = card.path.replace('/settings/', '');
      expect(routes, `${card.id} links to ${card.path}, which settings.tsx does not route`).toContain(
        `path="${relative}"`,
      );
    }
  });

  it('gives no two tiles the same destination', () => {
    const paths = integrationCards().map((c) => c.path);
    expect(new Set(paths).size).toBe(paths.length);
  });

  it('keeps the catalogue page free of any vendor of its own', () => {
    // The point of the registry: adding a third integration must not mean editing the page.
    // ⚠️ Read as text because there is nothing else that could notice the JSX growing a tile back.
    const page = readFileSync('src/pages/integrations/IntegrationsCatalogPage.tsx', 'utf8');
    for (const vendor of ['meraki', 'netbox']) {
      expect(page.toLowerCase(), `the catalogue page names ${vendor} directly again`).not.toContain(
        `/settings/integrations/${vendor}`,
      );
    }
    // The accept side — without it, an empty page would satisfy the assertion above.
    expect(page).toContain('integrationCards()');
  });
});

describe('chips', () => {
  it('only ever uses a neutral tone', () => {
    // Never the `--status-*` node-state palette: an integration that cannot be reached is not a
    // node that is down, and colouring it that way puts a red thing on screen that no alert
    // corresponds to.
    const chips = [
      loadingChip(),
      blockedChip('unavailable'),
      blockedChip('forbidden'),
      merakiChip({ kind: 'connected', orgs: 1, pollingOn: true }),
      merakiChip({ kind: 'connected', orgs: 1, pollingOn: false }),
      merakiChip({ kind: 'not-configured' }),
      netboxChip({ kind: 'not-configured' }),
      netboxChip({ kind: 'connected', servers: 1, anyEnabled: true, lastSyncFailed: false }),
      netboxChip({ kind: 'connected', servers: 1, anyEnabled: false, lastSyncFailed: false }),
      netboxChip({ kind: 'connected', servers: 1, anyEnabled: true, lastSyncFailed: true }),
    ];
    for (const c of chips) expect(CHIP_TONES).toContain(c.tone);
  });

  it('keeps "cannot reach it" and "you may not ask" apart', () => {
    // Collapsing them would tell an operator to go and fix a server that is fine.
    expect(blockedChip('unavailable').labelKey).not.toBe(blockedChip('forbidden').labelKey);
  });

  it('interpolates a count only when there is one', () => {
    expect(chipLabel({ labelKey: 'k', tone: 'ok' }, t)).toBe('k');
    expect(chipLabel({ labelKey: 'k', count: 3, tone: 'ok' }, t)).toBe('k({"count":3})');
  });
});

describe('netboxChip', () => {
  it('reports a failure ahead of a pause, and a pause ahead of the happy count', () => {
    // Ordered by what an operator most needs to see. The other order lets "3 servers" cover a
    // server that has not synced since Tuesday.
    expect(
      netboxChip({ kind: 'connected', servers: 3, anyEnabled: true, lastSyncFailed: true }).labelKey,
    ).toBe('netbox.status.syncFailed');
    expect(
      netboxChip({ kind: 'connected', servers: 3, anyEnabled: false, lastSyncFailed: false })
        .labelKey,
    ).toBe('integrations.status.pollingPaused');
    const ok = netboxChip({
      kind: 'connected',
      servers: 3,
      anyEnabled: true,
      lastSyncFailed: false,
    });
    expect(ok.labelKey).toBe('netbox.status.connected');
    expect(ok.count).toBe(3);
    expect(ok.tone).toBe('ok');
  });

  it('says nothing is configured when nothing is', () => {
    expect(netboxChip({ kind: 'not-configured' }).labelKey).toBe(
      'integrations.status.notConfigured',
    );
  });
});

describe('syncFailed', () => {
  it('treats "has not synced yet" as not a failure', () => {
    // 🚨 The bug the `=== false` spelling exists for: `!last_sync_ok` reports a freshly added
    // server as failing before it has done anything at all.
    expect(syncFailed(null)).toBe(false);
    expect(syncFailed(undefined)).toBe(false);
    expect(syncFailed(true)).toBe(false);
    expect(syncFailed(false)).toBe(true);
  });
});

describe('syncSummary', () => {
  it('never renders a success and an error at the same time', () => {
    // Three columns answer three questions; a screen that renders each independently produces
    // "succeeded" next to an error string.
    expect(
      syncSummary({ last_sync_at: '2026-09-03T00:00:00Z', last_sync_ok: false, last_sync_error: 'refused' }),
    ).toEqual({ kind: 'failed', error: 'refused' });
  });

  it('distinguishes never-synced from synced-with-nothing-missing', () => {
    expect(syncSummary({})).toEqual({ kind: 'never' });
    expect(
      syncSummary({ last_sync_at: '2026-09-03T00:00:00Z', last_sync_ok: true, missing_folders: 0 }),
    ).toEqual({ kind: 'ok', at: '2026-09-03T00:00:00Z', missing: 0 });
  });

  it('carries the missing count through, because nothing else surfaces it', () => {
    // A folder NetBox no longer lists is marked and never deleted (ADR-100 decision 5), so this
    // number is the only place the operator learns it happened.
    expect(
      syncSummary({ last_sync_at: '2026-09-03T00:00:00Z', last_sync_ok: true, missing_folders: 2 }),
    ).toEqual({ kind: 'ok', at: '2026-09-03T00:00:00Z', missing: 2 });
  });
});
