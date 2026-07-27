// SPDX-License-Identifier: AGPL-3.0-only
// Text helpers for the "Explain this incident" modal (ADR-029). Pure (no React) so the mapping
// from a typed API refusal to the one sentence the operator reads is unit-testable — that mapping
// is the whole difference between "the AI thing is broken" and "nobody has configured a provider".

import { ApiError } from '../../services/api';
import type { RcaNodeFacts } from '../../types/api';

/** Minimal shape of i18next's `t` — enough to map a code to a sentence without importing react. */
export type Translate = (key: string, opts?: Record<string, unknown>) => string;

/** Compact window label (`1h`, `30m`, `3d`) — the same unit spelling the range control uses. */
export function formatWindow(secs: number): string {
  if (secs > 0 && secs % 86400 === 0) return `${secs / 86400}d`;
  if (secs > 0 && secs % 3600 === 0) return `${secs / 3600}h`;
  return `${Math.round(secs / 60)}m`;
}

/** `name (address)`, with vendor/model appended when the inventory has them. */
export function nodeLine(n: RcaNodeFacts): string {
  const suffix = [n.vendor, n.model].filter(Boolean).join(' ');
  return suffix ? `${n.name} (${n.address}) · ${suffix}` : `${n.name} (${n.address})`;
}

/**
 * Map a refusal onto the sentence that tells the reader what to do about it.
 *
 * Each branch is a different next action — configure a provider, fix what is configured, wait,
 * ask someone with the operator role, look at a different alert, or check the vendor's status —
 * so they are not collapsed into one generic failure message.
 */
export function refusalText(e: unknown, t: Translate): string {
  if (!(e instanceof ApiError)) return t('err.generic');
  // Checked before the code: an unauthorized caller is told about the permission, not about
  // whatever the server would have said next.
  if (e.status === 401 || e.status === 403) return t('err.forbidden');
  switch (e.code) {
    case 'rca_not_configured':
      return t('err.notConfigured');
    case 'rca_misconfigured':
      return t('err.misconfigured', { message: e.message });
    case 'rate_limited':
      return t('err.rateLimited');
    case 'no_incident':
      return t('err.noIncident', { message: e.message });
    case 'provider_error':
    case 'model_refused':
      return t('err.provider', { message: e.message });
    default:
      return e.message || t('err.generic');
  }
}
