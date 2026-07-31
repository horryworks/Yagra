// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { REPORT_FREQUENCIES, REPORT_RUN_STATES } from '../types/api';
import { CADENCE, RUN_STATUS, SELECTABLE_FREQUENCIES, isRunInFlight, isRunState } from './runStatus';

describe('report run status', () => {
  it('covers every run state and every cadence', () => {
    // The Record types already force this at compile time; asserting it at runtime is what catches
    // the union and the generated schema drifting apart after a regeneration.
    expect(Object.keys(RUN_STATUS).sort()).toEqual([...REPORT_RUN_STATES].sort());
    expect(Object.keys(CADENCE).sort()).toEqual([...REPORT_FREQUENCIES].sort());
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

  it('offers every cadence except the storage-only one', () => {
    // A deliberate subset: `unknown` means "written by a newer core", so an operator picking it
    // would be asking for a cadence the scheduler silently treats as daily.
    expect([...SELECTABLE_FREQUENCIES]).toEqual(['daily', 'weekly', 'monthly']);
    expect(SELECTABLE_FREQUENCIES).not.toContain('unknown');
    // Pin the subset relation, so a fourth cadence has to choose a side.
    for (const f of SELECTABLE_FREQUENCIES) expect(REPORT_FREQUENCIES).toContain(f);
    expect(SELECTABLE_FREQUENCIES.length).toBe(REPORT_FREQUENCIES.length - 1);
  });

  it('marks exactly the state whose label needs a percentage', () => {
    expect(REPORT_RUN_STATES.filter((s) => RUN_STATUS[s].showsPct)).toEqual(['running']);
  });
});
