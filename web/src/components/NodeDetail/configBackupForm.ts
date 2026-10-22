// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind NodeDetail ▸ Config (ADR-210 stages 2 and 3), out of the `.tsx` so a test
// reaches it: the form's state and the body it sends, the one line that says where the backup
// stands, which version a version is compared with, and how often the tab reads itself again.

import type { AttemptView, BackupInput, ConfigBackupView, ConfigVersion } from '../../types/api';
import { BACKUP_INPUTS } from '../../types/api';

/** The settings a person edits. `order` is the node's own order of CLI logins, or null for "every
 *  CLI login in the try order"; `model` is '' for "from the profile", because a `<select>` holds
 *  strings. Whether the backup runs is not here: the Start and Stop buttons save it. */
export interface ConfigBackupForm {
  order: string[] | null;
  input: BackupInput;
  model: string;
}

/** The form a stored configuration opens as. A stored `unknown` input (a newer core wrote it) opens
 *  as the default, and saving replaces it — the select cannot show a value it does not offer. */
export function formOf(view: ConfigBackupView): ConfigBackupForm {
  const input = (BACKUP_INPUTS as readonly string[]).includes(view.input)
    ? (view.input as BackupInput)
    : 'ssh';
  return {
    order: view.login_order ?? null,
    input,
    model: view.model_override ?? '',
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
} {
  return {
    enabled,
    login_order: form.order,
    input: form.input,
    model_override: form.model === '' ? null : form.model,
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
    form.model !== stored.model
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
    tone: a.outcome === 'unreachable' ? 'none' : 'bad',
  };
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
  if (view.exhausted) return { key: 'config.state.exhausted', tone: 'bad' };
  const last = view.attempts[0];
  if (last) return attemptLine(last);
  return { key: 'config.state.waiting', tone: 'none' };
}

/** How often the tab reads itself again: fast while a fetch is out, slowly while a login is still
 *  being looked for (the leader moves the rotation on every minute), not at all once settled. */
export function refreshMs(view: ConfigBackupView | null): number | undefined {
  if (!view?.enabled) return undefined;
  if (view.pending) return 3_000;
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

/** Whether a refused "fetch now" is nothing to report: the limits hold the next login back (429) or
 *  a fetch is already out or the node cannot be fetched (409). The state line says which. */
export function isQuietFetchRefusal(status: number): boolean {
  return status === 429 || status === 409;
}
