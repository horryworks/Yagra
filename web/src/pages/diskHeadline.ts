// SPDX-License-Identifier: AGPL-3.0-only
/** The System Health Disk card's headline, as a pure function.
 *
 *  Two branches, and the operator has to be able to tell them apart at a glance: a real filesystem
 *  reports a capacity and reads as "used / size · pct", while the PostgreSQL `database` proxy has no
 *  capacity at all (`size_bytes == 0`) and can only report its own size. Showing `0 B` or `100%` for
 *  the latter would invent an outage; showing the bare size says exactly what is known.
 *
 *  Lives in a `.ts` because Vitest here runs `environment: 'node'` with
 *  `include: ['src/ **\/*.test.ts']` — inside `SystemHealthPage.tsx` neither branch was reachable
 *  by a test. */

import { formatBytes, formatUtil } from '../lib/format';
import type { DiskUsage } from '../types/api';

export function diskHeadline(current: DiskUsage | undefined): string {
  if (!current) return '—';
  if (current.size_bytes > 0) {
    const pct = (current.used_bytes / current.size_bytes) * 100;
    return `${formatBytes(current.used_bytes)} / ${formatBytes(current.size_bytes)} · ${formatUtil(pct)}`;
  }
  return formatBytes(current.used_bytes);
}
