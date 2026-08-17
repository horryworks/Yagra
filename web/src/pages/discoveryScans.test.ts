// SPDX-License-Identifier: AGPL-3.0-only
// The Discovery screen's judgement (ADR-068 Increment 1).
//
// ⚠️ What these tests can and cannot reach is worth stating, because the gap is the feature's
// riskiest part: they cover *which* scan the page opens and *whether* it keeps polling. They do not
// cover the effect that reattaches on mount — Vitest runs in `environment: 'node'` and executes no
// `.tsx`, so "navigate away and come back" is verified by hand on a real deployment or not at all.

import { describe, expect, it } from 'vitest';
import type { DiscoveryScan, DiscoveryScanSummary, PoolOption } from '../types/api';
import { DISCOVERY_SCAN_STATES } from '../types/api';
import {
  canRequestStop,
  isScanInFlight,
  MAX_POLL_FAILURES,
  mergeScanIntoList,
  pickDefaultPool,
  poolIsUnrouted,
  SCAN_STATE_SPECS,
  scanState,
  selectInitialScan,
  shouldPollScan,
} from './discoveryScans';

function summary(over: Partial<DiscoveryScanSummary> = {}): DiscoveryScanSummary {
  return {
    scan_id: '00000000-0000-0000-0000-000000000001',
    state: 'running',
    probed: 0,
    total: 254,
    candidate_count: 0,
    started_at: '2026-08-17T00:00:00Z',
    updated_at: '2026-08-17T00:00:00Z',
    pool: null,
    ...over,
  };
}

function status(over: Partial<DiscoveryScan> = {}): DiscoveryScan {
  return {
    scan_id: '00000000-0000-0000-0000-000000000001',
    done: false,
    state: 'running',
    probed: 0,
    total: 254,
    scanning: '192.168.1.1',
    started_at: '2026-08-17T00:00:00Z',
    updated_at: '2026-08-17T00:00:00Z',
    pool: null,
    candidates: [],
    ...over,
  };
}

const pool = (name: string, live: boolean): PoolOption => ({ name, live });

/** The id both fixtures carry — i.e. "the page is looking at the scan it selected", which is
 *  what every test that is not about identity means. */
const SEL = '00000000-0000-0000-0000-000000000001';

describe('scan state', () => {
  it('gives every wire state a spec', () => {
    // The `Record` makes this a compile error too; asserting it keeps the guarantee if the type
    // ever loosens.
    for (const s of DISCOVERY_SCAN_STATES) {
      expect(SCAN_STATE_SPECS[s]).toBeDefined();
    }
  });

  it('renders an unrecognised state neutrally, never as a failure', () => {
    // The report-run bug this exists to prevent: a `default:` arm painted unknown states red, so a
    // word this build had not been taught read as bad news.
    expect(scanState('teleported')).toBe('unknown');
    expect(SCAN_STATE_SPECS.unknown.tone).toBe('neutral');
    expect(SCAN_STATE_SPECS.unknown.tone).not.toBe('critical');
    for (const raw of [null, undefined, '']) {
      expect(scanState(raw)).toBe('unknown');
    }
  });

  it('treats a stop that has not been confirmed as still in flight', () => {
    // `cancelling` means "we asked"; only the poller reporting turns it into `cancelled`. Stopping
    // the poll at the request would freeze the page on the one transition worth watching.
    expect(isScanInFlight('running')).toBe(true);
    expect(isScanInFlight('cancelling')).toBe(true);
    expect(isScanInFlight('cancelled')).toBe(false);
    expect(isScanInFlight('done')).toBe(false);
  });
});

describe('selectInitialScan', () => {
  it('honours the id in the URL even when the list has never heard of it', () => {
    // A reload or a shared link is an explicit request. Returning it lets the caller fetch it and
    // report "this core does not know that scan"; silently opening a different one would answer a
    // question nobody asked.
    const forgotten = 'ffffffff-0000-0000-0000-00000000000f';
    expect(selectInitialScan([summary()], forgotten)).toBe(forgotten);
    expect(selectInitialScan([], forgotten)).toBe(forgotten);
  });

  it('prefers a sweep still running over a newer finished one', () => {
    const rows = [
      summary({ scan_id: 'newest-done', state: 'done' }),
      summary({ scan_id: 'older-running', state: 'running' }),
    ];
    expect(selectInitialScan(rows, null)).toBe('older-running');
  });

  it('falls back to the newest finished scan so returning shows the candidates', () => {
    const rows = [
      summary({ scan_id: 'newest-done', state: 'done' }),
      summary({ scan_id: 'older-done', state: 'done' }),
    ];
    expect(selectInitialScan(rows, null)).toBe('newest-done');
  });

  it('is null only when there is genuinely nothing', () => {
    expect(selectInitialScan([], null)).toBeNull();
  });
});

