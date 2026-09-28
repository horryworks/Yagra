// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { codeOnly, readSources } from '../testSupport/sources';
import {
  done,
  initialSubmitState,
  partialOutcome,
  submitReducer,
  type SubmitEvent,
  type SubmitState,
} from './submitState';

const run = (events: SubmitEvent[], from: SubmitState = initialSubmitState) =>
  events.reduce(submitReducer, from);

describe('submitReducer', () => {
  it('stays busy after a success, so Save cannot come back while the dialog closes (F1)', () => {
    const s = run([{ type: 'start' }, { type: 'outcome', outcome: done() }]);
    expect(s).toEqual({ busy: true, error: null, settled: false });
  });

  it('a failure frees both buttons and says why; the next attempt clears the message', () => {
    const failed = run([{ type: 'start' }, { type: 'fail', message: 'refused' }]);
    expect(failed).toEqual({ busy: false, error: 'refused', settled: false });
    expect(run([{ type: 'start' }], failed)).toEqual({ busy: true, error: null, settled: false });
  });

  it('a partial batch keeps the dialog open and turns Cancel into Close for good (F3)', () => {
    const partial = run([
      { type: 'start' },
      { type: 'outcome', outcome: { kind: 'keepOpen', message: '1 of 3', refresh: false } },
    ]);
    expect(partial).toEqual({ busy: false, error: '1 of 3', settled: true });
    // A retry that then fails outright does not un-land the first write.
    const retried = run([{ type: 'start' }, { type: 'fail', message: 'down' }], partial);
    expect(retried.settled).toBe(true);
  });

  it('a refusal the dialog makes itself sends nothing and says why', () => {
    expect(run([{ type: 'refuse', message: 'name required' }])).toEqual({
      busy: false,
      error: 'name required',
      settled: false,
    });
  });
});

describe('partialOutcome', () => {
  const words = ({ applied, requested }: { applied: number; requested: number }) =>
    `${applied} of ${requested}`;

  it('is done when every row was applied, and names both numbers when not', () => {
    expect(partialOutcome(3, 3, words, true)).toEqual(done());
    expect(partialOutcome(1, 3, words, true)).toEqual({
      kind: 'keepOpen',
      message: '1 of 3',
      refresh: true,
    });
  });
});

/**
 * ADR-184 increment 35: a dialog's Cancel + submit pair is drawn by `FormFooter`, and the save
 * that drives it is `useSubmit`. Thirty-three files wrote the pair by hand.
 *
 * The needle is Cancel wired to close and disabled on the busy flag — the one line every hand copy
 * shares. Not `setBusy(true)`: ten page-level forms set that too, and they are a different
 * contract (the page stays, so busy must be released after a success).
 *
 * ⚠️ The pattern is assembled at runtime and read from code lines only, so this file and a doc
 * comment quoting a footer cannot match.
 */
describe('a dialog saves through useSubmit and draws its footer through FormFooter', () => {
  const NEEDLE = new RegExp(
    ['onClick=\\{(onClose|onCancel)\\}', 'disabled=\\{(busy|saving)\\b'].join('\\s+'),
  );
  const HOME = 'components/ui/FormFooter.tsx';

  /** Dialogs that keep a footer of their own, on purpose. */
  const PERMANENT: Record<string, string> = {
    'components/AddNodeModal/AddNodeModal.tsx':
      'two steps — the probe, then the confirmation of what it found — each with its own buttons',
    'components/MoveByPrefixModal/MoveByPrefixModal.tsx':
      'runs a loop over prefixes and ends on a summary of what moved, not on closing',
  };

  /** Dialogs still to move. Only ever shorter; deleted when empty (increment 40). */
  const NOT_YET_MIGRATED = [
    'components/GroupModal/GroupModal.tsx',
    'components/MoveNodeModal/MoveNodeModal.tsx',
    'components/NodeDetail/EditNodeModal.tsx',
    'components/NodeTree/BulkTagModal.tsx',
    'components/SetPoolModal/SetPoolModal.tsx',
    'components/suppression/AddMaintenanceWindowModal.tsx',
    'components/suppression/AddMuteModal.tsx',
    'pages/ApiTokensPage.tsx',
    'pages/AuthSettingsPage.tsx',
    'pages/BusPanel.tsx',
    'pages/ChannelTemplateModal.tsx',
    'pages/ClassificationRulesPage.tsx',
    'pages/CollectionTemplatesPage.tsx',
    'pages/EventRulesPage.tsx',
    'pages/ForwardingPage.tsx',
    'pages/MibRepositoryPage.tsx',
    'pages/PollersPage.tsx',
    'pages/ProfilesPage.tsx',
    'pages/RoutingPage.tsx',
    'pages/integrations/MerakiIntegrationPage.tsx',
    'pages/integrations/NetboxIntegrationPage.tsx',
    'reports/ReportBuilder.tsx',
    'reports/ScheduleModal.tsx',
    'troubleshoot/ScheduleModal.tsx',
  ];
  const CEILING = 24;

  const sources = readSources().map(([p, src]) => [p, codeOnly(src)] as const);
  const handWritten = sources.filter(([, code]) => NEEDLE.test(code)).map(([p]) => p);

  it('no other dialog writes its footer by hand', () => {
    const offenders = handWritten.filter(
      (p) => p !== HOME && !PERMANENT[p] && !NOT_YET_MIGRATED.includes(p),
    );
    expect(
      offenders,
      `these files write a dialog's Cancel + submit by hand. Use useSubmit and FormFooter ` +
        `(components/ui/FormFooter.tsx):\n  ` +
        offenders.join('\n  '),
    ).toEqual([]);
  });

  it('every listed file still does, so the lists cannot go stale', () => {
    const stale = [...Object.keys(PERMANENT), ...NOT_YET_MIGRATED].filter(
      (p) => !handWritten.includes(p),
    );
    expect(stale, 'listed, but no longer writes its footer by hand — take it off').toEqual([]);
  });

  it('the migration list only shrinks', () => {
    expect(NOT_YET_MIGRATED.length).toBeLessThanOrEqual(CEILING);
    expect(new Set(NOT_YET_MIGRATED).size).toBe(NOT_YET_MIGRATED.length);
  });

  it('finds the dialogs it is supposed to be reading', () => {
    // Counts footers written by hand **or** saves through the shared hook, so the number holds
    // while dialogs move and a walk that found nothing cannot pass.
    const HOOK = `${'useSubmit'}(`;
    const dialogs = sources.filter(
      ([p, code]) => p !== 'lib/useSubmit.ts' && (NEEDLE.test(code) || code.includes(HOOK)),
    );
    expect(dialogs.length).toBeGreaterThanOrEqual(28);
  });
});
