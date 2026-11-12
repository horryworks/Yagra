// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind NodeDetail ▸ Config (ADR-210 stages 2 and 3, ADR-240), out of the `.tsx` so
// a test reaches it: the form's state and the body it sends, the status card at the top and what
// it offers to fix, the one history the three records are merged into, which version a version is
// compared with, whether the tab wears its dot, and how often the tab reads itself again.

import type {
  AttemptView,
  BackupInput,
  ConfigBackupView,
  ConfigVersion,
  SaveRecord,
  ScanMode,
} from '../../types/api';
import { BACKUP_INPUTS, SCAN_MODES } from '../../types/api';

/** The settings a person edits. `order` is the node's own order of CLI logins, or null for "every
 *  CLI login in the try order"; `model` is '' for "from the profile", because a `<select>` holds
 *  strings. Whether the backup runs is not here: the Start and Stop buttons save it. */
export interface ConfigBackupForm {
  order: string[] | null;
  input: BackupInput;
  model: string;
  scanMode: ScanMode;
}

/** The form a stored configuration opens as. A stored `unknown` input (a newer core wrote it) opens
 *  as the default — Yagra picks SSH or telnet per attempt (ADR-242) — and saving replaces it. */
export function formOf(view: ConfigBackupView): ConfigBackupForm {
  const input = (BACKUP_INPUTS as readonly string[]).includes(view.input)
    ? (view.input as BackupInput)
    : 'ssh_then_telnet';
  // A mode a newer core wrote opens as the most careful one, which is how this core enforces it —
  // saving then never makes the node faster than it was.
  const scanMode = (SCAN_MODES as readonly string[]).includes(view.scan_mode)
    ? (view.scan_mode as ScanMode)
    : 'slow';
  return {
    order: view.login_order ?? null,
    input,
    model: view.model_override ?? '',
    scanMode,
  };
}

/** The "SSH only" box (ADR-242 decision 5): ticked keeps the node off telnet; unticked lets Yagra
 *  move to telnet when SSH does not connect. A node an API client set to telnet only shows unticked
 *  and stays telnet only until the box is ticked. */
export function sshOnly(form: ConfigBackupForm): boolean {
  return form.input === 'ssh';
}

/** The form with the "SSH only" box set. */
export function withSshOnly(form: ConfigBackupForm, on: boolean): ConfigBackupForm {
  if (on) return { ...form, input: 'ssh' };
  return { ...form, input: form.input === 'ssh' ? 'ssh_then_telnet' : form.input };
}

/** The `PUT …/config-backup` body for a form. */
export function bodyOf(
  form: ConfigBackupForm,
  enabled: boolean,
): {
  enabled: boolean;
  login_order: string[] | null;
  input: BackupInput;
  model_override: string | null;
  scan_mode: ScanMode;
} {
  return {
    enabled,
    login_order: form.order,
    input: form.input,
    model_override: form.model === '' ? null : form.model,
    scan_mode: form.scanMode,
  };
}

const sameOrder = (a: readonly string[] | null, b: readonly string[] | null): boolean =>
  a === null || b === null ? a === b : a.length === b.length && a.every((id, i) => id === b[i]);

/** Whether the form differs from what is stored, so Save means something. */
export function isDirty(form: ConfigBackupForm, view: ConfigBackupView): boolean {
  const stored = formOf(view);
  return (
    !sameOrder(form.order, stored.order) ||
    form.input !== stored.input ||
    form.model !== stored.model ||
    form.scanMode !== stored.scanMode
  );
}

/** The logins the picker shows ticked, in order: the form's own order, or every candidate. */
export function pickedLogins(form: ConfigBackupForm, view: ConfigBackupView): string[] {
  return form.order ?? view.candidates;
}

/** The form the tab opens with. A node switched off that never had its own order opens with no
 *  login ticked: starting it tries only the logins a person picked (ADR-240 Inc.2 decision 9).
 *  "Every CLI login" stays one press away, and is still sent as `null`. */
export function openingFormOf(view: ConfigBackupView): ConfigBackupForm {
  const form = formOf(view);
  return !view.enabled && form.order === null ? { ...form, order: [] } : form;
}

