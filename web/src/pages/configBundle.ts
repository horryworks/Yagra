// SPDX-License-Identifier: AGPL-3.0-only
// Judgement for Settings ▸ Configuration bundle (ADR-040 decision 3), kept out of the `.tsx` so it
// can be tested — Vitest runs `include: ['src/**/*.test.ts']`, so a test written next to a component
// is a file nothing executes (testing.md).
//
// What lives here: reading a file the operator picked and deciding whether it is a bundle at all,
// summarizing what a bundle contains before anything is sent, and totalling an import report. The
// server re-checks every one of these and is authoritative; doing it here is so the operator finds
// out from the file picker rather than from a 400 after uploading several megabytes.

import type { ConfigBundle, ImportReport } from '../types/api';

/** The document marker every bundle carries. Mirrors `config_bundle::BUNDLE_FORMAT`. */
export const BUNDLE_FORMAT = 'yagra.config-bundle';

/** The newest bundle schema this build reads. Mirrors `config_bundle::BUNDLE_VERSION`. */
export const BUNDLE_VERSION = 1;

/** The carried tables, in the order the import walks them.
 *
 *  `satisfies` is what keeps this honest: every entry must be a key of the generated `ConfigBundle`
 *  type, which comes from the Rust struct — so a renamed or removed section is a compile error
 *  here, not a blank row in the UI. The reverse direction (a table added in Rust and not listed
 *  here) is not a compile error, which is why the label lookup falls back to the raw table name
 *  rather than rendering a missing i18n key. */
export const BUNDLE_TABLES = [
  'profiles',
  'collection_templates',
  'collection_template_items',
  'profile_collection_templates',
  'classification_rules',
  'node_groups',
  'nodes',
  'thresholds',
  'url_checks',
  'dns_checks',
  'forward_destinations',
  'event_sources',
  'event_rules',
  'report_definitions',
  'report_schedules',
  'analysis_schedules',
] as const satisfies readonly (keyof ConfigBundle)[];

/** A carried table's name. */
export type BundleTable = (typeof BUNDLE_TABLES)[number];

/** Every reason a picked file is refused before it is uploaded. `as const` so the i18n coverage
 *  test can walk it — the page renders the resolved key with no fallback, and that sentence is all
 *  the operator is told about why their file will not import. */
export const BUNDLE_IMPORT_REASONS = ['invalid-json', 'not-a-bundle', 'unsupported-version'] as const;

/** Why a picked file is not usable. */
export type BundleImportReason = (typeof BUNDLE_IMPORT_REASONS)[number];

/** The `system:bundle.import.err.*` leaf for a refusal.
 *
 *  Not simply the reason: `unsupported-version` renders a sentence carrying `{{version}}`, so its
 *  key is `version`. Here rather than as a ternary in the page because a mapping in a `.tsx` cannot
 *  be walked by the i18n coverage test (Vitest never runs one), and this is exactly the branch that
 *  would otherwise let a new reason ship pointing at a key nobody wrote. */
export function bundleImportErrorKey(reason: BundleImportReason): string {
  return reason === 'unsupported-version' ? 'version' : reason;
}

/** Why a file the operator picked is not usable, or the bundle it turned out to be.
 *
 *  The reasons are spelled through `BundleImportReason` rather than re-listed, so the parser and
 *  the list the coverage test walks cannot come apart. */
export type ParsedBundle =
  | { ok: true; bundle: ConfigBundle }
  | { ok: false; reason: Exclude<BundleImportReason, 'unsupported-version'> }
  | { ok: false; reason: 'unsupported-version'; version: number };

/** Read a picked file. Deliberately strict about the two header fields and indifferent to
 *  everything else: an older bundle simply lacks sections, which the server treats as empty. */
export function parseBundleFile(text: string): ParsedBundle {
  let doc: unknown;
  try {
    doc = JSON.parse(text);
  } catch {
    return { ok: false, reason: 'invalid-json' };
  }
  if (!doc || typeof doc !== 'object' || Array.isArray(doc)) {
    return { ok: false, reason: 'not-a-bundle' };
  }
  const rec = doc as Record<string, unknown>;
  if (rec.format !== BUNDLE_FORMAT) return { ok: false, reason: 'not-a-bundle' };
  const version = typeof rec.version === 'number' ? rec.version : NaN;
  if (!Number.isInteger(version)) return { ok: false, reason: 'not-a-bundle' };
  if (version > BUNDLE_VERSION) return { ok: false, reason: 'unsupported-version', version };
  return { ok: true, bundle: doc as ConfigBundle };
}

/** How many rows each section carries. Sections with nothing in them are dropped — a list of
 *  sixteen zeros tells the operator less than the three lines that actually have content. */
export function bundleSections(bundle: ConfigBundle): { table: BundleTable; count: number }[] {
  return BUNDLE_TABLES.map((table) => ({
    table,
    count: Array.isArray(bundle[table]) ? bundle[table].length : 0,
  })).filter((s) => s.count > 0);
}

/** Total rows a bundle carries, across every section. */
export function bundleRowCount(bundle: ConfigBundle): number {
  return bundleSections(bundle).reduce((n, s) => n + s.count, 0);
}

/** A stable, sortable filename for a downloaded bundle. `isoOrEmpty` is the bundle's own
 *  `exported_at`; anything unparseable falls back to an undated name rather than to today's date,
 *  which would date the file wrong. */
export function bundleFilename(isoOrEmpty: string | undefined): string {
  const day = (isoOrEmpty ?? '').slice(0, 10);
  return /^\d{4}-\d{2}-\d{2}$/.test(day)
    ? `yagra-config-bundle-${day}.json`
    : 'yagra-config-bundle.json';
}

/** Import totals across every table. */
export function reportTotals(report: ImportReport): {
  created: number;
  updated: number;
  skipped: number;
} {
  return report.tables.reduce(
    (acc, t) => ({
      created: acc.created + t.created,
      updated: acc.updated + t.updated,
      skipped: acc.skipped + t.skipped,
    }),
    { created: 0, updated: 0, skipped: 0 },
  );
}

/** Whether an import changed anything at all. A report of all zeros is a real outcome — importing
 *  a bundle twice is a no-op — and it must read as "nothing to do", never as success or failure. */
export function reportIsEmpty(report: ImportReport): boolean {
  const t = reportTotals(report);
  return t.created === 0 && t.updated === 0;
}
