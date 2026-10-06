// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { defaultRouteChannels } from './defaultRoute';

describe('defaultRouteChannels', () => {
  it('is null when the core has no default route', () => {
    expect(defaultRouteChannels({ configured: false, kinds: [] })).toBeNull();
  });

  it('names each kind with the label the channel list uses, in order', () => {
    expect(defaultRouteChannels({ configured: true, kinds: ['webhook'] })).toBe('Webhook');
    expect(defaultRouteChannels({ configured: true, kinds: ['webhook', 'email'] })).toBe(
      'Webhook, Email',
    );
  });

  it('does not claim a route that names no kind', () => {
    expect(defaultRouteChannels({ configured: true, kinds: [] })).toBeNull();
  });
});
