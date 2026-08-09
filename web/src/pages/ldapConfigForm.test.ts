// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';

import {
  connectionUrl,
  defaultPortFor,
  emptyLdapForm,
  passwordIsEditable,
  toLdapForm,
  toLdapInput,
  validateLdapForm,
  type LdapFormState,
} from './ldapConfigForm';
import type { LdapConfigView } from '../types/api';

const stored = (over: Partial<LdapConfigView> = {}): LdapConfigView =>
  ({
    host: 'dc1.corp.example.com',
    port: 636,
    security: 'ldaps',
    ca_cert: null,
    bind_dn: 'CN=svc,DC=corp,DC=example,DC=com',
    has_bind_password: true,
    user_base_dn: 'DC=corp,DC=example,DC=com',
    user_filter: '(&(objectClass=user)(sAMAccountName={username}))',
    username_attribute: 'sAMAccountName',
    uid_attribute: 'objectGUID',
    member_of_attribute: 'memberOf',
    group_base_dn: null,
    group_filter: null,
    group_name_attribute: 'cn',
    role_map: { NetOps: 'admin' },
    default_role: null,
    enabled: true,
    updated_at: '2026-08-04T00:00:00Z',
    ...over,
  }) as LdapConfigView;

const filled = (over: Partial<LdapFormState> = {}): LdapFormState => ({
  ...emptyLdapForm(),
  host: 'dc1.corp.example.com',
  bindDn: 'CN=svc,DC=corp,DC=example,DC=com',
  bindPassword: 's3cret',
  userBaseDn: 'DC=corp,DC=example,DC=com',
  ...over,
});

const rows = (pairs: [string, 'viewer' | 'operator' | 'admin'][]) =>
  pairs.map(([group, role], key) => ({ key, group, role }));

describe('the bind password', () => {
  // The deliberate divergence from `aiConfigForm.ts`, where '' means "clear the credential". A
  // blank bind password here is an *unauthenticated* bind that a permissive directory answers
  // `success`, so it is not a state the form may produce.
  it('is required on a first save and cannot be blank', () => {
    expect(validateLdapForm(filled({ bindPassword: '' }), [], null)).toBe('bindPassword');
    expect(validateLdapForm(filled({ bindPassword: '   ' }), [], null)).toBe('bindPassword');
    expect(validateLdapForm(filled(), [], null)).toBeNull();
  });

  it('is not asked for again once one is stored', () => {
    const form = filled({ bindPassword: '' });
    expect(passwordIsEditable(stored(), form)).toBe(false);
    expect(validateLdapForm(form, [], stored())).toBeNull();
  });

  it('is required again the moment "replace" is ticked', () => {
    const form = filled({ bindPassword: '', replacePassword: true });
    expect(passwordIsEditable(stored(), form)).toBe(true);
    expect(validateLdapForm(form, [], stored())).toBe('bindPassword');
  });

  // The omission is what makes the server keep the stored credential — sending '' would be
  // *rejected*, not ignored, so this is the difference between "edit the base DN" working and
  // failing with a confusing 400.
  it('is omitted from the payload entirely when it is not being replaced', () => {
    const payload = toLdapInput(filled({ bindPassword: '' }), [], stored());
    expect('bind_password' in payload).toBe(false);
    const replacing = toLdapInput(filled({ replacePassword: true }), [], stored());
    expect(replacing.bind_password).toBe('s3cret');
  });
});

describe('the user filter', () => {
  // Without the placeholder the filter matches the whole subtree, and any code path that took the
  // first result would authenticate whoever sorts first.
  it('must contain the username placeholder', () => {
    expect(validateLdapForm(filled({ userFilter: '(&(objectClass=user))' }), [], null)).toBe(
      'userFilter',
    );
  });
});

describe('the group search', () => {
  it('needs both halves or neither', () => {
    expect(validateLdapForm(filled({ groupBaseDn: 'OU=Groups,DC=x' }), [], null)).toBe('groupPair');
    expect(validateLdapForm(filled({ groupFilter: '(member={user_dn})' }), [], null)).toBe(
      'groupPair',
    );
    expect(
      validateLdapForm(
        filled({ groupBaseDn: 'OU=Groups,DC=x', groupFilter: '(member={user_dn})' }),
        [],
        null,
      ),
    ).toBeNull();
  });

  it('refuses a filter that names nobody', () => {
    expect(
      validateLdapForm(
        filled({ groupBaseDn: 'OU=Groups,DC=x', groupFilter: '(objectClass=group)' }),
        [],
        null,
      ),
    ).toBe('groupFilter');
  });
});

describe('enabling the directory', () => {
  // Otherwise every login is denied with the same generic 401 a wrong password gets, while the
  // page looks correctly filled in.
  it('needs a mapping or a default role', () => {
    expect(validateLdapForm(filled({ enabled: true }), [], null)).toBe('noMapping');
    expect(validateLdapForm(filled({ enabled: true, defaultRole: 'viewer' }), [], null)).toBeNull();
    expect(
      validateLdapForm(filled({ enabled: true }), rows([['NetOps', 'admin']]), null),
    ).toBeNull();
  });

  it('does not need one while it is still a draft', () => {
    expect(validateLdapForm(filled({ enabled: false }), [], null)).toBeNull();
  });

  // A row the operator added but never named must not count as a mapping.
  it('does not count a blank row as a mapping', () => {
    expect(validateLdapForm(filled({ enabled: true }), rows([['   ', 'admin']]), null)).toBe(
      'noMapping',
    );
  });
});

describe('the connection URL', () => {
  // The point of showing it: choosing StartTLS is what makes the scheme `ldap://`, and there is no
  // combination that produces a plaintext connection.
  it('follows the security mode', () => {
    expect(connectionUrl(filled({ security: 'ldaps', port: '636' }))).toBe(
      'ldaps://dc1.corp.example.com:636',
    );
    expect(connectionUrl(filled({ security: 'starttls', port: '389' }))).toBe(
      'ldap://dc1.corp.example.com:389',
    );
  });

  it('brackets an IPv6 host', () => {
    expect(connectionUrl(filled({ host: '2001:db8::1', port: '636' }))).toBe(
      'ldaps://[2001:db8::1]:636',
    );
  });

  it('offers the conventional port for each mode', () => {
    expect(defaultPortFor('ldaps')).toBe(636);
    expect(defaultPortFor('starttls')).toBe(389);
  });
});

describe('the port', () => {
  it('must be a number in range', () => {
    for (const port of ['0', '65536', '', 'ldap', '63.6']) {
      expect(validateLdapForm(filled({ port }), [], null)).toBe('port');
    }
    expect(validateLdapForm(filled({ port: '3268' }), [], null)).toBeNull();
  });
});

describe('loading a saved configuration', () => {
  it('never carries a password into the form and keeps replace off', () => {
    const form = toLdapForm(stored());
    expect(form.bindPassword).toBe('');
    expect(form.replacePassword).toBe(false);
  });

  it('turns absent optional fields into empty strings, not "null"', () => {
    const form = toLdapForm(stored({ ca_cert: null, group_base_dn: null, default_role: null }));
    expect(form.caCert).toBe('');
    expect(form.groupBaseDn).toBe('');
    expect(form.defaultRole).toBe('');
  });

  it('sends absent optional fields back as null rather than empty strings', () => {
    const payload = toLdapInput(toLdapForm(stored()), [], stored());
    expect(payload.ca_cert).toBeNull();
    expect(payload.group_base_dn).toBeNull();
    expect(payload.default_role).toBeNull();
  });
});
