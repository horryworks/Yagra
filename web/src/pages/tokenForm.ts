// SPDX-License-Identifier: AGPL-3.0-only
// Judgement behind the "New token" dialog, kept out of the `.tsx` so it can be tested.
//
// Vitest runs with `environment: 'node'` and `include: ['src/**/*.test.ts']`, so a test written
// next to a component is a file nothing executes (testing.md). The rules below decide what a
// credential can do and how long it lives, which is not something to leave to the type checker and
// a careful reader.

import type { TokenSurface, UserKind, UserSummary } from '../types/api';

/** Expiry choices offered in the dialog. `never` is deliberate — see `EXPIRY_PRESETS`. */
export const EXPIRY_CHOICES = ['30d', '90d', '365d', 'never'] as const;

/** One expiry choice. */
export type ExpiryChoice = (typeof EXPIRY_CHOICES)[number];

/** Days per choice; `never` has none.
 *
 * No expiry stays available on purpose. The alternative — forcing every token to carry a date —
 * reads as safer and is not: a CI credential that dies on a day nobody wrote down gets replaced in
 * a hurry by whoever is on call, usually with a broader one. The owner binding is what bounds an
 * unattended token's life, and expiry is the extra belt for the human-owned case. */
const EXPIRY_DAYS: Record<ExpiryChoice, number | null> = {
  '30d': 30,
  '90d': 90,
  '365d': 365,
  never: null,
};

/** The default: bounded, but long enough to outlive a quarter's worth of forgetting. */
export const DEFAULT_EXPIRY: ExpiryChoice = '90d';

/**
 * Resolve an expiry choice to the RFC 3339 instant the API takes, or `undefined` for no expiry.
 *
 * `now` is a parameter rather than a `Date.now()` call so the mapping is testable at a fixed
 * instant.
 */
export function expiryFromChoice(choice: ExpiryChoice, now: Date): string | undefined {
  const days = EXPIRY_DAYS[choice];
  if (days === null) return undefined;
  return new Date(now.getTime() + days * 86_400_000).toISOString();
}

/**
 * Whether the dialog can be submitted: a non-blank name, and at least one surface.
 *
 * A token naming no surface would authenticate nowhere. The backend refuses it too
 * (`400 no_surface`) — this is the same rule stated where the operator can see it, not the only
 * place it is enforced.
 */
export function canSubmit(name: string, surfaces: readonly TokenSurface[]): boolean {
  return name.trim() !== '' && surfaces.length > 0;
}

/**
 * Whether the account a token will belong to is itself group-scoped.
 *
 * A token can never exceed its owner: one owned by a scoped account **inherits** that account's
 * scope at every request, so offering a scope picker there would let an admin pick a value the
 * server refuses (`400 owner_is_scoped`) — and, if it did not refuse, one that silently had no
 * effect. `ownerId` is the picker's value, empty meaning "me".
 */
export function ownerIsScoped(
  owners: readonly UserSummary[],
  ownerId: string,
  myUsername: string,
): boolean {
  const owner = ownerId
    ? owners.find((u) => u.id === ownerId)
    : owners.find((u) => u.username === myUsername);
  // An owner we cannot resolve is treated as unscoped: the picker is then offered and the server
  // decides. Guessing "scoped" would hide the control on a page that failed to load its accounts.
  return owner !== undefined && owner.scope !== 'All';
}

/** Toggle one surface in a selection, keeping `TOKEN_SURFACES` order stable for display. */
export function toggleSurface(
  selected: readonly TokenSurface[],
  surface: TokenSurface,
  all: readonly TokenSurface[],
): TokenSurface[] {
  const next = selected.includes(surface)
    ? selected.filter((s) => s !== surface)
    : [...selected, surface];
  return all.filter((s) => next.includes(s));
}

/**
 * The accounts a token may be issued to, in the order the picker lists them.
 *
 * Service accounts first, then the signed-in user. That ordering is the recommendation made
 * visible: an unattended credential should belong to a machine identity, so that it survives the
 * person who created it and so that disabling it stops everything it owns at once. Other people's
 * accounts are not offered — handing someone else a credential in their name, without them
 * present, is not something a picker should make easy.
 *
 * Disabled accounts are excluded: a token owned by one cannot authenticate, so offering it would
 * mint something dead on arrival.
 */
export function ownerChoices(users: readonly UserSummary[], selfUsername: string): UserSummary[] {
  const enabled = users.filter((u) => u.enabled);
  const services = enabled
    .filter((u) => (u.auth_source as UserKind) === 'service')
    .sort((a, b) => a.username.localeCompare(b.username));
  const self = enabled.filter((u) => u.username === selfUsername);
  return [...services, ...self];
}

/**
 * Whether a listed token is usable right now, and if not, why.
 *
 * The listing shows several independent reasons a token can be dead, and an operator staring at one
 * that "looks fine" needs the actual one. Checked in the order the server checks them, so the
 * answer here matches the 401 they got.
 */
/** Every state a listed token can be in, in the order the server checks them. An `as const` array
 *  rather than a bare union because the listing's state filter iterates it, and because
 *  `i18nEnumKeys.test.ts` can then demand the `stateHint.*` strings in both locales
 *  (`extensibility.md` §4). `active` trails so the dropdown reads "what is wrong with it" first. */
export const TOKEN_STATES = ['revoked', 'expired', 'no-owner', 'owner-disabled', 'active'] as const;

export type TokenState = (typeof TOKEN_STATES)[number];

export function tokenState(
  token: {
    revoked_at?: string | null;
    expires_at?: string | null;
    owner?: string | null;
    owner_active?: boolean;
  },
  now: Date,
): TokenState {
  if (token.revoked_at) return 'revoked';
  if (token.expires_at && new Date(token.expires_at) <= now) return 'expired';
  // An owner-less token is one whose account was deleted, or one the 0057 backfill could not match.
  // It cannot authenticate — the verify path INNER JOINs `users` — so it must not read as active.
  if (!token.owner) return 'no-owner';
  if (!token.owner_active) return 'owner-disabled';
  return 'active';
}

/** Days until a token expires, or `null` when it has no expiry or has already lapsed. */
export function daysUntilExpiry(expiresAt: string | null | undefined, now: Date): number | null {
  if (!expiresAt) return null;
  const ms = new Date(expiresAt).getTime() - now.getTime();
  return ms <= 0 ? null : Math.ceil(ms / 86_400_000);
}
