// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind NodeDetail ▸ Config (ADR-210 stages 2 and 3), out of the `.tsx` so a test
// reaches it: the form's state and the body it sends, the one line that says where the backup
// stands, which version a version is compared with, and how often the tab reads itself again.

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
 *  as the default, and saving replaces it — the select cannot show a value it does not offer. */
export function formOf(view: ConfigBackupView): ConfigBackupForm {
  const input = (BACKUP_INPUTS as readonly string[]).includes(view.input)
    ? (view.input as BackupInput)
    : 'ssh';
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

/** Whether the try order is drawn open: always while a login is still being looked for, and only
 *  when the operator opens it once one works — the working login is then named on its own line
 *  (ADR-210 stage 5 decision 55). */
export function orderShown(view: ConfigBackupView, opened: boolean): boolean {
  return !view.login_confirmed || !view.credential_id || opened;
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
