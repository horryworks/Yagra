// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { NetboxSiteIdFields } from '../../types/api';
import {
  SITE_ID_NONE,
  SITE_ID_OTHER,
  customKey,
  customKeyLooksValid,
  customValue,
  isCustom,
  selectionFor,
  siteIdFieldToSend,
  siteIdOptions,
  siteIdOutcome,
} from './siteIdField';

/** The lab's real answer: one custom field, `site_id`, labelled "Site ID". */
const LAB: NetboxSiteIdFields = {
  custom_fields_readable: true,
  custom_fields: [{ value: 'cf:site_id', label: 'Site ID' }],
  built_ins: ['slug', 'facility', 'description'],
};

const REFUSED: NetboxSiteIdFields = {
  custom_fields_readable: false,
  custom_fields: [],
  built_ins: ['slug', 'facility', 'description'],
};

describe('the picker rows', () => {
  it('offers none, every built-in, the custom fields, and an escape hatch', () => {
    const rows = siteIdOptions(LAB, null);
    expect(rows.map((r) => r.kind)).toEqual([
      'none',
      'builtIn',
      'builtIn',
      'builtIn',
      'custom',
      'other',
    ]);
    expect(rows).toContainEqual({ kind: 'custom', value: 'cf:site_id', label: 'Site ID' });
  });

  it('is never empty when the definitions could not be read', () => {
    // 🚨 This is the case the whole `custom_fields_readable` split exists for. A token without
    // `extras.view_customfield` yields no custom fields — and if the picker collapsed to nothing
    // the operator would have no way to reach the feature at all.
    const rows = siteIdOptions(REFUSED, null);
    expect(rows.filter((r) => r.kind === 'builtIn')).toHaveLength(3);
    expect(rows.at(-1)).toEqual({ kind: 'other' });
  });

  it('is never empty before the list has been fetched', () => {
    expect(siteIdOptions(null, null).length).toBeGreaterThan(2);
  });

  it('keeps a saved value the listing does not account for', () => {
    // The edit form renders before the fetch returns, and may never get a list at all. A server
    // set to `cf:site_id` must not appear as "none" in either case.
    const rows = siteIdOptions(null, 'cf:site_id');
    expect(rows).toContainEqual({ kind: 'custom', value: 'cf:site_id', label: 'site_id' });
    // …and it is not duplicated once the listing does mention it.
    const withList = siteIdOptions(LAB, 'cf:site_id');
    expect(withList.filter((r) => r.kind === 'custom')).toHaveLength(1);
  });
});

describe('the stored encoding', () => {
  it('round-trips a custom key', () => {
    expect(customValue('site_id')).toBe('cf:site_id');
    expect(customKey('cf:site_id')).toBe('site_id');
    expect(isCustom('cf:site_id')).toBe(true);
    expect(isCustom('facility')).toBe(false);
    // Whitespace an operator pasted is not part of the key — the same trim the API applies.
    expect(customValue('  site_id \n')).toBe('cf:site_id');
  });
});

describe('what a stored setting selects', () => {
  it('selects nothing for null and the row itself for a built-in', () => {
    expect(selectionFor(null, LAB)).toEqual({ selected: SITE_ID_NONE, customKeyInput: '' });
    expect(selectionFor('facility', LAB)).toEqual({ selected: 'facility', customKeyInput: '' });
  });

  it('selects a known custom field directly', () => {
    expect(selectionFor('cf:site_id', LAB)).toEqual({
      selected: 'cf:site_id',
      customKeyInput: '',
    });
  });

  it('falls back to Other with the key filled in when the listing does not know it', () => {
    // The realistic causes: the token cannot read the definitions, the field was removed in
    // NetBox, or the value arrived from a REST client. In all three the value must stay editable
    // instead of disappearing from the form.
    expect(selectionFor('cf:legacy_code', REFUSED)).toEqual({
      selected: SITE_ID_OTHER,
      customKeyInput: 'legacy_code',
    });
    expect(selectionFor('cf:legacy_code', null)).toEqual({
      selected: SITE_ID_OTHER,
      customKeyInput: 'legacy_code',
    });
  });
});

describe('what gets sent', () => {
  it('sends null for none and the value for anything chosen', () => {
    expect(siteIdFieldToSend(SITE_ID_NONE, '')).toBeNull();
    expect(siteIdFieldToSend('slug', '')).toBe('slug');
    expect(siteIdFieldToSend('cf:site_id', '')).toBe('cf:site_id');
  });

  it('encodes a typed key, and sends null rather than an empty cf:', () => {
    expect(siteIdFieldToSend(SITE_ID_OTHER, 'site_id')).toBe('cf:site_id');
    expect(siteIdFieldToSend(SITE_ID_OTHER, '  ')).toBeNull();
    expect(siteIdFieldToSend(SITE_ID_OTHER, '')).toBeNull();
  });
});

describe('the typed-key check', () => {
  it('accepts what the backend accepts', () => {
    for (const ok of ['site_id', 'SiteID', 'a', 'a_1', 'x'.repeat(64)]) {
      expect(customKeyLooksValid(ok)).toBe(true);
    }
  });

  it('refuses what the backend would 400', () => {
    for (const bad of ['', '   ', 'site id', 'site-id', '../etc', 'x'.repeat(65), 'cf:site_id']) {
      expect(customKeyLooksValid(bad)).toBe(false);
    }
  });
});

describe('the sync outcome', () => {
  it('says nothing when every site got a code', () => {
    expect(siteIdOutcome(2, 0)).toBeNull();
  });

  it('separates "none of them" from "some of them"', () => {
    // 🚨 "None of them" is the wrong-field symptom, and it is the one that otherwise looks exactly
    // like the feature not working: no error, no changed name, nothing.
    expect(siteIdOutcome(2, 2)).toEqual({ kind: 'none', without: 2, sites: 2 });
    expect(siteIdOutcome(5, 2)).toEqual({ kind: 'partial', without: 2, sites: 5 });
  });
});