/** Whether the backup can be started from this form: something is ticked to try. */
export function canStartWith(form: ConfigBackupForm, view: ConfigBackupView): boolean {
  return pickedLogins(form, view).length > 0;
}

/** What a picked login shows at the right of its row (ADR-240 Inc.2 decision 10): being tried,
 *  waiting its turn in this pass, or how its newest attempt ended. Null for a login with nothing
 *  to say — never tried, or tried longer ago than the attempts the view carries. */
export function loginMark(view: ConfigBackupView, id: string): Line | null {
  if (!view.enabled) return null;
  const i = view.candidates.indexOf(id);
  if (i < 0) return null;
  const at = view.credential_id ? view.candidates.indexOf(view.credential_id) : -1;
  if (i === at && view.pending) return { key: 'config.loginMark.trying', tone: 'busy' };
  const last = view.attempts.find((a) => a.credential_id === id);
  // The login just refused stays the one handed out until the rotation moves on: it says how it
  // ended, not that it is waiting.
  const justTried = i === at && last !== undefined && view.attempts[0] === last;
  const searching = !view.login_confirmed && !view.exhausted;
  if (searching && at >= 0 && i >= at && !justTried) return { key: 'config.loginMark.waiting', tone: 'none' };
  if (!last) return null;
  return {
    key: `config.loginMark.${last.outcome}`,
    tone:
      last.outcome === 'success'
        ? 'ok'
        : last.outcome === 'unreachable' || last.outcome === 'cancelled'
          ? 'none'
          : 'bad',
  };
}

/** How a login is named on this screen: its name when the caller may list credentials, otherwise
 *  its place in the node's order ("Login 2"), or "a deleted login" for one no longer there. */
export type LoginLabel =
  | { kind: 'name'; name: string }
  | { kind: 'index'; index: number }
  | { kind: 'gone' };

export function loginLabel(
  id: string | null | undefined,
  names: ReadonlyMap<string, string>,
  candidates: readonly string[],
): LoginLabel {
  if (!id) return { kind: 'gone' };
  const name = names.get(id);
  if (name !== undefined) return { kind: 'name', name };
  const i = candidates.indexOf(id);
  return i >= 0 ? { kind: 'index', index: i + 1 } : { kind: 'gone' };
}

/** The colour a line is drawn in. */
export type LineTone = 'ok' | 'bad' | 'busy' | 'none';

/** One sentence: an i18n key under `nodes:`, the login it names (if any), its parameters. */
export interface Line {
  key: string;
  login?: string | null;
  params?: Record<string, number>;
  tone: LineTone;
}

/** The sentence one attempt is shown as. A success says whether it stored a new version. */
export function attemptLine(a: AttemptView): Line {
  if (a.outcome === 'success') {
    const key =
      a.new_version === true
        ? 'config.result.newVersion'
        : a.new_version === false
          ? 'config.result.noChange'
          : 'config.outcome.success';
    return { key, tone: 'ok' };
  }
  return {
    key: `config.outcome.${a.outcome}`,
    login: a.credential_id,
    // Neither a silent device nor a cancelled fetch says anything is wrong with the login.
    tone: a.outcome === 'unreachable' || a.outcome === 'cancelled' ? 'none' : 'bad',
  };
}

/** A save still waiting for its fetch, or running. */
export const saveOpen = (s: SaveRecord): boolean => s.state === 'pending' || s.state === 'running';

/** The sentence one save is shown as (ADR-222). A failure names why. */
export function saveLine(s: SaveRecord): Line {
  switch (s.state) {
    case 'saved':
      return { key: 'config.saves.state.saved', tone: 'ok' };
    case 'failed':
      return { key: `config.saves.failure.${s.failure ?? 'other'}`, tone: 'bad' };
    case 'pending':
    case 'running':
      return { key: `config.saves.state.${s.state}`, tone: 'busy' };
  }
}

/** Whether the save button is offered: the node is backed up with a login that works, its model
 *  can be saved, and no save is already waiting or running. */
export function canOfferSave(view: ConfigBackupView): boolean {
  return (
    view.available &&
    view.enabled &&
    view.save_supported &&
    view.login_confirmed &&
    view.readiness === 'ready' &&
    !view.saves.some(saveOpen)
  );
}