describe('shouldPollScan', () => {
  it('polls from the act of starting, before the server can describe the scan', () => {
    // The inversion `upgradeStatus.ts` documents: deriving this from what the server currently says
    // means concluding there is nothing to watch during the window in which it cannot answer yet.
    expect(shouldPollScan({ scanId: SEL, status: null, justStarted: true, failures: 0 })).toBe(true);
  });

  it('polls a scan picked up on arrival, which has no status yet and was not started here', () => {
    // 🚨 The regression this file exists for, found on a real deployment rather than here. The
    // reattach path selects a scan id from the list and has never fetched it, so it arrives with
    // `justStarted: false` and `status: null`. Reading that as "nothing to watch" meant a running
    // sweep was never polled: Recent sweeps showed it as `Running` while the progress line, the
    // Stop button and the disabled Scan button — all derived from `status` — never appeared.
    //
    // Both of the original tests passed throughout. Neither covered this combination, because the
    // start path always sets `justStarted` and every other case supplies a status.
    expect(shouldPollScan({ scanId: SEL, status: null, justStarted: false, failures: 0 })).toBe(true);
  });

  it('polls when the status it holds is about some other scan', () => {
    // 🚨 The second real deployment bug, and the subtler face of the one above. Pressing Scan
    // while a finished sweep is on screen starts a poll straight away on `justStarted` — but the id
    // it polls is still the *previous* scan, because the new one does not exist until the server
    // answers. That reply lands first, clears `justStarted` and installs a `done` status, so the new
    // scan id arrives at a page that has already decided there is nothing to watch. The sweep ran to
    // completion with its row frozen at `0/254`; only a reload revealed it had finished.
    //
    // Reattach is what made this the normal path: arriving now selects the newest sweep, so there is
    // almost always a finished scan on screen when Scan is pressed.
    expect(
      shouldPollScan({
        scanId: 'the-new-scan',
        status: status({ scan_id: 'the-previous-scan', state: 'done', done: true }),
        justStarted: false,
        failures: 0,
      }),
    ).toBe(true);
  });

  it('still stops on a terminal status that IS about the selected scan', () => {
    // The other half: the identity check must not turn every terminal state into "keep asking",
    // which would poll every finished sweep forever. A guard that only ever answers `true` is
    // indistinguishable from no guard.
    expect(
      shouldPollScan({
        scanId: 'the-scan',
        status: status({ scan_id: 'the-scan', state: 'done', done: true }),
        justStarted: false,
        failures: 0,
      }),
    ).toBe(false);
  });

  it('keeps polling a scan that has not been declared over', () => {
    expect(shouldPollScan({ scanId: SEL, status: status(), justStarted: false, failures: 0 })).toBe(true);
    expect(
      shouldPollScan({
        scanId: SEL,
        status: status({ state: 'cancelling' }),
        justStarted: false,
        failures: 0,
      }),
    ).toBe(true);
  });

  it('stops once the scan is terminal', () => {
    for (const state of ['done', 'cancelled'] as const) {
      expect(
        shouldPollScan({ scanId: SEL, status: status({ state, done: true }), justStarted: false, failures: 0 }),
      ).toBe(false);
    }
  });

  it('stops on a state it cannot read, rather than waiting forever', () => {
    // The trade both ways: against a newer core this stops following a sweep that is still going,
    // which the operator can see and fix by reloading. The alternative — treating unknown as "still
    // going" — polls until the tab closes, because a build that cannot read a state today will
    // never learn it. The backend picks the same side (`AnalysisJobState::Unknown` is terminal).
    const unknown = { ...status(), state: 'teleported' } as unknown as DiscoveryScan;
    expect(shouldPollScan({ scanId: SEL, status: unknown, justStarted: false, failures: 0 })).toBe(false);
    // …but pressing Scan still starts a poll: that is the operator's own action, not a reading of
    // a state.
    expect(shouldPollScan({ scanId: SEL, status: unknown, justStarted: true, failures: 0 })).toBe(true);
  });

  it('gives up after repeated failures instead of hammering a 404 forever', () => {
    // The pre-ADR-068 behaviour: a scan the core had forgotten left the page polling every two
    // seconds for the life of the tab, with the progress note frozen mid-sentence.
    expect(
      shouldPollScan({ scanId: SEL, status: status(), justStarted: true, failures: MAX_POLL_FAILURES }),
    ).toBe(false);
    expect(
      shouldPollScan({ scanId: SEL, status: status(), justStarted: false, failures: MAX_POLL_FAILURES - 1 }),
    ).toBe(true);
  });
});

