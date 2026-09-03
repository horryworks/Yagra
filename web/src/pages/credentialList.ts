// SPDX-License-Identifier: AGPL-3.0-only
/** The Credentials table's client-side view: which rows are shown, and in what order.
 *
 *  Client-side filtering is deliberate here — the credential list is bounded by what an operator
 *  typed in, not by fleet size (ui-conventions, "scale-aware lists"), so the whole list is in the
 *  browser and this is the only thing deciding what an operator sees. Which makes it worth testing:
 *  a search that quietly stops matching the id column, or a sort that ignores its direction, is
 *  invisible until someone is hunting for one credential among fifty.
 *
 *  Lives in a `.ts` for the usual reason — Vitest here runs `environment: 'node'` with
 *  `include: ['src/ **\/*.test.ts']`, so inside `CredentialsPage.tsx` this was untestable.
 *
 *  **ADR-053 Inc.5** moved the toolbar's search box and type dropdown into the filter row, and the
 *  row predicate with them (`lib/filterPredicate.ts`). Two things went with `visibleCredentials`:
 *
 *  - The **`'all'` sentinel**. The old type filter used the literal string `'all'` for "no filter",
 *    which meant `kindFilter === 'all'` was a magic value every caller had to know and one wrong
 *    comparison away from filtering for a credential of kind "all". A filter cell's unset value is
 *    `''`, and `buildPredicate` skips an empty column entirely, so there is nothing to compare.
 *  - The **name-or-id search**. It matched both fields in one box, and the filter row cannot say
 *    that honestly — each column filters itself. That is the better control (an operator can now
 *    ask about the id without also matching a *name* containing those characters), and it is why
 *    the Credential ID column gained a filter it never had.
 *
 *  The sort stayed, because the sort was never the problem: `sortRows` from `lib/tableSort.ts` now
 *  applies it, and this module supplies the per-column accessors. */

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import type { SortState, SortValues } from '../lib/tableSort';
import type { CredentialSummary } from '../types/api';

/** The fields this module reads. Generic over the row so the page passes real `CredentialSummary`
 *  values and gets them back unchanged, rather than a re-declared copy of the API shape. */
type CredentialRow = Pick<CredentialSummary, 'id' | 'name' | 'kind' | 'used_by'>;

/** The name column sorts ascending by default — the order an operator scanning a list expects. */
export const DEFAULT_CREDENTIAL_SORT: SortState = { by: 'name', dir: 'asc' };

/** Per-column sort accessors. `used_by` is a number, so it sorts numerically rather than as text —
 *  a string sort would put 10 before 9. */
export function credentialSortValues<T extends CredentialRow>(): SortValues<T> {
  return {
    name: (c) => c.name,
    used_by: (c) => c.used_by,
  };
}

/**
 * The Settings ▸ Credentials filter row, keyed by `Column.key`.
 *
 * `kinds` comes from the **rows**, not from `lib/credentialKinds.ts::CREDENTIAL_KINDS`, and the
 * difference is not cosmetic: that constant is the list an operator may *create*, and it
 * deliberately excludes `meraki_api` (created by the integration, shown read-only). The old
 * dropdown hardcoded three of the five, so `http_auth` and `meraki_api` credentials existed in the
 * table and could not be filtered for at all. Reading the kinds off the rows means every kind that
 * is on screen is selectable, which is the property that actually matters here.
 */
export function credentialFilters(
  t: TFunction,
  kinds: readonly string[],
  kindLabel: (kind: string) => string,
): Record<string, ColumnFilterSpec<CredentialSummary>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (c) => [c.name],
      containsSemantics: 'substring',
      placeholder: t('cred.cols.name'),
    },
    type: {
      kind: 'enum',
      options: kinds.map((k) => ({ value: k, label: kindLabel(k) })),
      readValue: (c) => c.kind,
      allLabel: t('cred.filter.allTypes'),
      counts: 'client',
    },
    // The id is the handle that appears in a node's config and in an error message, so pasting one
    // has to find its row. Under its own column now rather than folded into the name search.
    id: {
      kind: 'text',
      modes: ['contains'],
      readText: (c) => [c.id],
      containsSemantics: 'substring',
      placeholder: t('cred.cols.credentialId'),
    },
  };
}

/** A credential kind's display label, falling back to the raw token for a kind this build does not
 *  know. A newer core can store a kind this WebUI has never heard of; showing `snmp_v4` is honest,
 *  and showing nothing would make the row look broken. */
/** The i18n key per credential kind. ⚠️ The **label** half of what used to be one `KIND_META`
 *  map in the page; the icon half stays there, because an icon is a component and this file is
 *  loaded by a test in a node environment. Both halves are keyed by the same strings. */
export const CREDENTIAL_KIND_LABEL_KEYS: Record<string, string> = {
  snmp_v2c: 'cred.kind.snmp_v2c',
  snmp_v3: 'cred.kind.snmp_v3',
  http_auth: 'cred.kind.http_auth',
  api_token: 'cred.kind.api_token',
  meraki_api: 'cred.kind.meraki_api',
  netbox_token: 'cred.kind.netbox_token',
};

export const kindLabel = (kind: string, t: TFunction) => {
  const key = CREDENTIAL_KIND_LABEL_KEYS[kind];
  return key ? t(key) : kind;
};

/** How many nodes reference a credential — "unused" is its own phrase rather than "0 nodes",
 *  because unused is the state an operator is looking for when deciding what may be deleted. */
export const usageLabel = (n: number, t: TFunction) =>
  n === 0 ? t('cred.usage.unused') : t('cred.usage.count', { count: n });
