// Pure helpers for the Pollers page (Settings ▸ Pollers). Kept out of the component so the pool
// warning, the working-set summary, id/pool validation, and the remote-poller config snippet are
// all unit-testable without the DOM.

import type { TFunction } from 'i18next';
import type { PoolSummary } from '../types/api';
import { relativeTime } from './format';

/** A pool has nodes but no live poller ⇒ those nodes go unmonitored (ADR-009 operational rule). */
export function poolHasWarning(pool: PoolSummary): boolean {
  return pool.warning === 'nodes_without_live_poller';
}

/** Human label for a pool's dispatch mode. `t` is threaded from the caller so the label follows the
 *  active language (this module must never resolve translations at load time). Keys are namespace-
 *  qualified so any bound `t` resolves them. */
export function poolModeLabel(mode: PoolSummary['mode'], t: TFunction): string {
  return mode === 'working_set'
    ? t('system:pollers.mode.workingSet')
    : t('system:pollers.mode.legacy');
}

/** Compact working-set summary ("5 nodes / 9 specs"), singularizing 1 via the count-pluralized
 *  keys. An offline poller reports zeroes, so callers pass `online: false` to render an em dash
 *  instead of "0 nodes / 0 specs". */
export function workingSetLabel(
  nodes: number,
  specs: number,
  online: boolean,
  t: TFunction,
): string {
  if (!online) return '—';
  return `${t('system:pollers.workingSet.nodes', { count: nodes })} / ${t('system:pollers.workingSet.specs', { count: specs })}`;
}

/** "Last seen" label: relative time when we have a durable timestamp; otherwise "Live" for a poller
 *  that is online but not yet persisted (within the 60s inventory throttle), or an em dash. */
export function lastSeenLabel(
  iso: string | null,
  online: boolean,
  t: TFunction,
  now: number = Date.now(),
): string {
  if (iso) return relativeTime(iso, now);
  return online ? t('system:pollers.lastSeen.live') : '—';
}

/** Poller id / pool naming rule — matches the server-side sanitizer's accepted alphabet
 *  (`[A-Za-z0-9_-]`). Empty is invalid. */
export function isValidPollerToken(value: string): boolean {
  return /^[A-Za-z0-9_-]+$/.test(value);
}

/** Fields the operator fills in the Register-poller dialog. */
export interface PollerEnvInput {
  id: string;
  pool: string;
  busUrl: string;
  /** Optional path to a CA bundle for the bus TLS cert; omitted from the snippet when blank. */
  caFile?: string;
}

/** Generate the `.env` snippet for `docker-compose.poller.yml`. Client-side only — no secrets are
 *  invented here; the operator supplies the (already-credentialed) bus URL. The CA line is emitted
 *  only when a path is given. */
export function buildPollerEnv({ id, pool, busUrl, caFile }: PollerEnvInput): string {
  const lines = [
    `YAGRA_POLLER_ID=${id}`,
    `YAGRA_POLLER_POOL=${pool}`,
    `YAGRA_BUS_URL=${busUrl}`,
  ];
  if (caFile && caFile.trim() !== '') {
    lines.push(`YAGRA_BUS_CA_FILE=${caFile.trim()}`);
  }
  return lines.join('\n');
}

/** The command that brings the remote poller up once its `.env` is in place. */
export const POLLER_UP_COMMAND = 'docker compose -f docker-compose.poller.yml up -d';
