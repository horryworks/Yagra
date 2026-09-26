// SPDX-License-Identifier: AGPL-3.0-only
// Detect-then-Monitor for unregistered endpoints (ADR-179 増分 2/3): the per-row state and the two
// requests behind the "Monitoring setup" cell, shared by Discovery ▸ Unregistered devices and the
// Node ▸ Neighbors tab so the two cannot drift apart.
//
// A hook, not a component: each surface lays the cell out itself (a grid track on one, an opened
// row on the other). The judgement it calls — what a scan answered, what fills the dropdowns, what
// name to import under — lives in `pages/discoveredEndpoints.ts`, where Vitest reaches it.

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import type { CredentialSummary, ProfileSummary } from '../types/api';
import {
  detectedDevice,
  detectedSelection,
  detectResultOf,
  importNameAfterDetect,
  type DetectResult,
} from '../pages/discoveredEndpoints';
import { MAX_POLL_FAILURES, POLL_INTERVAL_MS } from '../pages/discoveryScans';

/** The row a setup acts on: an unregistered endpoint's id, its address, and the name a neighbour or
 *  a syslog header gave it, if any. */
export interface SetupTarget {
  id: string;
  ip: string;
  name?: string | null;
}

/** The two dropdowns of one row. Empty strings are "(none)". */
export interface SetupSelection {
  profile_id: string;
  credential_id: string;
}

const EMPTY: SetupSelection = { profile_id: '', credential_id: '' };

/** How many reads one Detect may take before it reads as lost. Bounded: a probe no poller ever
 *  takes (a pool whose pollers are all gone) must not spin forever. At the 2 s spacing this is five
 *  minutes, which covers a long credential list at the sweep's own pacing. */
const MAX_DETECT_READS = 150;

export interface EndpointSetup {
  selection: (id: string) => SetupSelection;
  choose: (id: string, patch: Partial<SetupSelection>) => void;
  /** A Detect in flight, or what it found. Screen state only, like a range scan's results. */
  detect: Record<string, 'running' | DetectResult>;
  detectOne: (target: SetupTarget) => Promise<void>;
  /** The sentence over the dropdowns once a Detect has answered, or `null` before. */
  detectLine: (id: string) => string | null;
  /** Import the row with what the dropdowns hold. Resolves `true` once the node exists. */
  monitor: (target: SetupTarget) => Promise<boolean>;
  /** The row an import is running for; every control is disabled meanwhile. */
  busyId: string | null;
  error: string | null;
  clearError: () => void;
}

export function useEndpointSetup({
  profiles,
  creds,
  probeCredIds,
}: {
  profiles: ProfileSummary[];
  creds: CredentialSummary[];
  /** The credentials Detect tries, in order. */
  probeCredIds: string[];
}): EndpointSetup {
  const { t } = useTranslation('monitoring');
  const [rows, setRows] = useState<Record<string, SetupSelection>>({});
  const [detect, setDetect] = useState<Record<string, 'running' | DetectResult>>({});
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Whether the surface is still mounted: a probe's poll loop outlives the click that started it
  // and must not write state into a screen the operator has navigated away from.
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const selection = useCallback((id: string) => rows[id] ?? EMPTY, [rows]);
  const choose = useCallback((id: string, patch: Partial<SetupSelection>) => {
    setRows((cur) => ({ ...cur, [id]: { ...(cur[id] ?? EMPTY), ...patch } }));
  }, []);

  // A one-address scan through the observer's pool, polled until it finishes, then the two
  // dropdowns are filled from what answered. Nothing is imported — the operator reads the result
  // and presses Monitor. One read outstanding at a time, like the Scan tab's own poll.
  const detectOne = async (target: SetupTarget) => {
    setDetect((cur) => ({ ...cur, [target.id]: 'running' }));
    setError(null);
    const finish = (r: DetectResult) => {
      if (!alive.current) return;
      setDetect((cur) => ({ ...cur, [target.id]: r }));
      if (r.kind === 'found') {
        setRows((cur) => ({
          ...cur,
          [target.id]: detectedSelection(
            r,
            profiles.map((p) => p.id),
            creds.map((c) => c.id),
          ),
        }));
      }
    };
    try {
      const { scan_id } = await api.probeDiscoveredEndpoint(target.id, probeCredIds);
      let failures = 0;
      for (let i = 0; i < MAX_DETECT_READS && alive.current; i++) {
        await new Promise((res) => setTimeout(res, POLL_INTERVAL_MS));
        if (!alive.current) return;
        try {
          const r = detectResultOf(await api.getDiscoveryScan(scan_id), target.ip);
          failures = 0;
          if (r) return finish(r);
        } catch {
          failures += 1;
          if (failures >= MAX_POLL_FAILURES) break;
        }
      }
      finish({ kind: 'lost' });
    } catch (err: unknown) {
      if (!alive.current) return;
      setDetect((cur) => {
        const next = { ...cur };
        delete next[target.id];
        return next;
      });
      setError(errMsg(err, t('discovery.seen.detect.err')));
    }
  };

  const detectLine = (id: string): string | null => {
    const r = detect[id];
    if (r === undefined || r === 'running') return null;
    if (r.kind === 'silent') return t('discovery.seen.detect.silent');
    if (r.kind === 'lost') return t('discovery.seen.detect.lost');
    const credential = creds.find((c) => c.id === r.credentialId)?.name ?? r.credentialId;
    const device = detectedDevice(r);
    return device
      ? t('discovery.seen.detect.found', { device, credential })
      : t('discovery.seen.detect.foundBare', { credential });
  };

  const monitor = async (target: SetupTarget): Promise<boolean> => {
    const r = rows[target.id] ?? EMPTY;
    const d = detect[target.id];
    const result = d !== undefined && d !== 'running' ? d : undefined;
    setBusyId(target.id);
    setError(null);
    try {
      await api.importDiscoveredEndpoint(target.id, {
        // The name a neighbour or a syslog header gave it, else the sysName a Detect read; without
        // either the backend uses the address.
        name: importNameAfterDetect(target, result),
        profile_id: r.profile_id || undefined,
        credential_id: r.credential_id || undefined,
        vendor: result?.kind === 'found' ? result.vendor : undefined,
        model: result?.kind === 'found' ? result.model : undefined,
      });
      return true;
    } catch (err: unknown) {
      if (alive.current) setError(errMsg(err, t('discovery.seen.err.import')));
      return false;
    } finally {
      if (alive.current) setBusyId(null);
    }
  };

  return {
    selection,
    choose,
    detect,
    detectOne,
    detectLine,
    monitor,
    busyId,
    error,
    clearError: () => setError(null),
  };
}
