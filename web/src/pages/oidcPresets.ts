// SPDX-License-Identifier: AGPL-3.0-only
//
// What each supported IdP product needs, and nothing else.
//
// This module is the ONLY place the product knowledge lives. The server stores `issuer`, `scopes`,
// `groups_claim` and a descriptive `kind`, and makes no decision from the product — so putting the
// URL templates or the scope strings in Rust as well would be the same set of string literals in
// two files, which is the drift trap this repo keeps paying for.
//
// Why it exists at all: the provider form used to ask for all eight fields as free text, which
// assumes the operator already knows what their IdP wants. They do not, and the two products the
// release notes name want *opposite* things — Entra rejects any non-standard scope outright
// (AADSTS70011, before the sign-in page is ever drawn), while `groups` is exactly how Okta delivers
// the claim. No single default is right for both, so the form asks which product this is.
//
// ⚠️ Every judgement here must stay in this `.ts`. Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so anything decided inside `AuthSettingsPage.tsx` is untested.

import { OIDC_PROVIDER_KINDS, type OidcProviderKind } from '../types/api';

/** The single product-specific field an issuer is built from, when there is one. Iterable because
 *  the form builds its label/placeholder/hint keys from it — see `i18nEnumKeys.test.ts`. */
export const OIDC_ISSUER_PARAMS = ['tenant', 'domain'] as const;

/** The single product-specific field an issuer is built from, when there is one. */
export type OidcIssuerParam = (typeof OIDC_ISSUER_PARAMS)[number];

export type OidcPreset = {
  /** The one field the operator supplies to build the issuer; `null` ⇒ fixed or free text. */
  readonly issuerParam: OidcIssuerParam | null;
  /** The issuer when the product only has one; `null` otherwise. */
  readonly fixedIssuer: string | null;
  /** Scopes to request. Applied on create and when the product is switched — never on reopen. */
  readonly scopes: string;
  /** The ID-token claim carrying group membership. */
  readonly groupsClaim: string;
  /**
   * Whether the product can put group membership in the ID token at all.
   *
   * `false` for Google Workspace, and that is the single most useful thing this table encodes:
   * Google does not emit groups in the ID token, so a group→role map configured against it can
   * never match — every user resolves through `default_role` or is denied. The form hides the map
   * and says so rather than offering a control that cannot work.
   */
  readonly supportsGroups: boolean;
};

/**
 * Keyed by the union so the compiler demands an entry for every product (extensibility §1).
 * Adding a product is this table plus its `idp.` / `idpHint.` strings in both locales.
 */
export const OIDC_PRESETS: Record<OidcProviderKind, OidcPreset> = {
  entra: {
    issuerParam: 'tenant',
    fixedIssuer: null,
    // No `groups`. Entra's v2.0 endpoint accepts the standard OIDC scopes and fully-qualified
    // permission URIs, and rejects anything else with AADSTS70011 — the group claim is turned on
    // in the app registration's token configuration instead, not asked for as a scope.
    scopes: 'openid profile email',
    groupsClaim: 'groups',
    supportsGroups: true,
  },
  okta: {
    issuerParam: 'domain',
    fixedIssuer: null,
    // The org authorization server serves `groups` directly. A custom authorization server needs a
    // claim configured on it instead, which is why this form builds the org issuer and points a
    // custom-server deployment at the free-text option.
    scopes: 'openid profile email groups',
    groupsClaim: 'groups',
    supportsGroups: true,
  },
  google: {
    issuerParam: null,
    fixedIssuer: 'https://accounts.google.com',
    scopes: 'openid profile email',
    groupsClaim: 'groups',
    supportsGroups: false,
  },
  generic: {
    issuerParam: null,
    fixedIssuer: null,
    // Unchanged from what the form has always pre-filled, so choosing the free-text option lands
    // exactly where an operator who used this screen before would expect.
    scopes: 'openid profile email groups',
    groupsClaim: 'groups',
    supportsGroups: true,
  },
};

export function presetOf(kind: OidcProviderKind): OidcPreset {
  return OIDC_PRESETS[kind];
}

