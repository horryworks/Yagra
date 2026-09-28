// SPDX-License-Identifier: AGPL-3.0-only
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { ApiError } from '../services/api';
import { codeOnly, readSources, sourceFiles } from '../testSupport/sources';
import {
  classifyLoadError,
  initialLoadState,
  LOAD_BLOCKS,
  loadReducer,
  type LoadMachine,
} from './loadState';

describe('classifyLoadError', () => {
  it('names skeleton mode', () => {
    expect(classifyLoadError(new ApiError('admin_unavailable', 'no admin state', 503))).toBe(
      'unavailable',
    );
  });

  it('names a permission refusal, by status and by code', () => {
    // The guard extractors return `ApiError::forbidden()` — code `forbidden`, status 403.
    expect(classifyLoadError(new ApiError('forbidden', 'your role does not permit this', 403))).toBe(
      'forbidden',
    );
    // `ApiError::forbidden_code` keeps the status and changes the code. Still a refusal.
    expect(classifyLoadError(new ApiError('session_required', 'tokens cannot use this', 403))).toBe(
      'forbidden',
    );
  });

  it('is silent on 401, because the app handles it globally', () => {
    // The regression this pins: a page that drew "you lack permission" on the way to the sign-in
    // screen. 401 is "who are you", not "you may not".
    expect(classifyLoadError(new ApiError('unauthorized', 'sign in', 401))).toBeNull();
  });

  it('is silent on the errors a screen should not editorialize about', () => {
    expect(classifyLoadError(new ApiError('internal', 'boom', 500))).toBeNull();
    expect(classifyLoadError(new ApiError('not_found', 'gone', 404))).toBeNull();
    expect(classifyLoadError(new TypeError('network down'))).toBeNull();
    expect(classifyLoadError('a string nobody should throw')).toBeNull();
    expect(classifyLoadError(undefined)).toBeNull();
  });

  it('returns only declared blocks', () => {
    const seen = [
      classifyLoadError(new ApiError('admin_unavailable', '', 503)),
      classifyLoadError(new ApiError('forbidden', '', 403)),
    ];
    for (const s of seen) expect(LOAD_BLOCKS).toContain(s);
  });
});

/**
 * ADR-056's guard. Fourteen pages held a byte-identical `.catch` that handled `admin_unavailable`
 * and dropped the `403`, so a Viewer was told "No credentials yet" about two credentials. The
 * classifier above removes the duplication; this stops the next page from reintroducing it.
 *
 * ⚠️ **The needle is assembled at runtime.** A literal would match this file's own source and the
 * test would fail forever — the trap `reports/guards.rs::the_run_state_sql_is_built_from_the_enum` records
 * on the Rust side.
 */
describe('no screen classifies a load failure by hand', () => {
  const SRC = join(__dirname, '..');
  const NEEDLE = `'${'admin'}_${'unavailable'}'`;

  const tsxFiles = (dir: string) => sourceFiles(dir, { exts: ['.tsx'], includeTests: true });

  it('every screen that inspects the skeleton-mode code goes through classifyLoadError', () => {
    const offenders = tsxFiles(SRC)
      .map((p) => [p, readFileSync(p, 'utf8')] as const)
      .filter(([, src]) => src.includes(NEEDLE))
      .filter(([, src]) => !src.includes('classifyLoadError'))
      .map(([p]) => p.slice(SRC.length + 1).replace(/\\/g, '/'));

    expect(
      offenders,
      `these screens decide for themselves what a failed load means, so a 403 lands as an empty ` +
        `list (ADR-056). Use classifyLoadError + LoadBlockNotice instead:\n  ${offenders.join('\n  ')}`,
    ).toEqual([]);
  });

  it('finds the sources it is supposed to be reading', () => {
    // Without this, a broken path or a renamed directory turns the check above into a test that
    // scans nothing and passes — the failure mode that makes a guard worse than no guard.
    //
    // ⚠️ Counts screens that load through the classifier **or** through `useLoad`, which calls it
    // for them. Counting only the first would go red as screens moved onto the shared loader —
    // the work succeeding would read as the guard failing (ADR-184).
    const all = tsxFiles(SRC);
    expect(all.length).toBeGreaterThan(100);
    const loaders = all.filter((p) => {
      const src = readFileSync(p, 'utf8');
      return src.includes('classifyLoadError') || src.includes(`${'useLoad'}(`);
    });
    expect(loaders.length).toBeGreaterThanOrEqual(27);
  });
});

