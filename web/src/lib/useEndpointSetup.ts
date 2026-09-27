// SPDX-License-Identifier: AGPL-3.0-only
// Detect-then-Monitor for unregistered endpoints (ADR-179 増分 2/3): the per-row state and the two
// requests behind the "Monitoring setup" cell, shared by Discovery ▸ Unregistered devices and the
// Node ▸ Neighbors tab so the two cannot drift apart.
//
// A hook, not a component: each surface lays the cell out itself (a grid track on one, an opened
// row on the other). The judgement it calls — what a scan answered, what fills the dropdowns, what
// name to import under, how long a Detect polls, what sentence it ends in — lives in
// `pages/discoveredEndpoints.ts`, where Vitest reaches it.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import type { CredentialSummary, NodeGroup, ProfileSummary } from '../types/api';
import {
  detectedSelection,
  detectLineOf,
  detectResultOf,
  endpointDestinationLine,
  importNameAfterDetect,
  pollDetect,
  type DetectResult,
  type SetupDestination,
} from '../pages/discoveredEndpoints';
import { mergePreview, type RowDestination } from '../pages/importFiling';
import { groupOptions } from './nodeTree';
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

export interface EndpointSetup {
  selection: (id: string) => SetupSelection;
  choose: (id: string, patch: Partial<SetupSelection>) => void;
  /** A Detect in flight, or what it found. Screen state only, like a range scan's results. */
  detect: Record<string, 'running' | DetectResult>;
  detectOne: (target: SetupTarget) => Promise<void>;
  /** The sentence over the dropdowns once a Detect has answered, or `null` before. */
  detectLine: (id: string) => string | null;
  /** Import the row with what the dropdowns hold, into the surface's destination. Resolves `true`
   *  once the node exists. */
  monitor: (target: SetupTarget) => Promise<boolean>;
  /** Ask the server, once per address, which folder's IP range holds it (ADR-179 増分 8). */
  ensureDestination: (ip: string) => void;
  /** The sentence over Monitor saying where the node will land, or `null` until it is known. */
  destinationLine: (ip: string) => { text: string; warn: boolean } | null;
  /** The row an import is running for; every control is disabled meanwhile. */
  busyId: string | null;
  error: string | null;
  clearError: () => void;
}

export function useEndpointSetup({
  profiles,
  creds,
  probeCredIds,
  destination,
  groups,
}: {
  profiles: ProfileSummary[];
  creds: CredentialSummary[];
  /** The credentials Detect tries, in order. */
  probeCredIds: string[];
  /** Where an import lands (ADR-179 増分 8). */
  destination: SetupDestination;
  /** The folders the caller can see, to name a destination. */
  groups: NodeGroup[];
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

  // Which folder's range holds each address, as the server answered (`importFiling.ts`), and
  // whether any folder has a range at all. Asked once per address; a failed ask says nothing.
  const [destinations, setDestinations] = useState<Map<string, RowDestination>>(new Map());
  const [anyPrefixes, setAnyPrefixes] = useState<boolean | undefined>(undefined);
  const asked = useRef(new Set<string>());
  const ensureDestination = useCallback((ip: string) => {
    if (!ip || asked.current.has(ip)) return;
    asked.current.add(ip);
    api
      .previewDiscoveryImport([ip])
      .then((preview) => {
        if (!alive.current) return;
        setDestinations((cur) => mergePreview(cur, preview));
        setAnyPrefixes(preview.any_prefixes);
      })
      .catch(() => undefined);
  }, []);
  const paths = useMemo(
    () => new Map(groupOptions(groups).map((o) => [o.id, o.path])),
    [groups],
  );
  const destinationLine = (ip: string) => {
    const line = endpointDestinationLine(
      destinations.get(ip),
      anyPrefixes,
      destination,
      (id) => paths.get(id) ?? id,
    );
    if (!line) return null;
    return { text: t(`discovery.seen.dest.line.${line.line}`, line.values), warn: line.warn };
  };

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
      const r = await pollDetect({
        read: async () => detectResultOf(await api.getDiscoveryScan(scan_id), target.ip),
        wait: () => new Promise((res) => setTimeout(res, POLL_INTERVAL_MS)),
        alive: () => alive.current,
        maxFailures: MAX_POLL_FAILURES,
      });
      if (r) finish(r);
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
    const line = detectLineOf(detect[id], creds);
    if (!line) return null;
    return 'values' in line ? t(line.key, line.values) : t(line.key);
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
        group_id: destination.groupId || undefined,
        file_by_prefix: destination.fileByPrefix || undefined,
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
    ensureDestination,
    destinationLine,
    busyId,
    error,
    clearError: () => setError(null),
  };
}
