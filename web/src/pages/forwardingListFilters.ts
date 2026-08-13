// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Settings ▸ Forwarding table shows.
//
// Named `…ListFilters` and not `forwardingFilters` on purpose: `forwardingOptions.ts` next door is
// about a destination's **own** filter — the conditions deciding which events it relays — and two
// files a letter apart meaning different things by "filter" is a trap for whoever reads this next.
//
// Client-side: the destination list is bounded by what an operator configured, not by fleet size
// (ui-conventions). In a `.ts` so a test can reach it (testing.md).

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import { discoveredOptions, enumOptions } from '../lib/filterPresets';
import { ENABLED_STATES } from '../lib/filterQuery';
import {
  FORWARD_DEST_KINDS,
  FORWARD_SOURCE_KINDS,
  type ForwardDestination,
} from '../types/api';

/** The token standing for "no pool pinned". A sentinel is needed because the filter state is a
 *  string and `null` has to be selectable; `''` cannot be used — that is the *unfiltered* value. */
export const ALL_POOLS = '*';

/**
 * The Settings ▸ Forwarding filter row, keyed by `Column.key` (ADR-053 Inc.3).
 *
 * ⚠️ **This conversion changed the table, and that is the increment working rather than scope
 * creep.** The Target cell used to render two facts — where the relay sends, and *what protocol it
 * speaks* — stacked in one column. A filter row cannot express that: one column carries one filter,
 * so the choice was to keep the text search and lose the destination-kind dropdown, or to give the
 * kind the column it was already occupying. The second is what the model is for. `dest` is now its
 * own column, and Target is only the address.
 *
 * `pools` are discovered from the rows rather than declared, because a pool is whatever an operator
 * named — see `discoveredOptions` for the cap and why an unbounded column must not have a list.
 */
export function forwardingFilters(
  t: TFunction,
  rows: readonly ForwardDestination[],
): Record<string, ColumnFilterSpec<ForwardDestination>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (r) => [r.name],
      containsSemantics: 'substring',
      placeholder: t('cols.name'),
    },
    source: {
      kind: 'enum',
      options: enumOptions(FORWARD_SOURCE_KINDS, t, 'source.'),
      readValue: (r) => r.source_kind,
      allLabel: t('cols.source'),
      counts: 'client',
    },
    target: {
      kind: 'text',
      // Contains + regex over the address: "which of these points at 10.0.0.9" is the question an
      // operator actually has during an incident, and the name is whatever someone typed months ago.
      modes: ['contains', 'regex'],
      not: true,
      readText: (r) => [r.target],
      containsSemantics: 'substring',
      placeholder: t('cols.target'),
    },
    dest: {
      kind: 'enum',
      options: enumOptions(FORWARD_DEST_KINDS, t, 'dest.'),
      readValue: (r) => r.dest_kind,
      allLabel: t('cols.dest'),
      counts: 'client',
    },
    scope: {
      kind: 'enum',
      // `null` means every pool. It gets a real option rather than being unfilterable, because
      // "which of these is not pinned to a site" is a question with an answer.
      options: [
        { value: ALL_POOLS, label: t('scope.allPools') },
        ...discoveredOptions(rows, (r) => r.pool),
      ],
      readValue: (r) => r.pool ?? ALL_POOLS,
      allLabel: t('cols.scope'),
      counts: 'client',
    },
    fidelity: {
      kind: 'enum',
      options: [
        { value: 'verbatim', label: t('fidelity.verbatim') },
        { value: 'rendered', label: t('fidelity.rendered') },
      ],
      readValue: (r) => (r.verbatim ? 'verbatim' : 'rendered'),
      allLabel: t('cols.fidelity'),
      counts: 'client',
    },
    status: {
      kind: 'enum',
      options: enumOptions(ENABLED_STATES, t, 'common:filter.'),
      readValue: (r) => (r.enabled ? 'enabled' : 'disabled'),
      allLabel: t('common:filter.allEnabled'),
      counts: 'client',
    },
  };
}