describe('loadReducer', () => {
  const start = (): LoadMachine<string[]> => initialLoadState<string[]>([]);

  it('settles loading on the first answer and keeps it off', () => {
    const a = loadReducer(start(), { type: 'loaded', seq: 1, data: ['x'] });
    expect(a).toMatchObject({ data: ['x'], loading: false, block: null, error: null });
    const b = loadReducer(a, { type: 'failed', seq: 2, error: new Error('down') });
    expect(b.loading).toBe(false);
  });

  it('the request asked last wins, whichever answer arrives last', () => {
    // Asked 1 then 2; 2 answers first. The late answer to 1 must not put its rows back.
    const two = loadReducer(start(), { type: 'loaded', seq: 2, data: ['new'] });
    const late = loadReducer(two, { type: 'loaded', seq: 1, data: ['old'] });
    expect(late.data).toEqual(['new']);
    // A late failure is dropped the same way — it does not raise a notice over a good answer.
    const lateFail = loadReducer(two, {
      type: 'failed',
      seq: 1,
      error: new ApiError('forbidden', '', 403),
    });
    expect(lateFail.block).toBeNull();
  });

  it('keeps the last answer through a failed re-read', () => {
    const a = loadReducer(start(), { type: 'loaded', seq: 1, data: ['x'] });
    const b = loadReducer(a, { type: 'failed', seq: 2, error: new ApiError('internal', '', 500) });
    expect(b.data).toEqual(['x']);
  });

  it('decides the notice again at every settle', () => {
    const refused = loadReducer(start(), {
      type: 'failed',
      seq: 1,
      error: new ApiError('forbidden', '', 403),
    });
    expect(refused.block).toBe('forbidden');
    const allowed = loadReducer(refused, { type: 'loaded', seq: 2, data: [] });
    expect(allowed.block).toBeNull();
    const skeleton = loadReducer(allowed, {
      type: 'failed',
      seq: 3,
      error: new ApiError('admin_unavailable', '', 503),
    });
    expect(skeleton.block).toBe('unavailable');
  });

  it('turns a failure into text only when asked, and never a block into text', () => {
    const quiet = loadReducer(start(), { type: 'failed', seq: 1, error: new Error('down') });
    expect(quiet.error).toBeNull();
    const told = loadReducer(start(), {
      type: 'failed',
      seq: 1,
      error: new ApiError('internal', 'store unreachable', 500),
      fallback: 'Could not load',
    });
    expect(told.error).toBe('store unreachable');
    const refused = loadReducer(start(), {
      type: 'failed',
      seq: 1,
      error: new ApiError('forbidden', 'no', 403),
      fallback: 'Could not load',
    });
    expect(refused).toMatchObject({ block: 'forbidden', error: null });
  });

  it('a skipped read settles loading and drops what was in flight', () => {
    const skipped = loadReducer(start(), { type: 'skipped', seq: 2 });
    expect(skipped.loading).toBe(false);
    expect(loadReducer(skipped, { type: 'loaded', seq: 1, data: ['x'] }).data).toEqual([]);
  });
});

/**
 * ADR-184 increment 24: a screen reads through `useLoad`, which classifies for it. The guard above
 * (`admin_unavailable` by hand) stops the oldest copy; this one stops the next — a screen that
 * calls `classifyLoadError` itself is a screen writing its own `.catch` again.
 *
 * ⚠️ The needle is assembled at runtime, for the reason the guard above gives.
 */
describe('a screen reads through useLoad', () => {
  const NEEDLE = `${'classifyLoadError'}(`;

  /** Where classifying by hand is the design, not a leftover. */
  const PERMANENT: Record<string, string> = {
    'lib/loadState.ts': 'the classifier, and the reducer that calls it for useLoad',
    'pages/AuditPage.tsx':
      'keyset paging: the first page and "load older" share one cursor, which useLoad does not model',
    'pages/UpgradePage.tsx': 'a status poll with its own everSeen/stale state and a ceiling',
    'pages/RelocationPage.tsx': 'a status poll with its own everSeen/stale state and a ceiling',
    'pages/TlsSettingsPage.tsx': 'reads one document into a form, not a list',
    'pages/AiSettingsPage.tsx': 'reads one document into a form, not a list',
    'pages/integrations/IntegrationsCatalogPage.tsx': 'N independent checks, one per card',
    'pages/integrations/MerakiOrgPage.tsx':
      'chained reads, where "not found" is an answer the screen draws rather than a failure',
  };

  /** Screens still to move. Only ever shorter; deleted when empty (increment 27). */
  const NOT_YET_MIGRATED: string[] = [
    'pages/ApiTokensPage.tsx',
    'pages/DuplicateNodesPage.tsx',
    'pages/ForwardingPage.tsx',
    'pages/MaintenancePage.tsx',
    'pages/MibRepositoryPage.tsx',
    'pages/MutesPage.tsx',
    'pages/PollersPage.tsx',
    'pages/ProfilesPage.tsx',
    'pages/ReclassifyPage.tsx',
    'pages/RoutingPage.tsx',
    'pages/ThresholdsPage.tsx',
    'pages/integrations/MerakiIntegrationPage.tsx',
  ];
  /** The ratchet: lowered by each batch, never raised. */
  const CEILING = 12;

  const callers = readSources()
    .filter(([, src]) => codeOnly(src).includes(NEEDLE))
    .map(([p]) => p);

  it('no other file classifies a load failure itself', () => {
    const allowed = new Set([...Object.keys(PERMANENT), ...NOT_YET_MIGRATED]);
    const offenders = callers.filter((p) => !allowed.has(p));
    expect(
      offenders,
      `these files call classifyLoadError themselves. Read through useLoad (lib/useLoad.ts) and ` +
        `draw the notice with LoadGate:\n  ${offenders.join('\n  ')}`,
    ).toEqual([]);
  });

  it('every listed file still does, so the lists cannot go stale', () => {
    const stale = [...Object.keys(PERMANENT), ...NOT_YET_MIGRATED].filter(
      (p) => !callers.includes(p),
    );
    expect(stale, 'listed, but no longer classifies by hand — take it off the list').toEqual([]);
  });

  it('the migration list only shrinks', () => {
    expect(NOT_YET_MIGRATED.length).toBeLessThanOrEqual(CEILING);
    expect(new Set(NOT_YET_MIGRATED).size).toBe(NOT_YET_MIGRATED.length);
  });

  it('finds the callers it is supposed to be reading', () => {
    // The two lists together are exactly what the walk must see; this is the floor that tells a
    // walk that found nothing from a tree with nothing to find.
    expect(callers.length).toBeGreaterThanOrEqual(Object.keys(PERMANENT).length);
  });
});