/** A tenant id / Okta domain may not carry a scheme, a path, or whitespace — it is one segment. */
function isOneSegment(v: string): boolean {
  return v !== '' && !/[\s/\\?#]/.test(v);
}

/**
 * Build the issuer from the product's single field.
 *
 * `null` means "cannot be built" — a fixed-issuer or free-text product, or a value that would
 * produce a nonsense URL. The caller treats `null` as "not ready to save" rather than sending it.
 */
export function issuerFor(kind: OidcProviderKind, param: string): string | null {
  const v = param.trim();
  switch (kind) {
    case 'entra':
      return isOneSegment(v) ? `https://login.microsoftonline.com/${v}/v2.0` : null;
    case 'okta':
      return isOneSegment(v) ? `https://${v}` : null;
    case 'google':
    case 'generic':
      return null;
  }
}

/**
 * Recover the product field from a stored issuer, for reopening a saved provider.
 *
 * The inverse of {@link issuerFor} and tested as a round trip — a `join`/`split` pair written with
 * two different separators is exactly how the URL-monitor extraction bug survived (it only showed
 * up past the first element).
 *
 * `null` means the stored issuer is not one this product's form can represent: a hand-written row,
 * an Okta custom authorization server, an issuer edited through the API. The form then falls back
 * to showing the raw issuer rather than silently rewriting it.
 */
export function paramFromIssuer(kind: OidcProviderKind, issuer: string): string | null {
  const v = issuer.trim();
  switch (kind) {
    case 'entra': {
      const m = /^https:\/\/login\.microsoftonline\.com\/([^/]+)\/v2\.0$/.exec(v);
      return m && isOneSegment(m[1]) ? m[1] : null;
    }
    case 'okta': {
      const m = /^https:\/\/([^/]+)$/.exec(v);
      return m && isOneSegment(m[1]) ? m[1] : null;
    }
    case 'google':
    case 'generic':
      return null;
  }
}

/**
 * The issuer to send for a product, given whatever the form currently holds.
 *
 * One function so the modal never has to branch on the product itself: a fixed-issuer product
 * ignores both inputs, a parameterised one builds from `param`, and free text passes `raw`
 * through. A parameterised product whose `param` does not build also falls back to `raw`, which is
 * what keeps a hand-written issuer from being destroyed by opening the dialog and pressing save.
 */
export function effectiveIssuer(kind: OidcProviderKind, param: string, raw: string): string {
  const preset = presetOf(kind);
  if (preset.fixedIssuer !== null) return preset.fixedIssuer;
  return issuerFor(kind, param) ?? raw.trim();
}

/** Products in picker order; `generic` last because it is the escape hatch, not a suggestion. */
export const OIDC_PICKER_ORDER = OIDC_PROVIDER_KINDS;

/**
 * Whether the provider form holds enough to be saved.
 *
 * Lives here rather than in the modal because of the last clause: a product that cannot deliver
 * groups has no working role map, so a blank default role means **every** sign-in is denied. That
 * is a configuration nobody wants and the server has no reason to refuse (it is legal, just
 * useless), which makes the form the only place it can be caught.
 */
export function providerFormReady(f: {
  kind: OidcProviderKind;
  name: string;
  /** The issuer that would actually be sent — see {@link effectiveIssuer}. */
  issuer: string;
  clientId: string;
  redirectUri: string;
  /** Whether a client secret is stored or has been typed. */
  secretReady: boolean;
  /** `''` = "deny when no group maps". */
  defaultRole: string;
}): boolean {
  return (
    f.name.trim() !== '' &&
    f.issuer.trim() !== '' &&
    f.clientId.trim() !== '' &&
    f.redirectUri.trim() !== '' &&
    f.secretReady &&
    (presetOf(f.kind).supportsGroups || f.defaultRole !== '')
  );
}

/**
 * The role map to store for a product.
 *
 * Empty for a product that cannot deliver groups: keeping a map the IdP can never satisfy would
 * leave the operator a stored rule that silently does nothing, which is worse than not having it.
 */
export function roleMapToSend<T>(
  kind: OidcProviderKind,
  map: Record<string, T>,
): Record<string, T> {
  return presetOf(kind).supportsGroups ? map : {};
}
