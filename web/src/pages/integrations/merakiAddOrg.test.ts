// SPDX-License-Identifier: AGPL-3.0-only
// The "Add organization" dialog's decisions (ADR-164 Inc.6).
//
// The one that matters most is the first: a request names its key by exactly one field, and the
// server refuses both and refuses neither. Nothing else can see that — `tsc` accepts a body with
// both (both are optional in the schema), and the dialog lives in a `.tsx` Vitest never loads.

import { describe, expect, it } from 'vitest';
import type { CredentialSummary, MerakiOrgOption } from '../../types/api';
import {
  allAlreadyAdded,
  keyFields,
  keyNameFor,
  regionForSavedKey,
  savedMerakiKeys,
  selectableOrgIds,
  unlistedRegion,
} from './merakiAddOrg';
import { DEFAULT_MERAKI_BASE_URL, MERAKI_REGIONS } from './merakiRegions';

const cred = (id: string, name: string, kind: string): CredentialSummary => ({
  id,
  name,
  kind,
  used_by: 0,
  used_by_meraki_orgs: 0,
  used_by_netbox_servers: 0,
});

describe('keyFields — the one field a request names its key by', () => {
  it('sends the typed key, trimmed, and nothing else', () => {
    expect(keyFields('typed', '  abc123\n', '')).toEqual({ api_key: 'abc123' });
  });

  it('sends the saved key’s id, and nothing else', () => {
    expect(keyFields('saved', '', 'cred-1')).toEqual({ credential_id: 'cred-1' });
  });

  it('never carries both, whatever is left in the other piece of state', () => {
    // The box keeps what was typed when the operator switches to "Use a saved key", and the picker
    // keeps its choice when they switch back. The server answers a body naming both with 400.
    const saved = keyFields('saved', 'typed-earlier', 'cred-1');
    expect(saved).toEqual({ credential_id: 'cred-1' });
    expect(saved).not.toHaveProperty('api_key');

    const typed = keyFields('typed', 'abc123', 'cred-1');
    expect(typed).toEqual({ api_key: 'abc123' });
    expect(typed).not.toHaveProperty('credential_id');
  });

  it('is null for a blank typed key, so the button stays off', () => {
    expect(keyFields('typed', '', '')).toBeNull();
    expect(keyFields('typed', '   \n', '')).toBeNull();
    // A saved key picked earlier does not stand in for a box that is empty now.
    expect(keyFields('typed', '', 'cred-1')).toBeNull();
  });

  it('is null for "saved" with nothing chosen', () => {
    expect(keyFields('saved', '', '')).toBeNull();
    // …and a typed key does not stand in for it either.
    expect(keyFields('saved', 'abc123', '')).toBeNull();
  });
});

describe('savedMerakiKeys — what the picker offers', () => {
  const creds = [
    cred('3', 'Zeta', 'meraki_api'),
    cred('1', 'core-ro', 'snmp_v2c'),
    cred('4', 'alpha', 'meraki_api'),
    cred('2', 'netbox', 'netbox_token'),
    cred('5', 'Beta', 'meraki_api'),
  ];

  it('offers Meraki keys only', () => {
    // Any other kind's id would be answered with `400 invalid_credential`.
    expect(savedMerakiKeys(creds).map((c) => c.kind)).toEqual([
      'meraki_api',
      'meraki_api',
      'meraki_api',
    ]);
  });

  it('sorts by name without regard to case', () => {
    // A code-point sort puts every capital before every lower-case letter — `Beta`, `Zeta`,
    // `alpha` — which reads as unsorted to someone looking for a name.
    expect(savedMerakiKeys(creds).map((c) => c.name)).toEqual(['alpha', 'Beta', 'Zeta']);
  });

  it('leaves the caller’s list as it was', () => {
    const before = creds.map((c) => c.id);
    savedMerakiKeys(creds);
    expect(creds.map((c) => c.id)).toEqual(before);
  });

  it('offers nothing when no Meraki key is stored — the dialog then has no choice to draw', () => {
    expect(savedMerakiKeys([cred('1', 'core-ro', 'snmp_v2c')])).toEqual([]);
    expect(savedMerakiKeys([])).toEqual([]);
  });
});

describe('regionForSavedKey — the region follows the key', () => {
  const orgs = [
    { credential_id: 'cred-global', base_url: 'https://api.meraki.com' },
    { credential_id: 'cred-china', base_url: 'https://api.meraki.cn' },
    { credential_id: 'cred-china', base_url: 'https://api.meraki.cn' },
  ];

  it('is the region an organization already using the key is polled in', () => {
    expect(regionForSavedKey('cred-china', orgs, DEFAULT_MERAKI_BASE_URL)).toBe(
      'https://api.meraki.cn',
    );
    expect(regionForSavedKey('cred-global', orgs, 'https://api.meraki.ca')).toBe(
      'https://api.meraki.com',
    );
  });

  it('falls back for a key no organization uses yet', () => {
    expect(regionForSavedKey('cred-unused', orgs, 'https://api.meraki.ca')).toBe(
      'https://api.meraki.ca',
    );
    expect(regionForSavedKey('cred-china', [], DEFAULT_MERAKI_BASE_URL)).toBe(
      DEFAULT_MERAKI_BASE_URL,
    );
  });
});

describe('unlistedRegion — what is shown is what is sent', () => {
  it('is null for every region the dialog offers', () => {
    for (const r of MERAKI_REGIONS) expect(unlistedRegion(r.base_url)).toBeNull();
  });

  it('names a base URL the select has no option for', () => {
    // An organization added through the API; the trailing slash alone makes it a different value.
    expect(unlistedRegion('https://api.meraki.cn/')).toBe('https://api.meraki.cn/');
  });
});

describe('selectableOrgIds — which discovered organizations can still be added', () => {
  const option = (id: string, already_added: boolean): MerakiOrgOption => ({
    id,
    name: `org-${id}`,
    already_added,
  });

  it('leaves out the ones that are monitored already', () => {
    expect(selectableOrgIds([option('1', false), option('2', true), option('3', false)])).toEqual([
      '1',
      '3',
    ]);
  });

  it('is empty when every organization is already added, and when there are none', () => {
    expect(selectableOrgIds([option('1', true), option('2', true)])).toEqual([]);
    expect(selectableOrgIds([])).toEqual([]);
  });

  it('tells "all of them are here already" from "the key sees none"', () => {
    // Both leave nothing to tick, and they are two different sentences on the screen.
    expect(allAlreadyAdded([option('1', true), option('2', true)])).toBe(true);
    expect(allAlreadyAdded([])).toBe(false);
    expect(allAlreadyAdded([option('1', true), option('2', false)])).toBe(false);
  });
});

describe('keyNameFor — which stored key an organization uses', () => {
  const creds = [cred('cred-1', 'meraki-prod', 'meraki_api'), cred('cred-2', 'meraki-lab', 'meraki_api')];

  it('is the credential’s name', () => {
    expect(keyNameFor({ credential_id: 'cred-2' }, creds)).toBe('meraki-lab');
  });

  it('is null when the caller may not read credentials, or the list has not arrived', () => {
    expect(keyNameFor({ credential_id: 'cred-1' }, null)).toBeNull();
  });

  it('is null — never the raw id — for a credential the list does not contain', () => {
    expect(keyNameFor({ credential_id: 'cred-gone' }, creds)).toBeNull();
    expect(keyNameFor({ credential_id: 'cred-1' }, [])).toBeNull();
  });
});
