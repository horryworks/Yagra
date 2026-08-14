// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { channelKindLabel, channelKindOptions } from './channelKinds';
import { CHANNEL_KINDS } from '../types/api';

describe('channel kind labels', () => {
  it('offers exactly the kinds the union declares, in its order', () => {
    // The dialog's option list must not be able to drift from the union again — that drift is why
    // this module exists. `Record<ChannelKind, string>` makes a missing label a compile error;
    // this makes a missing *option* a test failure.
    expect(channelKindOptions().map((o) => o.value)).toEqual([...CHANNEL_KINDS]);
  });

  it('labels every declared kind with something other than its own token', () => {
    // The bug being fixed was a column whose filter read `pagerduty` while the dialog above it
    // read `PagerDuty`. Falling back to the token is right for a kind we do not know; for a kind
    // we *do* know it means the label was never written, which is what the filter was doing.
    for (const k of CHANNEL_KINDS) {
      expect(channelKindLabel(k), k).not.toBe(k);
    }
    expect(channelKindLabel('pagerduty')).toBe('PagerDuty');
    expect(channelKindLabel('jsm')).toBe('Jira Service Management');
  });

  it('renders an unknown kind as its own token rather than as blank', () => {
    // A core one version ahead can serve a kind this build has never heard of. The filter builds
    // its options from the rows it was given, so that token reaches this function.
    expect(channelKindLabel('kafka')).toBe('kafka');
    expect(channelKindLabel('')).toBe('');
  });
});
