// SPDX-License-Identifier: AGPL-3.0-only
// Small Troubleshoot display helpers: relative timestamps and turning a stored job's params
// back into a re-run request (the "Retry" / "Re-run" path).

import i18n from '../i18n';
import type { AnalysisJob, AnalysisJobInput } from '../types/api';
import { BASELINE_SECS, DEFAULT_WINDOW_SECS } from './analysisDefaults';

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

/** A job's `params` is typed `unknown` because the contract describes it as an untyped object,
 *  not because its content is arbitrary: the backend stores what `JobParams::to_json` builds from
 *  the *validated* request — `window_secs`, `baseline_secs`, `sensitivity`, `depth`, `family`,
 *  `notify` — and reads those six keys back to replay a scheduled run. The launcher's own body
 *  never reaches the column. Every field is still checked before use, because a row written by an
 *  older build may be missing one; a blob of another shape yields the defaults. */
function paramFields(params: unknown): Record<string, unknown> {
  return typeof params === 'object' && params !== null ? (params as Record<string, unknown>) : {};
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

/** Rebuild the launch request from a finished/failed job so it can be re-run with the same config.
 *  A row that recorded no window (or one of the wrong type) re-runs over the same default window the
 *  launchers use — it used to fall back to 24 hours while every launcher defaulted to 7 days. */
export function inputFromJob(job: AnalysisJob): AnalysisJobInput {
  const params = paramFields(job.params);
  return {
    tool: job.tool,
    scope_kind: job.scope_kind,
    scope_id: job.scope_id,
    scope_label: job.scope_label,
    window_secs: num(params, 'window_secs', DEFAULT_WINDOW_SECS),
    baseline_secs: num(params, 'baseline_secs', BASELINE_SECS),
    sensitivity: num(params, 'sensitivity', 3.0),
    depth: str(params, 'depth', 'standard'),
    family: str(params, 'family', 'all'),
    notify: typeof params.notify === 'boolean' ? params.notify : true,
  };
}
