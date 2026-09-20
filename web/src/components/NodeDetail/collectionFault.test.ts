// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import en from '../../locales/en/system.json';
import ja from '../../locales/ja/system.json';
import { MERAKI_SYNC_FAILURES, type NodeStatus } from '../../types/api';
import { collectionFaultNotice } from './collectionFault';

type Fault = NonNullable<NodeStatus['collection_fault']>;

const fault = (over: Partial<Fault> = {}): Fault => ({
  cause: 'meraki_api',
  meraki_org: '00000000-0000-0000-0000-000000000ace',
  meraki_org_name: 'Acme',
  reason: 'auth',
  since_unix_ms: 1_790_000_000_000,
  ...over,
});

describe('the node overview’s stale-state notice (ADR-164 決定 18)', () => {
  it('says nothing for a node whose state is a current reading', () => {
    expect(collectionFaultNotice(null)).toBeNull();
    expect(collectionFaultNotice(undefined)).toBeNull();
    expect(collectionFaultNotice({})).toBeNull();
    expect(collectionFaultNotice({ collection_fault: null })).toBeNull();
  });

  it('names the organization, why, since when, and where to fix it', () => {
    expect(collectionFaultNotice({ collection_fault: fault() })).toEqual({
      org: 'Acme',
      orgPath: '/settings/integrations/meraki/00000000-0000-0000-0000-000000000ace',
      reasonKey: 'meraki.sync.reason.auth',
      sinceUnixMs: 1_790_000_000_000,
    });
  });

  it('falls back to the id for an organization the server could not name yet', () => {
    const got = collectionFaultNotice({ collection_fault: fault({ meraki_org_name: null }) });
    expect(got?.org).toBe('00000000-0000-0000-0000-000000000ace');
  });

  it('gives no reason rather than a raw key, when there is none or it is one this bundle does not know', () => {
    expect(collectionFaultNotice({ collection_fault: fault({ reason: null }) })?.reasonKey).toBeNull();
    const future = fault({ reason: 'a_reason_from_the_future' as Fault['reason'] });
    expect(collectionFaultNotice({ collection_fault: future })?.reasonKey).toBeNull();
  });

  it('builds a key that both locales carry, for every reason there is', () => {
    // The key is assembled here rather than inside a `t(`…`)` template, so the prefix check that
    // guards those cannot see it. This is that check, for this one call site.
    for (const reason of MERAKI_SYNC_FAILURES) {
      const key = collectionFaultNotice({ collection_fault: fault({ reason }) })?.reasonKey;
      expect(key).toBe(`meraki.sync.reason.${reason}`);
      expect(en.meraki.sync.reason).toHaveProperty(reason);
      expect(ja.meraki.sync.reason).toHaveProperty(reason);
    }
  });
});
