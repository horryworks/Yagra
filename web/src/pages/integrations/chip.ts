// SPDX-License-Identifier: AGPL-3.0-only
// The status chip an integration tile wears.
//
// Its own module rather than part of `registry.ts` because both directions need it: the registry
// imports each integration's `*Chip` reducer, and each reducer returns this type. Putting it in the
// registry would make that a cycle — which ESM tolerates for hoisted functions and then breaks on
// the first `const`.

import type { TFunction } from 'i18next';

/** The chip's colour family.
 *
 *  ⚠️ Deliberately **not** the `--status-*` node-state palette. An integration that cannot be
 *  reached is not a node that is down, and colouring it that way puts a red thing on the screen
 *  that no alert corresponds to. */
export type ChipTone = 'ok' | 'paused' | 'muted' | 'idle';

/** Every tone, so a test can assert a reducer never invents one. */
export const CHIP_TONES = ['ok', 'paused', 'muted', 'idle'] as const;

/** What a tile's status chip says.
 *
 *  A key plus a count rather than a rendered string, so `t()` stays in the component and these
 *  modules stay testable in Vitest's node environment (`testing.md`). */
export interface Chip {
  labelKey: string;
  /** Interpolated as `count` when present. */
  count?: number;
  tone: ChipTone;
}

/** The chip for a tile whose probe was refused or could not be answered.
 *
 *  One implementation for every tile, because the reason is about the deployment or the caller and
 *  never about the vendor. Keeping the two apart is what stops "you may not read this" from being
 *  rendered as "this integration is broken".
 *
 *  `unavailable` = the deployment could not answer. `forbidden` = this operator may not ask.
 *  Collapsing them would tell an operator to go and fix a server that is fine. */
export function blockedChip(block: 'unavailable' | 'forbidden'): Chip {
  return {
    labelKey:
      block === 'forbidden'
        ? 'integrations.status.forbidden'
        : 'integrations.status.unavailable',
    tone: 'muted',
  };
}

/** The chip shown while a probe is in flight. */
export function loadingChip(): Chip {
  return { labelKey: 'integrations.status.checking', tone: 'idle' };
}

/** Render a chip's text. The one place `t()` meets these keys. */
export function chipLabel(chip: Chip, t: TFunction): string {
  return chip.count === undefined ? t(chip.labelKey) : t(chip.labelKey, { count: chip.count });
}
