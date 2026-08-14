// SPDX-License-Identifier: AGPL-3.0-only
// A consistency test, not a browser test — it needs no page and costs milliseconds. It lives here
// rather than under `src/` because its subject is the walk's data, and Vitest only collects
// `src/**/*.test.ts`; putting it there would separate it from the file it pins.

import { expect, test } from '@playwright/test';
import { ALL_SCREENS, SCREEN_EXPECT } from './screens';
import { auditFixtures, continuationProps, OPERATION_COUNT } from '../support/openapi';

test('every walked screen declares what "rendered" means for it', () => {
  const walked = ALL_SCREENS.map((s) => s.path).sort();
  const declared = Object.keys(SCREEN_EXPECT).sort();

  // Named both ways so the failure says which screen, not just that the counts differ.
  expect(
    walked.filter((p) => !declared.includes(p)),
    'screens the walk visits with no entry in SCREEN_EXPECT (add one line)',
  ).toEqual([]);
  expect(
    declared.filter((p) => !walked.includes(p)),
    'SCREEN_EXPECT entries for screens the walk no longer visits (delete them)',
  ).toEqual([]);
});

test('the walk covers every nav item', () => {
  // Guards against the derivation silently collapsing — if `sectionItems` or the dedupe broke,
  // the walk would still pass with two screens in it. Asserting a floor, not the exact number,
  // because the exact number is the thing that must be free to change.
  expect(ALL_SCREENS.length).toBeGreaterThanOrEqual(40);
});

test('no new pagination cursor has appeared that the mock would answer non-null', () => {
  // A nullable "there is more" field answered with a value makes a correct client page until its
  // own safety cap: 405 requests for one page load, and a map claiming 200 nodes when the mock had
  // served one. Both spellings below are handled in `TERMINAL_NULL_PROPS`; a third would not be.
  //
  // This test earned its place on its first run — it was written asserting `['next_cursor']` and
  // immediately failed, naming `next` (the keyset cursor on DiscoveredEndpointPage,
  // DnsChainHistory and NeighborHistory). The list it guards had been wrong from the moment it
  // was written, and no screen in the walk happened to expose it.
  expect(continuationProps()).toEqual(['next', 'next_cursor']);
});

test('the generated fixtures are valid against the contract that generated them', () => {
  // A null in a required field is a generator bug that reads as a product bug: the request
  // succeeds, the shape looks right, and the screen blanks on the first property access. The DNS
  // chain did exactly that — `record.kind` came back null because the instance builder ran out of
  // depth mid-schema — and the symptom was a white node-detail page for every DNS monitor.
  expect(auditFixtures().invalid).toEqual([]);
});

test('the contract still declines to describe exactly these fields', () => {
  // Not a mock problem: `{}` is what utoipa emits for a `serde_json::Value`, so the document
  // promises nothing and the generated TypeScript types them `unknown` too. They are pinned
  // rather than filtered out because the list only grows by someone putting another untyped
  // value on the public API, and that is worth having to acknowledge (ADR-035's blind spot).
  expect(auditFixtures().undescribed).toEqual([
    'GET /api/v1/analysis/jobs/{id}.params',
    'GET /api/v1/analysis/jobs/{id}/findings[0].detail',
    'GET /api/v1/analysis/jobs[0].params',
    'GET /api/v1/analysis/schedules[0].params',
    'GET /api/v1/config/bundle.analysis_schedules[0].params',
    'GET /api/v1/config/bundle.forward_destinations[0].filter',
    'GET /api/v1/config/bundle.nodes[0].tags',
    'GET /api/v1/config/bundle.report_definitions[0].spec',
    'GET /api/v1/config/bundle.url_checks[0].expected_status',
    'GET /api/v1/reports/definitions[0].spec',
    'GET /api/v1/reports/sections[0].settings[0].default',
    'POST /api/v1/analysis/jobs.params',
    'POST /api/v1/reports/definitions.spec',
  ]);
});

test('the OpenAPI document still yields a route table', () => {
  // The mock is only as real as its source. A document that failed to parse, or a shape change
  // that emptied `paths`, would otherwise show up as 46 screens quietly answering 404 to
  // everything — which the unmatched check would catch, but far less legibly.
  expect(OPERATION_COUNT).toBeGreaterThan(200);
});