/** Where the backup stands, as the one line under the buttons. */
export function stateLine(view: ConfigBackupView): Line {
  if (!view.enabled) return { key: 'config.readiness.off', tone: 'none' };
  if (view.readiness !== 'ready') return { key: `config.readiness.${view.readiness}`, tone: 'bad' };
  if (view.pending) {
    if (view.login_confirmed) {
      return { key: 'config.state.fetching', login: view.credential_id, tone: 'busy' };
    }
    const n = view.credential_id ? view.candidates.indexOf(view.credential_id) + 1 : 0;
    return {
      key: 'config.state.trying',
      login: view.credential_id,
      params: { n: Math.max(n, 1), total: view.candidates.length },
      tone: 'busy',
    };
  }
  if (view.exhausted) {
    // Given up on because the device stopped answering, not because every login was refused.
    const gaveUp = view.attempts[0]?.outcome === 'unreachable';
    return { key: gaveUp ? 'config.state.gaveUp' : 'config.state.exhausted', tone: 'bad' };
  }
  const last = view.attempts[0];
  if (last) return attemptLine(last);
  return { key: 'config.state.waiting', tone: 'none' };
}

/** How often the tab reads itself again: fast while a fetch is out, slowly while a login is still
 *  being looked for (the leader moves the rotation on every minute), not at all once settled. */
export function refreshMs(view: ConfigBackupView | null): number | undefined {
  if (!view?.enabled) return undefined;
  if (view.pending || view.saves.some(saveOpen)) return 3_000;
  if (!view.login_confirmed && !view.exhausted) return 30_000;
  return undefined;
}

/** A version id as the list shows it. */
export const shortOid = (oid: string): string => oid.slice(0, 8);

/** The version stored just before `oid` — what "Compare" compares it with — or null for the oldest.
 *  The list is newest first, as oxidized gives it. */
export function previousOf(versions: readonly ConfigVersion[], oid: string): string | null {
  const i = versions.findIndex((v) => v.oid === oid);
  return i >= 0 && i + 1 < versions.length ? versions[i + 1].oid : null;
}

/** Whether a refused "fetch now" is nothing to report: a fetch is already out for this node, or the
 *  node has nothing to fetch with — the state line already says which. Another node's fetch holding
 *  this one back (`config_backup_busy`) is reported, with the server's sentence. The lockout limits
 *  never refuse it: a press tries at once (ADR-210 stage 3 decision 25). */
export function isQuietFetchRefusal(status: number, code: string): boolean {
  return status === 409 && (code === 'config_backup_pending' || code === 'config_backup_not_ready');
}

/** What the status card at the top of the tab is (ADR-240 decision 1). */
export type CardKind = 'off' | 'busy' | 'ok' | 'fix';

/** The one way out the card offers for a problem (decision 2). `fetch` asks for a fetch now;
 *  `rescan` starts the login search again; `resave` saves the settings as they are. */
export const CARD_FIXES = ['pickModel', 'addLogin', 'rescan', 'fetch', 'settings', 'resave', 'pollers'] as const;
export type CardFix = (typeof CARD_FIXES)[number];

export interface Card {
  kind: CardKind;
  line: Line;
  fixes: CardFix[];
}

/** The fix each reason the node is not fetched points at. Keyed by every readiness so a new one
 *  cannot compile without an answer; `ready` and `off` never reach it. */
const READINESS_FIX: Record<ConfigBackupView['readiness'], CardFix[]> = {
  ready: [],
  off: [],
  no_model: ['pickModel'],
  no_credential: ['addLogin'],
  unknown_input: ['settings'],
  address_changed: ['resave'],
  no_poller: ['pollers'],
  poller_too_old: ['pollers'],
  relay_refused: ['pollers'],
};

/** When the node was last fetched: core's answer, read from every attempt. A core older than that
 *  field leaves it out, and then the newest successful attempt, or the newest stored version, says.
 *  `null` when none does. */
export function lastSuccessAt(view: ConfigBackupView, versions: readonly ConfigVersion[]): string | null {
  const ok = view.attempts.find((a) => a.outcome === 'success');
  return view.last_success_at ?? ok?.at ?? versions[0]?.stored_at ?? null;
}

/** The status card for a node. Whether it is stale is core's verdict (`view.stale`), the one that
 *  opens its "config backup not fetched" alert, so the card, the tab's dot and the alert agree —
 *  a node still looking for a login included. */
