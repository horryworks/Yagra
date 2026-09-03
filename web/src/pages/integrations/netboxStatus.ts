// SPDX-License-Identifier: AGPL-3.0-only
// What the integrations catalogue says about the NetBox tile, and in what tone.
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`). The exhaustive `switch` below is the
// half worth testing: it has no `default`, so a new `NetboxStatus` variant is a compile error
// rather than a tile that renders `undefined` — and that property is only worth anything if the
// union and the switch stay in the same file, which is why the type lives here too.
//
// Shaped after `merakiStatus.ts`, deliberately: two tiles that answer the same question in two
// vocabularies is how a catalogue starts reading as several products.

import type { Chip } from './chip';

/** What this browser currently knows about the NetBox integration.
 *
 *  `unavailable` and `forbidden` are not here — a load failure is the page's concern and is
 *  identical for every tile (`chip.ts::blockedChip`). What is here is what is specific to
 *  NetBox. */
export type NetboxStatus =
  | { kind: 'not-configured' }
  | {
      kind: 'connected';
      servers: number;
      /** At least one server is switched on. All-disabled is configured-and-idle, which is not
       *  the same as configured-and-working. */
      anyEnabled: boolean;
      /** At least one server's **last completed** sync failed.
       *
       *  ⚠️ Read from `last_sync_ok === false`, never `!last_sync_ok`: the column is null until a
       *  sync has run, and a freshly added server would otherwise be reported as failing before it
       *  has done anything at all. */
      lastSyncFailed: boolean;
    };

/** The tile's chip. Exhaustive over the union — no `default`, deliberately.
 *
 *  The three connected outcomes are ordered by what an operator most needs to see: a failure
 *  outranks being paused, and being paused outranks the happy count. Ordering them the other way
 *  would let "3 servers" cover a server that has not synced since Tuesday. */
export function netboxChip(s: NetboxStatus): Chip {
  switch (s.kind) {
    case 'not-configured':
      return { labelKey: 'integrations.status.notConfigured', tone: 'idle' };
    case 'connected':
      if (s.lastSyncFailed) return { labelKey: 'netbox.status.syncFailed', tone: 'muted' };
      if (!s.anyEnabled) return { labelKey: 'integrations.status.pollingPaused', tone: 'paused' };
      return { labelKey: 'netbox.status.connected', count: s.servers, tone: 'ok' };
  }
}

/** Whether a stored sync outcome should read as a failure on the detail screen.
 *
 *  The same `=== false` rule as above, given a name so the two surfaces cannot disagree about what
 *  "has not synced yet" looks like. */
export function syncFailed(lastSyncOk: boolean | null | undefined): boolean {
  return lastSyncOk === false;
}

/** How a server's last sync should be summarised on the detail screen. */
export type SyncSummary =
  | { kind: 'never' }
  | { kind: 'ok'; at: string; missing: number }
  | { kind: 'failed'; error: string | null };

/** Reduce a server row's three sync columns into one thing to render.
 *
 *  They are three columns because they answer three questions, but a screen that renders all three
 *  independently produces "succeeded" next to an error string. */
export function syncSummary(row: {
  last_sync_at?: string | null;
  last_sync_ok?: boolean | null;
  last_sync_error?: string | null;
  missing_folders?: number;
}): SyncSummary {
  if (syncFailed(row.last_sync_ok)) {
    return { kind: 'failed', error: row.last_sync_error ?? null };
  }
  if (!row.last_sync_at) return { kind: 'never' };
  return { kind: 'ok', at: row.last_sync_at, missing: row.missing_folders ?? 0 };
}
