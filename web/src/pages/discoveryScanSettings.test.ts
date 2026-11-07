// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { settingsForcedOpen, settingsSummary, type ScanSettings } from './discoveryScanSettings';

const base: ScanSettings = {
  credentials: 3,
  pool: null,
  fileByPrefixOffered: true,
  fileByPrefix: true,
  snmpWhenUnreachable: false,
};

describe('settingsSummary', () => {
  it('names the credential count, the poller and the options that are on, in that order', () => {
    expect(settingsSummary(base)).toEqual([
      { key: 'creds', values: { count: 3 } },
      { key: 'anyPoller' },
      { key: 'fileByPrefix' },
    ]);
  });

  it('says ping only when no credential is ticked', () => {
    expect(settingsSummary({ ...base, credentials: 0 })[0]).toEqual({ key: 'noCreds' });
  });

  it('names the chosen pool', () => {
    expect(settingsSummary({ ...base, pool: 'site-b' })[1]).toEqual({
      key: 'pool',
      values: { pool: 'site-b' },
    });
  });

  it('leaves out filing when the option is not offered, even if it is on', () => {
    const keys = settingsSummary({ ...base, fileByPrefixOffered: false }).map((p) => p.key);
    expect(keys).not.toContain('fileByPrefix');
  });

  it('leaves out an option that is off and adds the SNMP fallback when it is on', () => {
    const keys = settingsSummary({ ...base, fileByPrefix: false, snmpWhenUnreachable: true }).map(
      (p) => p.key,
    );
    expect(keys).toEqual(['creds', 'anyPoller', 'snmpBlocked']);
  });
});

describe('settingsForcedOpen', () => {
  it('opens the settings only while the chosen pool has no live poller', () => {
    expect(settingsForcedOpen(true)).toBe(true);
    expect(settingsForcedOpen(false)).toBe(false);
  });
});
