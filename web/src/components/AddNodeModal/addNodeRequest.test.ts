// SPDX-License-Identifier: AGPL-3.0-only
// What the add-node form actually sends. The form keeps every kind's fields in one object, so the
// interesting cases are the ones where a field belongs to a kind other than the one being created.

import { describe, expect, it } from 'vitest';
import { ADDABLE_KINDS } from '../../pages/monitorKinds';
import { createRequest, EMPTY_ADD_NODE_FORM, type AddNodeForm } from './addNodeRequest';

const form = (over: Partial<AddNodeForm> = {}): AddNodeForm => ({
  ...EMPTY_ADD_NODE_FORM,
  name: 'edge-1',
  ...over,
});

describe('createRequest', () => {
  it('sends only the fields belonging to the kind being created', () => {
    // The operator can fill in an address, then switch the select to "URL monitor" — the address
    // stays in state. Carrying it into the URL body would bind a monitor to a device address it
    // never polls; carrying a URL into a device body would be rejected by the API at best.
    const filled = form({ address: '10.0.0.1', url: 'https://example.com', dnsName: 'example.com' });

    const url = createRequest('url', filled);
    expect(url.body).not.toHaveProperty('address');
    expect(url.body).not.toHaveProperty('dns_name');
    expect(url.body.url).toBe('https://example.com');

    const dns = createRequest('dns', filled);
    expect(dns.body).not.toHaveProperty('address');
    expect(dns.body).not.toHaveProperty('url');
    expect(dns.body.dns_name).toBe('example.com');

    const device = createRequest('device', filled);
    expect(device.body).not.toHaveProperty('url');
    expect(device.body).not.toHaveProperty('dns_name');
    expect(device.body.address).toBe('10.0.0.1');
  });

  it('omits a blank pool rather than sending an empty one', () => {
    // `pool: ''` is not "inherit" — it names a pool called "", which no poller serves, so the node
    // would be created and then never polled by anything.
    for (const kind of ADDABLE_KINDS) {
      expect(createRequest(kind, form({ pool: '   ' })).body.pool).toBeUndefined();
      expect(createRequest(kind, form({ pool: ' site-a ' })).body.pool).toBe('site-a');
    }
  });

  it('omits a blank parent rather than sending an empty id', () => {
    for (const kind of ADDABLE_KINDS) {
      expect(createRequest(kind, form()).body.parent_id).toBeUndefined();
      expect(createRequest(kind, form({ parentId: 'p1' })).body.parent_id).toBe('p1');
    }
  });

  it('omits a blank DNS resolver so the poller uses its system resolver', () => {
    expect(createRequest('dns', form()).body.resolver).toBeUndefined();
    expect(createRequest('dns', form({ dnsResolver: ' 1.1.1.1 ' })).body.resolver).toBe('1.1.1.1');
  });

  it('omits blank maker/model but keeps the address verbatim', () => {
    const req = createRequest('device', form({ vendor: '  ', model: ' MX240 ', address: '10.0.0.1' }));
    expect(req.body.vendor).toBeUndefined();
    expect(req.body.model).toBe('MX240');
    // Not trimmed: the address is validated server-side, and quietly rewriting what the operator
    // typed would hide a paste error rather than surface it.
    expect(req.body.address).toBe('10.0.0.1');
  });

  it('tags each request with the kind it was built for', () => {
    for (const kind of ADDABLE_KINDS) expect(createRequest(kind, form()).kind).toBe(kind);
  });
});
