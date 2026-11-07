// SPDX-License-Identifier: AGPL-3.0-only
// The folded "Scan settings" line on Discovery's Scan a range card (ADR-244 decision 3): what the
// sweep will use, said with the values themselves, so the settings can stay closed.

/** The values the summary reads. */
export interface ScanSettings {
  /** How many SNMP credentials are ticked. */
  credentials: number;
  /** The pool the sweep runs from, or `null` for any poller. */
  pool: string | null;
  /** Whether the "file by IP range" option is on screen at all (some folder carries a range). */
  fileByPrefixOffered: boolean;
  fileByPrefix: boolean;
  snmpWhenUnreachable: boolean;
}

/** One piece of the summary: a locale key under `monitoring:discovery.how.` and its values. */
export interface SummaryPart {
  key: 'creds' | 'noCreds' | 'pool' | 'anyPoller' | 'fileByPrefix' | 'snmpBlocked';
  values?: Record<string, string | number>;
}

/** The summary, in the order it reads: credentials, where it runs from, then the options that
 *  are on. An option that is off says nothing — the line is about what will happen. */
export function settingsSummary(s: ScanSettings): SummaryPart[] {
  const parts: SummaryPart[] = [
    s.credentials > 0 ? { key: 'creds', values: { count: s.credentials } } : { key: 'noCreds' },
    s.pool ? { key: 'pool', values: { pool: s.pool } } : { key: 'anyPoller' },
  ];
  if (s.fileByPrefixOffered && s.fileByPrefix) parts.push({ key: 'fileByPrefix' });
  if (s.snmpWhenUnreachable) parts.push({ key: 'snmpBlocked' });
  return parts;
}

/** Whether the settings must be open whatever the operator chose (decision 4): the chosen pool
 *  has no live poller, and the line saying so lives inside them. */
export function settingsForcedOpen(unroutedPool: boolean): boolean {
  return unroutedPool;
}
