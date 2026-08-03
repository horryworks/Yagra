// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { ConfigBundle, ImportReport } from '../types/api';
import {
  BUNDLE_FORMAT,
  BUNDLE_TABLES,
  BUNDLE_VERSION,
  bundleFilename,
  bundleRowCount,
  bundleSections,
  parseBundleFile,
  reportIsEmpty,
  reportTotals,
} from './configBundle';

function bundle(extra: Partial<ConfigBundle> = {}): ConfigBundle {
  return {
    format: BUNDLE_FORMAT,
    version: BUNDLE_VERSION,
    exported_at: '2026-08-03T04:05:06Z',
    yagra_version: '0.1.20',
    ...extra,
  } as ConfigBundle;
}

describe('parseBundleFile', () => {
  it('accepts a bundle carrying only its header', () => {
    const r = parseBundleFile(JSON.stringify(bundle()));
    expect(r.ok).toBe(true);
  });

  it('rejects text that is not JSON at all', () => {
    // The realistic mistake: the operator picked a PDF or a half-downloaded file.
    const r = parseBundleFile('not json {');
    expect(r).toEqual({ ok: false, reason: 'invalid-json' });
  });

  it('rejects a JSON document that is not a bundle', () => {
    // Valid JSON with no marker — a config file from something else entirely. Refusing here is what
    // keeps an unrelated document from being POSTed at an endpoint that writes sixteen tables.
    expect(parseBundleFile('{"nodes":[]}')).toEqual({ ok: false, reason: 'not-a-bundle' });
    expect(parseBundleFile('[]')).toEqual({ ok: false, reason: 'not-a-bundle' });
    expect(parseBundleFile('null')).toEqual({ ok: false, reason: 'not-a-bundle' });
    expect(parseBundleFile('{"format":"yagra.config-bundle"}')).toEqual({
      ok: false,
      reason: 'not-a-bundle',
    });
  });

  it('names the version when a bundle is newer than this build', () => {
    const r = parseBundleFile(JSON.stringify(bundle({ version: BUNDLE_VERSION + 3 })));
    expect(r).toEqual({ ok: false, reason: 'unsupported-version', version: BUNDLE_VERSION + 3 });
  });

  it('accepts an older bundle rather than treating it as foreign', () => {
    // Forward compatibility runs one way: an older document is missing sections, which the server
    // reads as empty. Refusing it would make every upgrade break existing bundles.
    const older = JSON.parse(JSON.stringify(bundle())) as Record<string, unknown>;
    older.version = 0;
    const r = parseBundleFile(JSON.stringify(older));
    expect(r.ok).toBe(true);
  });
});

describe('bundleSections', () => {
  it('lists only the sections that carry something', () => {
    const b = bundle({
      nodes: [{ id: 'n1' }, { id: 'n2' }],
      node_groups: [{ id: 'g1' }],
      thresholds: [],
    } as unknown as Partial<ConfigBundle>);
    expect(bundleSections(b)).toEqual([
      { table: 'node_groups', count: 1 },
      { table: 'nodes', count: 2 },
    ]);
    expect(bundleRowCount(b)).toBe(3);
  });

  it('reads an absent section as empty rather than throwing', () => {
    expect(bundleSections(bundle())).toEqual([]);
    expect(bundleRowCount(bundle())).toBe(0);
  });

  it('keeps the dependency order the importer walks', () => {
    // Sections are shown in the order they are applied, so a skipped reference in a later table is
    // read against the earlier one it depended on.
    expect(BUNDLE_TABLES.indexOf('profiles')).toBeLessThan(BUNDLE_TABLES.indexOf('nodes'));
    expect(BUNDLE_TABLES.indexOf('node_groups')).toBeLessThan(BUNDLE_TABLES.indexOf('nodes'));
    expect(BUNDLE_TABLES.indexOf('nodes')).toBeLessThan(BUNDLE_TABLES.indexOf('url_checks'));
    expect(BUNDLE_TABLES.indexOf('event_sources')).toBeLessThan(
      BUNDLE_TABLES.indexOf('event_rules'),
    );
    expect(BUNDLE_TABLES.indexOf('report_definitions')).toBeLessThan(
      BUNDLE_TABLES.indexOf('report_schedules'),
    );
  });
});

describe('bundleFilename', () => {
  it('dates the file from the bundle itself', () => {
    expect(bundleFilename('2026-08-03T04:05:06Z')).toBe('yagra-config-bundle-2026-08-03.json');
  });

  it('falls back to an undated name rather than to today', () => {
    // Stamping today's date on a bundle exported last month would make the filename a lie, and the
    // filename is how an operator tells two bundles apart.
    for (const bad of [undefined, '', 'yesterday', '03/08/2026']) {
      expect(bundleFilename(bad)).toBe('yagra-config-bundle.json');
    }
  });
});

describe('reportTotals', () => {
  const report = (tables: ImportReport['tables']): ImportReport => ({
    dry_run: false,
    tables,
    notes: [],
  });

  it('sums across tables', () => {
    const r = report([
      { table: 'nodes', created: 4, updated: 1, skipped: 2 },
      { table: 'thresholds', created: 0, updated: 3, skipped: 0 },
    ]);
    expect(reportTotals(r)).toEqual({ created: 4, updated: 4, skipped: 2 });
    expect(reportIsEmpty(r)).toBe(false);
  });

  it('treats a no-op import as empty even when rows were skipped', () => {
    // Re-importing the same bundle writes nothing new. That is a legitimate outcome and must not
    // read as a failure — but a run that only skipped rows has not applied the bundle either.
    const r = report([{ table: 'nodes', created: 0, updated: 0, skipped: 7 }]);
    expect(reportIsEmpty(r)).toBe(true);
    expect(reportTotals(r).skipped).toBe(7);
  });

  it('handles a report with no tables', () => {
    expect(reportTotals(report([]))).toEqual({ created: 0, updated: 0, skipped: 0 });
    expect(reportIsEmpty(report([]))).toBe(true);
  });
});