export function cardOf(view: ConfigBackupView): Card {
  if (!view.enabled) return { kind: 'off', line: { key: 'config.card.off', tone: 'none' }, fixes: [] };
  if (view.readiness !== 'ready') {
    return { kind: 'fix', line: stateLine(view), fixes: READINESS_FIX[view.readiness] };
  }
  if (view.pending) return { kind: 'busy', line: stateLine(view), fixes: [] };
  if (view.exhausted) {
    const line = stateLine(view);
    const fixes: CardFix[] = line.key === 'config.state.gaveUp' ? ['rescan'] : ['addLogin', 'rescan'];
    return { kind: 'fix', line, fixes };
  }
  if (view.stale) {
    // A working login is fetched again; a search that has gone on this long is started again.
    const fixes: CardFix[] = view.login_confirmed ? ['fetch'] : ['rescan'];
    return { kind: 'fix', line: { key: 'config.card.stale', tone: 'bad' }, fixes };
  }
  if (!view.login_confirmed) {
    // Between two tries the card still says which login is next (ADR-240 Inc.2 decision 11).
    const n = view.credential_id ? view.candidates.indexOf(view.credential_id) + 1 : 0;
    const line: Line =
      n > 0
        ? { key: 'config.card.searchingN', params: { n, total: view.candidates.length }, tone: 'busy' }
        : { key: 'config.card.searching', tone: 'busy' };
    return { kind: 'busy', line, fixes: [] };
  }
  return { kind: 'ok', line: { key: 'config.card.ok', tone: 'ok' }, fixes: [] };
}

/** Whether the Config tab wears the attention dot (decision 5): the card asks for a fix, or the
 *  running configuration is unsaved. */
export function needsAttention(view: ConfigBackupView | null): boolean {
  if (!view?.available || !view.enabled) return false;
  return cardOf(view).kind === 'fix' || !!view.unsaved;
}

/** Which rows the history shows: what changed (failures included), or everything. */
export const HISTORY_FILTERS = ['changes', 'all'] as const;
export type HistoryFilter = (typeof HISTORY_FILTERS)[number];

/** One row of the history (decision 4). A fetch that stored a new version is its version's row;
 *  one that found no change is `same`, folded into a run under `changes`. */
export type HistoryRow =
  | { kind: 'version'; at: string | null; oid: string; prev: string | null }
  | { kind: 'save'; at: string; save: SaveRecord }
  | { kind: 'attempt'; at: string; attempt: AttemptView }
  | { kind: 'same'; at: string | null; count: number };

const timeOf = (at: string | null): number => (at === null ? Number.NEGATIVE_INFINITY : Date.parse(at));

/** The stored versions, the saves and the attempts as one list, newest first. A version whose time
 *  could not be read sorts after everything timed and keeps its place among the others. */
export function historyRows(
  versions: readonly ConfigVersion[],
  saves: readonly SaveRecord[],
  attempts: readonly AttemptView[],
  filter: HistoryFilter,
): HistoryRow[] {
  const rows: HistoryRow[] = [
    ...versions.map(
      (v): HistoryRow => ({ kind: 'version', at: v.stored_at ?? null, oid: v.oid, prev: previousOf(versions, v.oid) }),
    ),
    ...saves.map((s): HistoryRow => ({ kind: 'save', at: s.requested_at, save: s })),
    ...attempts.flatMap((a): HistoryRow[] => {
      if (a.outcome !== 'success') return [{ kind: 'attempt', at: a.at, attempt: a }];
      // A new version is already its version's row.
      return a.new_version === true ? [] : [{ kind: 'same', at: a.at, count: 1 }];
    }),
  ];
  const sorted = rows
    .map((r, i) => ({ r, i }))
    .sort((x, y) => timeOf(y.r.at) - timeOf(x.r.at) || x.i - y.i)
    .map((x) => x.r);
  if (filter === 'all') return sorted;
  const folded: HistoryRow[] = [];
  for (const r of sorted) {
    const last = folded.at(-1);
    if (r.kind === 'same' && last?.kind === 'same') last.count += 1;
    else folded.push(r.kind === 'same' ? { ...r } : r);
  }
  return folded;
}
