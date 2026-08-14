// SPDX-License-Identifier: AGPL-3.0-only
// Which accounts Settings ▸ Users shows (ADR-053 Inc.6).
//
// The Users screen is the "identity list" (data-table standard v2, variant B) — a roomy card per
// account, with no header row — so there is nowhere to put a column filter row. It gets a
// `FilterBar` instead: the same controls, carrying their own names (decision E).
//
// **What the operator gains is not the layout.** The role control was a segmented radio group, so it
// could ask for exactly one role; "operators and admins" — the set that can actually change
// anything, and therefore the set an audit starts from — was unsayable. Status and account kind were
// not filterable at all, though both are rendered on every row.
//
// Client-side, and legitimately: `ui-conventions.md` puts configuration lists bounded by what an
// operator typed in (users, credentials, profiles) on the client side, and the whole list arrives in
// one `listUsers()`. In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import {
  specColumns,
  TEXT_MODES,
  type ColumnFilterSpec,
  type FilterableColumn,
} from '../lib/columnFilter';
import { ROLES, USER_KINDS, type UserSummary } from '../types/api';
import type { TFunction } from 'i18next';

/** The account statuses the control offers. Derived from the one boolean the row carries, so there
 *  is no second list to keep in step — and spelled as tokens rather than `true`/`false` so the URL
 *  reads as what it means. */
export const USER_STATUSES = ['active', 'disabled'] as const;
export type UserStatus = (typeof USER_STATUSES)[number];

/**
 * The screen's filter columns.
 *
 * Roles are listed most-privileged first, matching the identity list's own order — the segmented
 * control this replaces did the same, by reversing `ROLES`. Keeping the order here means the URL's
 * token order is that order too (`encodeSet` sorts by the option list), so two people who tick the
 * same boxes in different orders produce the same link.
 */
export function userFilters(t: TFunction): Record<string, ColumnFilterSpec<UserSummary>> {
  return {
    role: {
      kind: 'enum',
      options: [...ROLES].reverse().map((r) => ({ value: r, label: t(`role.${r}`) })),
      readValue: (u) => u.role,
      allLabel: t('users.filter.all'),
      counts: 'client',
    },
    status: {
      kind: 'enum',
      options: USER_STATUSES.map((s) => ({ value: s, label: t(`users.status.${s}`) })),
      readValue: (u) => (u.enabled ? 'active' : 'disabled'),
      allLabel: t('users.filter.allStatuses'),
      counts: 'client',
    },
    // Where the account comes from — local, an OIDC provider, LDAP, or a service token. Rendered as
    // a badge on every row since the LDAP work, and never filterable until now.
    kind: {
      kind: 'enum',
      options: USER_KINDS.map((k) => ({ value: k, label: t(`users.kind.${k}`) })),
      readValue: (u) => u.auth_source,
      allLabel: t('users.filter.allKinds'),
      counts: 'client',
    },
    q: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (u) => [u.username],
      containsSemantics: 'substring',
      placeholder: t('users.searchPlaceholder'),
    },
  };
}

export function userColumns(t: TFunction): FilterableColumn<UserSummary>[] {
  return specColumns(userFilters(t));
}

/** Plain-text names for the bar and the mobile sheet. */
export function userFilterLabels(t: TFunction): Record<string, string> {
  return {
    role: t('users.cols.role'),
    status: t('users.cols.status'),
    kind: t('users.cols.kind'),
    q: t('users.cols.username'),
  };
}
