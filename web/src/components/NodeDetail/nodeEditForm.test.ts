// SPDX-License-Identifier: AGPL-3.0-only
// The Edit-node dialog's judgement: which fields a kind gets, and what one Save writes.
//
// The regression these pin: the dialog rendered an SNMP credential picker on a URL and a DNS
// monitor, where `node.credential_id` is written by nothing that ever reads it. The second, worse
// hazard is the fix's own: `PUT /nodes/{id}/bindings` replaces profile/credential/vendor/model
// unconditionally, so a field the dialog stops *showing* must still be *sent* — otherwise hiding
// the SNMP credential on a URL node would blank it on the first save, and "your maker/model
// vanished when you edited the URL" is a worse bug than the one being fixed.

import { beforeEach, describe, expect, it, vi } from 'vitest';

const setUrlCheck = vi.fn();
const setDnsCheck = vi.fn();
const setNodeBindings = vi.fn();

vi.mock('../../services/api', () => ({
  api: {
    setUrlCheck: (...a: unknown[]) => setUrlCheck(...a),
    setDnsCheck: (...a: unknown[]) => setDnsCheck(...a),
    setNodeBindings: (...a: unknown[]) => setNodeBindings(...a),
  },
}));

import { NODE_KINDS, type NodeDetail, type NodeKind, type ProfileSummary } from '../../types/api';
import { PROFILE_CATEGORIES } from '../../lib/profileCategories';
import {
  NODE_EDIT_FIELDS,
  NODE_EDIT_FIELD_META,
  NODE_EDIT_KIND_SPEC,
  nodeEditDraftFrom,
  nodeEditErrorKey,
  nodeEditRequest,
  profileIsOffKind,
  profileOptions,
  sendNodeEdit,
  visibleNodeEditFields,
  visibleNodeEditSections,
  type NodeEditRequest,
} from './nodeEditForm';

/** A node carrying a value in EVERY binding, whatever its kind — the shape that catches a dropped
 *  passthrough. Real url/dns nodes rarely have a vendor, but rows created before the kinds existed
 *  (or by an operator using the old dialog, which offered all five fields to everyone) do. */
const node = (over: Partial<NodeDetail> = {}): NodeDetail =>
  ({
    id: 'n-1',
    name: 'edge-1',
    address: '10.0.0.1',
    kind: 'device',
    profile_id: 'prof-1',
    credential_id: 'cred-1',
    vendor: 'Cisco',
    model: 'C9300',
    pool: 'site-a',
    ...over,
  }) as NodeDetail;

const urlNode = (over: Partial<NodeDetail> = {}) =>
  node({
    kind: 'url',
    url_check: { url: 'https://example.test/health' },
    ...over,
  });

const dnsNode = (over: Partial<NodeDetail> = {}) =>
  node({ kind: 'dns', dns_check: { name: 'example.test' }, ...over });

const profile = (id: string, category: string): ProfileSummary => ({ id, name: id, category });

const req = (r: { req: NodeEditRequest } | { error: string }): NodeEditRequest => {
  if ('error' in r) throw new Error(`unexpectedly rejected: ${r.error}`);
  return r.req;
};

