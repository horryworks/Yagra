// SPDX-License-Identifier: AGPL-3.0-only
// What the integrations catalogue says about the Meraki tile, and in what tone.
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`). The exhaustive `switch` below is the
// half worth testing: it has no `default`, so a new `MerakiStatus` variant is a compile error
// rather than a tile that renders `undefined` — and that property is only worth anything if the
// union and the switch stay in the same file, which is why the type moved here too.

import type { TFunction } from 'i18next';

/** What this browser currently knows about the Meraki integration.
 *
 *  `unavailable` and `forbidden` are separate on purpose: the first means the deployment could not
 *  answer, the second means this operator may not ask. Collapsing them would tell an operator to
 *  go and fix a server that is fine. */
export type MerakiStatus =
  | { kind: 'loading' }
  | { kind: 'unavailable' }
  | { kind: 'forbidden' }
  | { kind: 'not-configured' }
  | { kind: 'connected'; orgs: number; pollingOn: boolean };

/** The chip's text. Exhaustive over the union — no `default`, deliberately. */
export function merakiStatusLabel(s: MerakiStatus, t: TFunction): string {
  switch (s.kind) {
    case 'loading':
      return t('integrations.status.checking');
    case 'unavailable':
      return t('integrations.status.unavailable');
    case 'forbidden':
      return t('integrations.status.forbidden');
    case 'not-configured':
      return t('integrations.status.notConfigured');
    case 'connected':
      // Configured but paused is not the same as configured and running, and the org count is the
      // fact that makes "connected" mean something.
      if (!s.pollingOn) return t('integrations.status.pollingPaused');
      return t('integrations.status.connected', { count: s.orgs });
  }
}

/** The chip's tone class.
 *
 *  ⚠️ Neutral/derived classes — never the `--status-*` node-state palette. An integration that
 *  cannot be reached is not a node that is down, and colouring it that way puts a red thing on the
 *  screen that no alert corresponds to. */
export function merakiStatusTone(s: MerakiStatus): string {
  if (s.kind === 'connected') return s.pollingOn ? 'ok' : 'paused';
  if (s.kind === 'unavailable' || s.kind === 'forbidden') return 'muted';
  return 'idle';
}
