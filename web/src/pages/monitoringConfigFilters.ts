// SPDX-License-Identifier: AGPL-3.0-only
// Filter rows for the two Nodes ▸ configuration screens whose rows expand — Metric sets and Device
// profiles (ADR-053 Inc.6 decision H).
//
// These were the last two of the twelve hand-rolled `ytable` screens, held back from Inc.5 not
// because they were hard to filter but because clicking a row grows an editor underneath it, and
// `DataTable` had no expansion. It has one now, so they arrive with virtualization and a filter row
// at the same time.
//
// Client-side, and legitimately: `ui-conventions` puts configuration lists bounded by what an
// operator typed in on the client side, and each screen loads its whole list in one request. In a
// `.ts` because Vitest never executes a `.tsx` (testing.md).

import {
  specColumns,
  TEXT_MODES,
  type ColumnFilterSpec,
  type FilterableColumn,
} from '../lib/columnFilter';
import type { CollectionTemplate, ProfileSummary } from '../types/api';
import type { TFunction } from 'i18next';

// ──────────────────────────────────────────────────────────────── metric sets

/**
 * Metric sets: name and description, asked separately.
 *
 * The one box this replaces read both at once, which is fine until two sets share a word — and the
 * description is where the "why" lives, so "every set that mentions QoS" is a question about
 * descriptions specifically.
 */
export function metricSetFilters(t: TFunction): Record<string, ColumnFilterSpec<CollectionTemplate>> {
  return {
    name: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (r) => [r.name],
      containsSemantics: 'substring',
      placeholder: t('sets.cols.name'),
    },
    description: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (r) => [r.description],
      containsSemantics: 'substring',
      placeholder: t('sets.cols.description'),
    },
  };
}

export function setColumns(t: TFunction): FilterableColumn<CollectionTemplate>[] {
  return specColumns(metricSetFilters(t));
}

export function setFilterLabels(t: TFunction): Record<string, string> {
  return { name: t('sets.cols.name'), description: t('sets.cols.description') };
}

// ─────────────────────────────────────────────────────────────── device profiles

/**
 * Device profiles: name, vendor, and the poll interval as an explicit inherited/overridden split.
 *
 * ⚠️ **The interval column is not a text filter, and not a number one either.** What the cell
 * renders is either a number of seconds or the word "default" — the profile inherits the system
 * value when `poll_interval_secs` is null — so the question an operator actually has is "which
 * profiles override the system interval", and that is a two-valued set. A numeric range over a
 * column where most rows have no number would answer a different question and hide the rest.
 */
export const PROFILE_INTERVAL = ['inherited', 'overridden'] as const;

export function profileFilters(t: TFunction): Record<string, ColumnFilterSpec<ProfileSummary>> {
  return {
    name: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (p) => [p.name],
      containsSemantics: 'substring',
      placeholder: t('profiles.cols.name'),
    },
    vendor: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (p) => [p.vendor],
      containsSemantics: 'substring',
      placeholder: t('profiles.cols.vendor'),
    },
    interval: {
      kind: 'enum',
      options: PROFILE_INTERVAL.map((v) => ({ value: v, label: t(`profiles.filter.interval.${v}`) })),
      readValue: (p) => (p.poll_interval_secs == null ? 'inherited' : 'overridden'),
      allLabel: t('profiles.filter.allIntervals'),
      counts: 'client',
    },
  };
}

/**
 * The category filter, which has **no column** — it is the group heading the table sorts rows
 * under.
 *
 * Kept separate so the page can mount it in a `FilterBar` beside the table rather than in the
 * filter row, where there is no header to put it below. That is the same shape the Events page uses
 * for its node picker, and the reason it matters here: the one search box this replaces *did* match
 * the category label, so dropping it silently would take a capability away.
 *
 * ⚠️ The options come from `PROFILE_CATEGORIES` plus the `__other` bucket the grouping falls back
 * to, because a profile carrying a token this build does not know still has to be selectable — the
 * table already shows it under "Other", and a filter that could not name that group would hide it.
 */
export function profileCategoryFilter(
  t: TFunction,
  categories: readonly { token: string; label: string }[],
): Record<string, ColumnFilterSpec<ProfileSummary>> {
  const known = new Set(categories.map((c) => c.token));
  return {
    category: {
      kind: 'enum',
      options: [
        ...categories.map((c) => ({ value: c.token, label: c.label })),
        { value: OTHER_CATEGORY, label: t('profiles.otherGroup') },
      ],
      readValue: (p) => (known.has(p.category) ? p.category : OTHER_CATEGORY),
      allLabel: t('profiles.filter.allCategories'),
      counts: 'client',
    },
  };
}

/** The bucket an unrecognised category token falls into — the same one the grouping uses. */
export const OTHER_CATEGORY = '__other';

export function profileCategoryColumns(
  t: TFunction,
  categories: readonly { token: string; label: string }[],
): FilterableColumn<ProfileSummary>[] {
  return specColumns(profileCategoryFilter(t, categories));
}

export function profileColumns(t: TFunction): FilterableColumn<ProfileSummary>[] {
  return specColumns(profileFilters(t));
}

export function profileFilterLabels(t: TFunction): Record<string, string> {
  return {
    name: t('profiles.cols.name'),
    vendor: t('profiles.cols.vendor'),
    interval: t('profiles.cols.pollInterval'),
  };
}