describe('node edit field registry', () => {
  it('covers exactly the backend node kinds', () => {
    expect(Object.keys(NODE_EDIT_KIND_SPEC).sort()).toEqual([...NODE_KINDS].sort());
    expect(Object.keys(NODE_EDIT_FIELD_META).sort()).toEqual([...NODE_EDIT_FIELDS].sort());
  });

  it('gives every field at least one kind, and every kind at least one field', () => {
    for (const f of NODE_EDIT_FIELDS) {
      const kinds = NODE_EDIT_FIELD_META[f].kinds;
      expect(kinds.length, f).toBeGreaterThan(0);
      expect(new Set(kinds).size, f).toBe(kinds.length);
      for (const k of kinds) expect(NODE_KINDS, f).toContain(k);
    }
    for (const k of NODE_KINDS) expect(visibleNodeEditFields(k).length, k).toBeGreaterThan(0);
  });

  // ⚠️ Deliberately verbatim, and load-bearing: membership is keyed by field, so nothing else makes
  // a NEW KIND get a considered answer for each field. Do not replace these with a derivation.
  it('pins the per-kind field lists', () => {
    expect([...visibleNodeEditFields('device')]).toEqual([
      'profile',
      'snmpCredential',
      'identity',
      'pool',
    ]);
    expect([...visibleNodeEditFields('meraki')]).toEqual(['profile', 'identity', 'pool']);
    expect([...visibleNodeEditFields('url')]).toEqual(['urlCheck', 'profile', 'pool']);
    expect([...visibleNodeEditFields('dns')]).toEqual(['dnsCheck', 'profile', 'pool']);
  });

  it('keeps each section contiguous, and heads a dialog only when it has two', () => {
    // The dialog draws a heading where the section changes, so an interleaved order would draw
    // "Node settings" twice with a check field between them.
    for (const kind of NODE_KINDS) {
      const sections = visibleNodeEditFields(kind).map((f) => NODE_EDIT_FIELD_META[f].section);
      const runs = sections.filter((s, i) => i === 0 || s !== sections[i - 1]);
      expect(new Set(runs).size, kind).toBe(runs.length);
      expect([...visibleNodeEditSections(kind)], kind).toEqual(runs);
    }
    expect(visibleNodeEditSections('device')).toEqual(['node']);
    expect(visibleNodeEditSections('meraki')).toEqual(['node']);
    expect(visibleNodeEditSections('url')).toEqual(['check', 'node']);
    expect(visibleNodeEditSections('dns')).toEqual(['check', 'node']);
  });

  // The reported bug, as a biconditional so neither direction can drift.
  it('offers an SNMP credential to an ordinary device and to nothing else', () => {
    for (const kind of NODE_KINDS) {
      expect(visibleNodeEditFields(kind).includes('snmpCredential'), kind).toBe(kind === 'device');
    }
  });

  it('gives each monitor kind its own check block and no other', () => {
    for (const kind of NODE_KINDS) {
      const fields = visibleNodeEditFields(kind);
      expect(fields.includes('urlCheck'), kind).toBe(kind === 'url');
      expect(fields.includes('dnsCheck'), kind).toBe(kind === 'dns');
    }
  });

  it('keeps placement editable for every kind', () => {
    // Where a node is filed and how often it is polled are true of a monitor exactly as of a switch.
    for (const kind of NODE_KINDS) {
      expect(visibleNodeEditFields(kind), kind).toContain('profile');
      expect(visibleNodeEditFields(kind), kind).toContain('pool');
    }
  });

  it('renders fields in registry order, so no kind reshuffles the dialog', () => {
    for (const kind of NODE_KINDS) {
      const at = visibleNodeEditFields(kind).map((f) => NODE_EDIT_FIELDS.indexOf(f));
      expect(at, kind).toEqual([...at].sort((a, b) => a - b));
    }
    // The check block leads: the operator opened this to change what is monitored.
    expect(visibleNodeEditFields('url')[0]).toBe('urlCheck');
    expect(visibleNodeEditFields('dns')[0]).toBe('dnsCheck');
  });

  it('names a real profile category for the kinds that filter on one', () => {
    // A typo here empties the picker, which looks exactly like "no profiles are configured".
    const tokens = PROFILE_CATEGORIES.map((c) => c.token);
    for (const kind of NODE_KINDS) {
      const cat = NODE_EDIT_KIND_SPEC[kind].profileCategory;
      if (cat !== null) expect(tokens, kind).toContain(cat);
    }
    expect(NODE_EDIT_KIND_SPEC.url.profileCategory).toBe('url-check');
    expect(NODE_EDIT_KIND_SPEC.dns.profileCategory).toBe('dns-check');
    expect(NODE_EDIT_KIND_SPEC.device.profileCategory).toBeNull();
  });
});