describe('mergeScanIntoList', () => {
  it('brings the watched row up to what the poll just returned', () => {
    // 🚨 The defect this exists for, seen on a real deployment: the list is fetched once and only
    // the selected scan is polled, so the row read `Running · 0/254 probed · 0 devices` while the
    // table right below it listed the two devices that sweep had already found.
    const stale = summary({ scan_id: 'a', state: 'running', probed: 0, candidate_count: 0 });
    const fresh = status({
      scan_id: 'a',
      state: 'running',
      probed: 96,
      candidates: [{}, {}] as never,
    });
    const [row] = mergeScanIntoList([stale], fresh);
    expect(row.probed).toBe(96);
    expect(row.candidate_count).toBe(2);
  });

  it('leaves the other rows alone and does not reorder them', () => {
    // The list is newest-first from the server. Moving a row because it reported progress would
    // shuffle the list under the operator while they are reaching for it.
    const rows = [
      summary({ scan_id: 'newest' }),
      summary({ scan_id: 'middle' }),
      summary({ scan_id: 'oldest' }),
    ];
    const merged = mergeScanIntoList(rows, status({ scan_id: 'middle', probed: 7 }));
    expect(merged.map((s) => s.scan_id)).toEqual(['newest', 'middle', 'oldest']);
    expect(merged[1].probed).toBe(7);
    expect(merged[0]).toBe(rows[0]);
    expect(merged[2]).toBe(rows[2]);
  });

  it('prepends a scan the list has never mentioned', () => {
    // The window between pressing Scan and the list fetch landing: the only way to be polling a
    // scan that is not in the list is to have just started it, and the list is newest-first.
    const merged = mergeScanIntoList([summary({ scan_id: 'older' })], status({ scan_id: 'brand-new' }));
    expect(merged.map((s) => s.scan_id)).toEqual(['brand-new', 'older']);
  });

  it('carries a terminal state across, so a finished sweep stops saying Running', () => {
    const merged = mergeScanIntoList(
      [summary({ scan_id: 'a', state: 'running' })],
      status({ scan_id: 'a', state: 'done', done: true, probed: 254 }),
    );
    expect(merged[0].state).toBe('done');
    expect(merged[0].probed).toBe(254);
  });
});

describe('canRequestStop', () => {
  it('offers a stop only while the sweep is running', () => {
    expect(canRequestStop('running')).toBe(true);
    // Already asked — the screen shows "stopping…" instead of a second button.
    expect(canRequestStop('cancelling')).toBe(false);
    for (const s of ['cancelled', 'done'] as const) {
      expect(canRequestStop(s)).toBe(false);
    }
  });

  it('does not offer to stop a state it cannot read', () => {
    // Acting on behalf of a sweep this build cannot reason about is guessing.
    expect(canRequestStop('teleported')).toBe(false);
    expect(canRequestStop(null)).toBe(false);
  });
});

describe('pool selection', () => {
  it('preselects the only live pool so a single-site deployment is not asked', () => {
    expect(pickDefaultPool([pool('default', true)])).toBe('default');
    expect(pickDefaultPool([pool('tokyo', true), pool('osaka', false)])).toBe('tokyo');
  });

  it('preselects default when several sites are live, and nothing when it is not', () => {
    expect(pickDefaultPool([pool('default', true), pool('tokyo', true)])).toBe('default');
    // Several live sites and no `default` among them: guessing which site to sweep from is the
    // guess this control exists to stop.
    expect(pickDefaultPool([pool('tokyo', true), pool('osaka', true)])).toBeNull();
  });

  it('never preselects a pool with no live poller', () => {
    // Choosing one sends the sweep to a route with no listener, which the server redirects to the
    // global subject — the silent behaviour this control was added to make visible.
    expect(pickDefaultPool([pool('default', false)])).toBeNull();
    expect(pickDefaultPool([])).toBeNull();
  });

  it('flags both ways a sweep ends up with an undecided site', () => {
    const pools = [pool('default', true), pool('tokyo', false)];
    expect(poolIsUnrouted(pools, null)).toBe(true);
    expect(poolIsUnrouted(pools, 'tokyo')).toBe(true);
    expect(poolIsUnrouted(pools, 'nonexistent')).toBe(true);
    expect(poolIsUnrouted(pools, 'default')).toBe(false);
  });
});
