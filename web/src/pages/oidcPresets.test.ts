// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { OIDC_PROVIDER_KINDS } from '../types/api';
import {
  OIDC_PRESETS,
  effectiveIssuer,
  issuerFor,
  paramFromIssuer,
  presetOf,
  providerFormReady,
  roleMapToSend,
} from './oidcPresets';

describe('oidc product presets', () => {
  it('covers every product kind', () => {
    for (const k of OIDC_PROVIDER_KINDS) {
      expect(OIDC_PRESETS[k], k).toBeDefined();
    }
    expect(Object.keys(OIDC_PRESETS).sort()).toEqual([...OIDC_PROVIDER_KINDS].sort());
  });

  // The whole point of the Entra product form. `groups` is not a scope Entra will accept, and it
  // refuses the authorize request outright — the operator never reaches a sign-in page, so there is
  // nothing in Yagra's logs to diagnose from.
  it('never asks Entra for a non-standard scope', () => {
    expect(presetOf('entra').scopes).toBe('openid profile email');
    expect(presetOf('entra').scopes).not.toContain('groups');
  });

  // And the opposite: dropping `groups` for Okta would make the role map match nobody, so every
  // user would resolve through default_role — a silent downgrade, the failure ADR-010 calls the
  // only real harm among the Entra/Okta gaps.
  it('still asks Okta for the groups scope', () => {
    expect(presetOf('okta').scopes).toContain('groups');
  });

  // The single fact that decides whether the role-map control is rendered at all.
  it('knows Google cannot deliver groups in the ID token', () => {
    expect(presetOf('google').supportsGroups).toBe(false);
    for (const k of ['entra', 'okta', 'generic'] as const) {
      expect(presetOf(k).supportsGroups, k).toBe(true);
    }
  });

  it('leaves the free-text product exactly as the form always pre-filled it', () => {
    expect(presetOf('generic').scopes).toBe('openid profile email groups');
    expect(presetOf('generic').issuerParam).toBeNull();
    expect(presetOf('generic').fixedIssuer).toBeNull();
  });
});

describe('issuer round trip', () => {
  // A build/parse pair written with two different spellings only misbehaves past the simplest
  // case — the URL-monitor extraction bug shipped for exactly that reason. Pin them to each other.
  const cases: Array<[(typeof OIDC_PROVIDER_KINDS)[number], string]> = [
    ['entra', '72f988bf-86f1-41af-91ab-2d7cd011db47'],
    ['entra', 'contoso.onmicrosoft.com'],
    ['entra', 'organizations'],
    ['okta', 'acme.okta.com'],
    ['okta', 'login.acme.com'],
  ];

  it.each(cases)('%s round-trips %s', (kind, param) => {
    const issuer = issuerFor(kind, param);
    expect(issuer).not.toBeNull();
    expect(paramFromIssuer(kind, issuer as string)).toBe(param);
  });

  it('builds the issuer each product actually publishes', () => {
    expect(issuerFor('entra', 'tenant-id')).toBe(
      'https://login.microsoftonline.com/tenant-id/v2.0',
    );
    expect(issuerFor('okta', 'acme.okta.com')).toBe('https://acme.okta.com');
  });

  it('trims before building', () => {
    expect(issuerFor('okta', '  acme.okta.com ')).toBe('https://acme.okta.com');
    expect(paramFromIssuer('okta', ' https://acme.okta.com ')).toBe('acme.okta.com');
  });

  it('refuses a value that would build a nonsense issuer', () => {
    for (const bad of ['', '   ', 'https://acme.okta.com', 'acme.okta.com/oauth2', 'a b']) {
      expect(issuerFor('okta', bad), bad).toBeNull();
      expect(issuerFor('entra', bad), bad).toBeNull();
    }
  });

  it('has no parameterised issuer for the fixed and free-text products', () => {
    expect(issuerFor('google', 'anything')).toBeNull();
    expect(issuerFor('generic', 'anything')).toBeNull();
    expect(paramFromIssuer('google', 'https://accounts.google.com')).toBeNull();
    expect(paramFromIssuer('generic', 'https://idp.example.com')).toBeNull();
  });

  // Reading one product's issuer as another's must not half-succeed — an Okta domain is not a
  // tenant id, and answering with one would rewrite the issuer on the next save.
  it('does not read one product’s issuer as another’s', () => {
    expect(paramFromIssuer('entra', 'https://acme.okta.com')).toBeNull();
    expect(paramFromIssuer('okta', 'https://login.microsoftonline.com/t/v2.0')).toBeNull();
    expect(paramFromIssuer('okta', 'https://accounts.google.com')).toBe('accounts.google.com');
  });

  // A row written through the API, or an Okta custom authorization server. The form has to notice
  // and show the raw issuer instead of pretending it can represent it.
  it('reports an unrepresentable stored issuer rather than guessing', () => {
    expect(paramFromIssuer('entra', 'https://idp.example.com')).toBeNull();
    expect(paramFromIssuer('entra', 'https://login.microsoftonline.com/t/v1.0')).toBeNull();
    expect(paramFromIssuer('okta', 'https://acme.okta.com/oauth2/default')).toBeNull();
  });
});

