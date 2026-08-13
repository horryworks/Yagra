// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Alerts ▸ Dependency table shows (ADR-053 Inc.3).
//
// The toolbar this replaces had a search box and one three-way select — `all` / `upstream` /
// `suppressed` — and that select is the interesting part of the conversion: it was **two different
// columns' questions sharing one control**. "Has an upstream" is about the Upstream column and
// "is being suppressed" is about the Root cause column, so an operator could never ask both at
// once, and the control could not say which column it was narrowing. Split across the two columns
// they belong to, both are answerable together and each reads where it applies.
//
// In a `.ts` so a test can reach it (testing.md).

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import { stateLabel } from '../lib/format';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { DIFF_VERDICTS, type DiffRow } from './topologyDiff';
import type { TopologyNode } from '../types/api';

/** Tokens for "this reference is set" / "it is not". A reference column cannot offer the node ids
 *  themselves as options — that list is the fleet, which decision 6 forbids — but whether the
 *  reference exists is a closed set of exactly two. */
export const REF_SET = 'set';
export const REF_NONE = 'none';

const refOptions = (t: TFunction, prefix: string) => [
  { value: REF_SET, label: t(`${prefix}.set`) },
  { value: REF_NONE, label: t(`${prefix}.none`) },
];

/**
 * The Dependency filter row, keyed by `Column.key`.
 *
 * `diffs` is passed rather than read from a closure so the verdict column filters on exactly what
 * it renders. `comparing` mirrors the column list: the three comparison columns only exist when a
 * derived graph is being compared, and a spec for a column that is not rendered would be a filter
 * the operator can set from a URL and never see.
 */
export function dependencyFilters(
  t: TFunction,
  diffs: ReadonlyMap<string, DiffRow>,
  comparing: boolean,
): Record<string, ColumnFilterSpec<TopologyNode>> {
  const specs: Record<string, ColumnFilterSpec<TopologyNode>> = {
    node: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (r) => [r.name],
      containsSemantics: 'substring',
      placeholder: t('dependency.cols.node'),
    },
    upstream: {
      kind: 'enum',
      options: refOptions(t, 'dependency.filter.upstreamRef'),
      readValue: (r) => (r.parent_id ? REF_SET : REF_NONE),
      allLabel: t('dependency.cols.upstream'),
      counts: 'client',
    },
    status: {
      kind: 'enum',
      // Worst first, so the states an operator came here for are at the top of the list. Labels
      // come from `stateLabel`, the same helper the cell uses — a second mapping here would be a
      // place for the filter and the badge to disagree about what a state is called.
      options: SEVERITY_ORDER.map((s) => ({ value: s, label: stateLabel(s) })),
      readValue: (r) => r.state,
      allLabel: t('dependency.cols.status'),
      counts: 'client',
    },
    root: {
      kind: 'enum',
      options: refOptions(t, 'dependency.filter.rootRef'),
      readValue: (r) => (r.root_cause ? REF_SET : REF_NONE),
      allLabel: t('dependency.cols.root'),
      counts: 'client',
    },
  };
  if (comparing) {
    specs.verdict = {
      kind: 'enum',
      options: DIFF_VERDICTS.map((v) => ({ value: v, label: t(`dependency.verdict.${v}`) })),
      // A node with no diff row has no verdict, and the cell renders an em dash. `null` matches
      // nothing once a selection exists, which is the same thing the cell says.
      readValue: (r) => diffs.get(r.id)?.verdict ?? null,
      allLabel: t('dependency.cols2.verdict'),
      counts: 'client',
    };
  }
  return specs;
}
