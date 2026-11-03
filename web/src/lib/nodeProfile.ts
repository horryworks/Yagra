// SPDX-License-Identifier: AGPL-3.0-only
// What a node's profile is called and whether it is the generic one a device falls back to when no
// classification rule recognised it (ADR-231). `fallback` is decided by core; nothing here compares
// a profile's name.

import type { ProfileSummary } from '../types/api';

export interface NodeProfile {
  name: string;
  fallback: boolean;
}

/** The node's profile, or `null` when it has none (ICMP only) or the list does not hold it — a
 *  caller without the permission to read profiles gets an empty list, and then shows nothing. */
export function nodeProfileOf(
  profiles: readonly ProfileSummary[],
  profileId: string | null | undefined,
): NodeProfile | null {
  if (!profileId) return null;
  const p = profiles.find((x) => x.id === profileId);
  return p ? { name: p.name, fallback: p.fallback } : null;
}