describe('effectiveIssuer', () => {
  it('ignores both inputs for a fixed-issuer product', () => {
    expect(effectiveIssuer('google', 'x', 'https://elsewhere.example.com')).toBe(
      'https://accounts.google.com',
    );
  });

  it('builds from the product field when it builds', () => {
    expect(effectiveIssuer('entra', 'tid', '')).toBe(
      'https://login.microsoftonline.com/tid/v2.0',
    );
  });

  // Opening a provider whose issuer this form cannot represent and pressing save must not destroy
  // it. Falling back to the stored value is what makes reopening a hand-written row non-destructive.
  it('falls back to the stored issuer when the product field cannot build one', () => {
    expect(effectiveIssuer('entra', '', 'https://idp.example.com')).toBe(
      'https://idp.example.com',
    );
    expect(effectiveIssuer('generic', '', ' https://idp.example.com ')).toBe(
      'https://idp.example.com',
    );
  });
});

describe('providerFormReady', () => {
  const complete = {
    kind: 'okta' as const,
    name: 'Company SSO',
    issuer: 'https://acme.okta.com',
    clientId: 'client',
    redirectUri: 'https://yagra.example.com/auth/callback',
    secretReady: true,
    defaultRole: '',
  };

  it('accepts a complete form', () => {
    expect(providerFormReady(complete)).toBe(true);
  });

  it.each(['name', 'issuer', 'clientId', 'redirectUri'] as const)(
    'refuses a blank %s',
    (field) => {
      expect(providerFormReady({ ...complete, [field]: '   ' })).toBe(false);
    },
  );

  it('refuses until a secret is stored or typed', () => {
    expect(providerFormReady({ ...complete, secretReady: false })).toBe(false);
  });

  // A product with no groups and no default role denies every sign-in. Legal, so the server has no
  // reason to refuse it — which makes this the only place it can be caught.
  it('refuses Google Workspace with no default role, and accepts it with one', () => {
    const google = { ...complete, kind: 'google' as const };
    expect(providerFormReady(google)).toBe(false);
    expect(providerFormReady({ ...google, defaultRole: 'viewer' })).toBe(true);
  });

  it('does not require a default role from a product that can deliver groups', () => {
    for (const kind of ['entra', 'okta', 'generic'] as const) {
      expect(providerFormReady({ ...complete, kind, defaultRole: '' }), kind).toBe(true);
    }
  });
});

describe('roleMapToSend', () => {
  const map = { 'net-admins': 'admin' };

  it('stores no map for a product that cannot deliver groups', () => {
    expect(roleMapToSend('google', map)).toEqual({});
  });

  it('stores the map for every product that can', () => {
    for (const kind of ['entra', 'okta', 'generic'] as const) {
      expect(roleMapToSend(kind, map), kind).toEqual(map);
    }
  });
});
