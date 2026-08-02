// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { REPORT_RUN_STATES } from '../types/api';
import { RUN_STATUS, isRunInFlight, isRunState } from './runStatus';

describe('report run status', () => {
  it('covers every run state', () => {
    // The Record types already force this at compile time; asserting it at runtime is what catches
    // the union and the generated schema drifting apart after a regeneration.
    expect(Object.keys(RUN_STATUS).sort()).toEqual([...REPORT_RUN_STATES].sort());
  });

  it('never shows an unrecognised state as a failure', () => {
    // The bug this module exists to fix: a `default:` arm painted anything unknown critical-red,
    // so a run that succeeded on a newer core read as broken.
    expect(RUN_STATUS.unknown.tone).not.toBe('critical');
    expect(RUN_STATUS.failed.tone).toBe('critical');
  });

  it('treats queued and running as in flight, and nothing else', () => {
    expect(REPORT_RUN_STATES.filter(isRunInFlight)).toEqual(['queued', 'running']);
  });

  it('narrows only real run states', () => {
    for (const s of REPORT_RUN_STATES) expect(isRunState(s)).toBe(true);
    expect(isRunState('cancelled')).toBe(false);
    expect(isRunState(undefined)).toBe(false);
    expect(isRunState(3)).toBe(false);
  });

  it('marks exactly the state whose label needs a percentage', () => {
    expect(REPORT_RUN_STATES.filter((s) => RUN_STATUS[s].showsPct)).toEqual(['running']);
  });
});
