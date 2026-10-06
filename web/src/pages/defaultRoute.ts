// SPDX-License-Identifier: AGPL-3.0-only
// The env-configured default notification route, as the Notification delivery screen writes it
// (ADR-200 Inc.28). The core reports kinds only - never the URL or the mail settings - so the
// line can name what it sends to and nothing about where.

import { channelKindLabel } from '../lib/channelKinds';
import type { NotificationDefaultRoute } from '../types/api';

/** The channel kinds the default route sends to, as one comma-separated label, or `null` when
 *  this core has no default route. */
export function defaultRouteChannels(route: NotificationDefaultRoute): string | null {
  if (!route.configured || route.kinds.length === 0) return null;
  return route.kinds.map(channelKindLabel).join(', ');
}
