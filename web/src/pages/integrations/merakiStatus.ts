// SPDX-License-Identifier: AGPL-3.0-only
// What the integrations catalogue says about the Meraki tile, and in what tone.
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`). The exhaustive `switch` below is the
// half worth testing: it has no `default`, so a new `MerakiStatus` variant is a compile error
// rather than a tile that renders `undefined` — and that property is only worth anything if the
// union and the switch stay in the same file, which is why the type moved here too.

import type { TFunction } from 'i18next';
import { chipLabel, type Chip } from './chip';

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

/** The tile's chip: what it says and in what tone, in one place.
 *
 *  Exhaustive over the union — no `default`, deliberately.
 *
 *  ⚠️ Tones are neutral/derived classes, never the `--status-*` node-state palette. An integration
 *  that cannot be reached is not a node that is down, and colouring it that way puts a red thing on
 *  the screen that no alert corresponds to.
 *
 *  This is the **one** mapping. [`merakiStatusLabel`] and [`merakiStatusTone`] are projections of
 *  it, so the label and the colour cannot come to disagree about what a state means — which they
 *  could while each ran its own `switch`. */
export function merakiChip(s: MerakiStatus): Chip {
  switch (s.kind) {
    case 'loading':
      return { labelKey: 'integrations.status.checking', tone: 'idle' };
    case 'unavailable':
      return { labelKey: 'integrations.status.unavailable', tone: 'muted' };
    case 'forbidden':
      return { labelKey: 'integrations.status.forbidden', tone: 'muted' };
    case 'not-configured':
      return { labelKey: 'integrations.status.notConfigured', tone: 'idle' };
    case 'connected':
      // Configured but paused is not the same as configured and running, and the org count is the
      // fact that makes "connected" mean something.
      if (!s.pollingOn) return { labelKey: 'integrations.status.pollingPaused', tone: 'paused' };
      return { labelKey: 'integrations.status.connected', count: s.orgs, tone: 'ok' };
  }
}

/** The chip's text. */
export function merakiStatusLabel(s: MerakiStatus, t: TFunction): string {
  return chipLabel(merakiChip(s), t);
}

/** The chip's tone class. */
export function merakiStatusTone(s: MerakiStatus): string {
  return merakiChip(s).tone;
}
