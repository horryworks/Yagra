// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { ReclassifyProposal } from '../types/api';
import { applyItems, emptyState, pruneSelection, ruleSignature } from './reclassify';

describe('emptyState', () => {
  it('says nothing has been compared yet when no node is identified', () => {
    expect(emptyState({ identified: 0, unidentified: 27 })).toEqual({
      key: 'reclassify.emptyUnidentified',
      count: 27,
    });
  });

  it('says every identified node matches once any node is identified', () => {
    // Some nodes are never identified (ping-only, Meraki API), so a count of them must not keep the
    // "not collected yet" sentence up for ever once others have been compared.
    expect(emptyState({ identified: 20, unidentified: 7 }).key).toBe('reclassify.empty');
  });

  it('falls back to the ordinary sentence before the first read and on an empty inventory', () => {
    expect(emptyState(null).key).toBe('reclassify.empty');
    expect(emptyState({ identified: 0, unidentified: 0 }).key).toBe('reclassify.empty');
  });
});

const proposal = (over: Partial<ReclassifyProposal> = {}): ReclassifyProposal =>
  ({
    node_id: 'n-1',
    node_name: 'ale-sw01',
    current_profile_id: 'nokia',
    current_profile_name: 'Nokia SR router',
    suggested_profile_id: 'omniswitch',
    suggested_profile_name: 'Alcatel-Lucent OmniSwitch',
    rule: {
      id: 'r-28',
      priority: 340,
      sysobjectid_prefix: '1.3.6.1.4.1.6486.',
      sysdescr_regex: null,
    },
    vendor: 'Alcatel-Lucent',
    model: null,
    sys_object_id: '1.3.6.1.4.1.6486.801.1.1.2.1.11.1.9',
    sys_descr: 'Alcatel-Lucent Enterprise OS6860E-U28',
    ...over,
  }) as ReclassifyProposal;

describe('applyItems', () => {
  it('sends only the selected rows, each with the profile the screen showed it on', () => {
    const rows = [
      proposal(),
      proposal({ node_id: 'n-2', current_profile_id: null }),
      proposal({ node_id: 'n-3' }),
    ];
    expect(applyItems(rows, new Set(['n-2', 'n-1']))).toEqual([
      { node_id: 'n-1', from_profile_id: 'nokia', to_profile_id: 'omniswitch' },
      // A node with no profile is sent as `null`, never omitted — the server compares it.
      { node_id: 'n-2', from_profile_id: null, to_profile_id: 'omniswitch' },
    ]);
  });

  it('ignores a selected id that is not on the screen', () => {
    expect(applyItems([proposal()], new Set(['gone']))).toEqual([]);
  });
});

describe('pruneSelection', () => {
  it('keeps only the nodes the reloaded list still shows', () => {
    const kept = pruneSelection(new Set(['n-1', 'applied', 'locked']), [proposal()]);
    expect([...kept]).toEqual(['n-1']);
  });
});

describe('ruleSignature', () => {
  it('writes the matchers the rule has, joined the way they combine', () => {
    expect(ruleSignature(proposal())).toBe('1.3.6.1.4.1.6486.');
    expect(
      ruleSignature(
        proposal({
          rule: {
            id: 'r-4',
            priority: 25,
            sysobjectid_prefix: '1.3.6.1.4.1.9.',
            sysdescr_regex: '(?i)firepower threat defense',
          },
        }),
      ),
    ).toBe('1.3.6.1.4.1.9. + (?i)firepower threat defense');
  });

  it('is null when no rule matched, so the screen can say Generic SNMP in words', () => {
    expect(ruleSignature(proposal({ rule: null }))).toBeNull();
  });
});