describe('profileOptions', () => {
  const all = [
    profile('router-1', 'router'),
    profile('url-1', 'url-check'),
    profile('dns-1', 'dns-check'),
    profile('generic-1', 'generic-snmp'),
  ];

  it('offers a monitor only its own category', () => {
    expect(profileOptions('url', all, '').map((p) => p.id)).toEqual(['url-1']);
    expect(profileOptions('dns', all, '').map((p) => p.id)).toEqual(['dns-1']);
  });

  it('offers a device every category that is not a monitor kind', () => {
    for (const kind of ['device', 'meraki'] as const) {
      expect(profileOptions(kind, all, '').map((p) => p.id)).toEqual(['router-1', 'generic-1']);
    }
  });

  it('keeps the bound profile even when the filter would hide it', () => {
    // A `<select>` whose value matches no option renders blank, and saving that would read as a
    // deliberate re-bind. Legacy url/dns nodes bound to a device profile are exactly this case.
    const opts = profileOptions('url', all, 'router-1');
    expect(opts.map((p) => p.id)).toEqual(['router-1', 'url-1']);
    expect(new Set(opts.map((p) => p.id)).size).toBe(opts.length);
  });

  it('does not duplicate the bound profile when the filter already keeps it', () => {
    expect(profileOptions('url', all, 'url-1').map((p) => p.id)).toEqual(['url-1']);
  });

  it('preserves the caller ordering', () => {
    const reversed = [...all].reverse();
    expect(profileOptions('device', reversed, '').map((p) => p.id)).toEqual([
      'generic-1',
      'router-1',
    ]);
  });

  it('flags only a binding the filter would have hidden', () => {
    expect(profileIsOffKind('url', all, 'router-1')).toBe(true);
    expect(profileIsOffKind('url', all, 'url-1')).toBe(false);
    expect(profileIsOffKind('device', all, 'url-1')).toBe(true);
    expect(profileIsOffKind('device', all, 'router-1')).toBe(false);
    // Nothing bound, or a profile the listing does not carry: no claim either way.
    expect(profileIsOffKind('url', all, '')).toBe(false);
    expect(profileIsOffKind('url', all, 'gone')).toBe(false);
  });
});

describe('nodeEditRequest bindings', () => {
  // ⚠️ The whole point of the fix. `set_node_bindings` is an unconditional
  // `UPDATE nodes SET profile_id=$2, credential_id=$3, vendor=$4, model=$5`, and serde defaults a
  // missing `Option` to `None` — so an omitted field is a CLEARED field.
  it('resends every binding for every kind, including the ones that kind hides', () => {
    const cases: [NodeKind, NodeDetail][] = [
      ['device', node()],
      ['meraki', node({ kind: 'meraki' })],
      ['url', urlNode()],
      ['dns', dnsNode()],
    ];
    for (const [kind, n] of cases) {
      const { bindings } = req(nodeEditRequest(kind, nodeEditDraftFrom(n)));
      expect(Object.keys(bindings).sort(), kind).toEqual([
        'credential_id',
        'model',
        'pool',
        'profile_id',
        'vendor',
      ]);
      expect(bindings, kind).toEqual({
        profile_id: 'prof-1',
        credential_id: 'cred-1',
        vendor: 'Cisco',
        model: 'C9300',
        pool: 'site-a',
      });
    }
  });

  it('sends the pool as a string so blanking it means inherit', () => {
    // A JSON null reads server-side as "leave unchanged" and would silently drop the edit.
    for (const kind of NODE_KINDS) {
      const draft = { ...nodeEditDraftFrom(node()), pool: '  ' };
      expect(req(nodeEditRequest(kind, draft)).bindings.pool, kind).toBe('');
    }
    expect(
      req(nodeEditRequest('device', { ...nodeEditDraftFrom(node()), pool: ' site-b ' })).bindings
        .pool,
    ).toBe('site-b');
  });

  it('clears a binding the operator emptied', () => {
    const draft = { ...nodeEditDraftFrom(node()), profileId: '', credentialId: '', vendor: '  ' };
    const { bindings } = req(nodeEditRequest('device', draft));
    expect(bindings.profile_id).toBeNull();
    expect(bindings.credential_id).toBeNull();
    expect(bindings.vendor).toBeNull();
  });

  it('reads absent bindings as empty rather than undefined', () => {
    const draft = nodeEditDraftFrom(node({ profile_id: null, credential_id: null, pool: null }));
    expect(draft).toMatchObject({ profileId: '', credentialId: '', pool: '' });
  });
});

