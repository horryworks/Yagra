// SPDX-License-Identifier: AGPL-3.0-only
// Small Troubleshoot display helpers: relative timestamps and turning a stored job's params
// back into a re-run request (the "Retry" / "Re-run" path).

import i18n from '../i18n';
import type { AnalysisJob, AnalysisJobInput } from '../types/api';

/** Compact "just now" / "Xm ago" / "Xh ago" / "Xd ago" from an epoch-millis timestamp, empty for a
 *  missing one. Reuses the shared `format:relative.*` keys (resolved at call time via the global
 *  i18n instance — not at module load — so it follows the active language, like lib/format.ts). */
export function relTime(ms: number | null | undefined): string {
  if (!ms) return '';
  const d = Date.now() - ms;
  if (d < 60_000) return i18n.t('format:relative.justNow');
  if (d < 3_600_000) return i18n.t('format:relative.min', { count: Math.floor(d / 60_000) });
  if (d < 86_400_000) return i18n.t('format:relative.hour', { count: Math.floor(d / 3_600_000) });
  return i18n.t('format:relative.day', { count: Math.floor(d / 86_400_000) });
}

/** Read a numeric param from a job's params blob, with a fallback. */
function num(params: Record<string, unknown>, key: string, fallback: number): number {
  const v = params[key];
  return typeof v === 'number' ? v : fallback;
}

function str(params: Record<string, unknown>, key: string, fallback: string): string {
  const v = params[key];
  return typeof v === 'string' ? v : fallback;
}

/** Rebuild the launch request from a finished/failed job so it can be re-run with the same config. */
export function inputFromJob(job: AnalysisJob): AnalysisJobInput {
  return {
    tool: job.tool,
    scope_kind: job.scope_kind,
    scope_id: job.scope_id,
    scope_label: job.scope_label,
    window_secs: num(job.params, 'window_secs', 24 * 3600),
    baseline_secs: num(job.params, 'baseline_secs', 14 * 86_400),
    sensitivity: num(job.params, 'sensitivity', 3.0),
    depth: str(job.params, 'depth', 'standard'),
    family: str(job.params, 'family', 'all'),
    notify: typeof job.params.notify === 'boolean' ? job.params.notify : true,
  };
}
