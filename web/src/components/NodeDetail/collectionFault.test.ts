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
      kind: 'meraki_api',
      org: 'Acme',
      orgPath: '/settings/integrations/meraki/00000000-0000-0000-0000-000000000ace',
      reasonKey: 'meraki.sync.reason.auth',
      sinceUnixMs: 1_790_000_000_000,
    });
  });

  it('falls back to the id for an organization the server could not name yet', () => {
    const got = collectionFaultNotice({ collection_fault: fault({ meraki_org_name: null }) });
    expect(got?.kind === 'meraki_api' && got.org).toBe('00000000-0000-0000-0000-000000000ace');
  });

  it('says since when for an access point no controller has reported, and names no controller (ADR-064 増分 G)', () => {
    // The server sends only the cause and the time: the controller is the overview row above.
    const ap: Fault = { cause: 'wireless_controller', since_unix_ms: 1_790_000_000_000 };
    expect(collectionFaultNotice({ collection_fault: ap })).toEqual({
      kind: 'wireless_controller',
      sinceUnixMs: 1_790_000_000_000,
    });
  });

  it('says nothing about a cause this bundle has never heard of, rather than the wrong sentence', () => {
    const future = { cause: 'a_cause_from_the_future', since_unix_ms: 1 } as unknown as Fault;
    expect(collectionFaultNotice({ collection_fault: future })).toBeNull();
  });

  it('gives no reason rather than a raw key, when there is none or it is one this bundle does not know', () => {
    const reasonKey = (f: Fault) => {
      const got = collectionFaultNotice({ collection_fault: f });
      return got?.kind === 'meraki_api' ? got.reasonKey : 'not a meraki notice';
    };
    expect(reasonKey(fault({ reason: null }))).toBeNull();
    expect(reasonKey(fault({ reason: 'a_reason_from_the_future' as Fault['reason'] }))).toBeNull();
  });

  it('builds a key that both locales carry, for every reason there is', () => {
    // The key is assembled here rather than inside a `t(`…`)` template, so the prefix check that
    // guards those cannot see it. This is that check, for this one call site.
    for (const reason of MERAKI_SYNC_FAILURES) {
      const got = collectionFaultNotice({ collection_fault: fault({ reason }) });
      const key = got?.kind === 'meraki_api' ? got.reasonKey : null;
      expect(key).toBe(`meraki.sync.reason.${reason}`);
      expect(en.meraki.sync.reason).toHaveProperty(reason);
      expect(ja.meraki.sync.reason).toHaveProperty(reason);
    }
  });
});