describe('nodeEditRequest check half', () => {
  it('carries the check only for the kind that owns one', () => {
    expect(req(nodeEditRequest('device', nodeEditDraftFrom(node()))).check).toBeNull();
    expect(req(nodeEditRequest('meraki', nodeEditDraftFrom(node({ kind: 'meraki' })))).check).toBe(
      null,
    );
    expect(req(nodeEditRequest('url', nodeEditDraftFrom(urlNode()))).check).toEqual({
      kind: 'url',
      body: expect.objectContaining({ url: 'https://example.test/health' }),
    });
    expect(req(nodeEditRequest('dns', nodeEditDraftFrom(dnsNode()))).check).toEqual({
      kind: 'dns',
      body: expect.objectContaining({ name: 'example.test' }),
    });
  });

  it('never sends the check belonging to another kind, even when both rows exist', () => {
    // The API edge refuses a second check row, but rows predating that guard exist — and `kind` is
    // the resolved answer, so it decides here too rather than "whichever config is non-null".
    const both = urlNode({ dns_check: { name: 'example.test' } });
    expect(req(nodeEditRequest('url', nodeEditDraftFrom(both))).check?.kind).toBe('url');
  });

  it('propagates a refusal from the check form verbatim', () => {
    const draft = nodeEditDraftFrom(urlNode());
    expect(nodeEditRequest('url', { ...draft, url: { ...draft.url!, url: 'ftp://x' } })).toEqual({
      error: 'urlScheme',
    });
    const dns = nodeEditDraftFrom(dnsNode());
    expect(nodeEditRequest('dns', { ...dns, dns: { ...dns.dns!, name: '  ' } })).toEqual({
      error: 'dnsNameRequired',
    });
  });

  it('still saves the bindings for a monitor whose check row is somehow missing', () => {
    const { check, bindings } = req(
      nodeEditRequest('url', { ...nodeEditDraftFrom(node({ kind: 'url' })), url: null }),
    );
    expect(check).toBeNull();
    expect(bindings.pool).toBe('site-a');
  });
});

describe('sendNodeEdit', () => {
  beforeEach(() => {
    setUrlCheck.mockReset().mockResolvedValue(undefined);
    setDnsCheck.mockReset().mockResolvedValue(undefined);
    setNodeBindings.mockReset().mockResolvedValue(undefined);
  });

  const urlReq = () => req(nodeEditRequest('url', nodeEditDraftFrom(urlNode())));

  it('writes the check before the bindings', () => {
    // Order is the whole partial-failure story: the check PUT is the one the server can still
    // refuse (SSRF target, credential without TLS), so a rejection must leave nothing written.
    return sendNodeEdit('n-1', urlReq()).then((out) => {
      expect(out).toEqual({ ok: true });
      expect(setUrlCheck).toHaveBeenCalledTimes(1);
      expect(setNodeBindings).toHaveBeenCalledTimes(1);
      expect(setUrlCheck.mock.invocationCallOrder[0]).toBeLessThan(
        setNodeBindings.mock.invocationCallOrder[0],
      );
    });
  });

  it('does not touch the bindings when the check is refused', async () => {
    setUrlCheck.mockRejectedValue(new Error('ssrf'));
    const out = await sendNodeEdit('n-1', urlReq());
    expect(out).toMatchObject({ ok: false, stage: 'check' });
    expect(setNodeBindings).not.toHaveBeenCalled();
  });

  it('reports which half landed when the bindings are refused', async () => {
    setNodeBindings.mockRejectedValue(new Error('nope'));
    const out = await sendNodeEdit('n-1', urlReq());
    expect(out).toMatchObject({ ok: false, stage: 'bindings' });
    expect(setUrlCheck).toHaveBeenCalledTimes(1);
  });

  it('sends only the bindings for a kind with no check', async () => {
    await sendNodeEdit('n-1', req(nodeEditRequest('device', nodeEditDraftFrom(node()))));
    expect(setUrlCheck).not.toHaveBeenCalled();
    expect(setDnsCheck).not.toHaveBeenCalled();
    expect(setNodeBindings).toHaveBeenCalledTimes(1);
  });

  it('uses the DNS writer for a DNS monitor', async () => {
    await sendNodeEdit('n-1', req(nodeEditRequest('dns', nodeEditDraftFrom(dnsNode()))));
    expect(setDnsCheck).toHaveBeenCalledTimes(1);
    expect(setUrlCheck).not.toHaveBeenCalled();
  });
});

describe('nodeEditErrorKey', () => {
  const withCheck = { check: { kind: 'url' as const, body: {} }, bindings: {} } as NodeEditRequest;
  const noCheck = { check: null, bindings: {} } as NodeEditRequest;

  it('says the monitor config failed when that is the half that failed', () => {
    expect(nodeEditErrorKey(withCheck, 'check')).toBe('checkEdit.err.save');
  });

  it('says what already landed when only the bindings failed', () => {
    // "failed to save node" would be a lie about the half that succeeded, and the operator would
    // reopen the dialog to find their URL change applied with no sign the pool move was not.
    expect(nodeEditErrorKey(withCheck, 'bindings')).toBe('editNode.err.bindingsAfterCheck');
    expect(nodeEditErrorKey(noCheck, 'bindings')).toBe('err.saveNode');
  });
});
