// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind NodeDetail ▸ Config (ADR-210 stage 2), out of the `.tsx` so a test reaches
// it: the form's state and the body it sends, which version a version is compared with, and when
// what oxidized last said is too old to stand for "now".

import type { BackupInput, ConfigBackupView, ConfigVersion } from '../../types/api';
import { BACKUP_INPUTS } from '../../types/api';

/** The form, as the selects hold it. `credentialId` and `model` are '' for "none" / "from the
 *  profile", because a `<select>` holds strings. */
export interface ConfigBackupForm {
  enabled: boolean;
  credentialId: string;
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
    enabled: view.enabled,
    credentialId: view.credential_id ?? '',
    input,
    model: view.model_override ?? '',
  };
}

/** The `PUT …/config-backup` body for a form. */
export function bodyOf(form: ConfigBackupForm): {
  enabled: boolean;
  credential_id: string | null;
  input: BackupInput;
  model_override: string | null;
} {
  return {
    enabled: form.enabled,
    credential_id: form.credentialId === '' ? null : form.credentialId,
    input: form.input,
    model_override: form.model === '' ? null : form.model,
  };
}

/** Whether the form differs from what is stored, so Save means something. */
export function isDirty(form: ConfigBackupForm, view: ConfigBackupView): boolean {
  const stored = formOf(view);
  return (
    form.enabled !== stored.enabled ||
    form.credentialId !== stored.credentialId ||
    form.input !== stored.input ||
    form.model !== stored.model
  );
}

/** A version id as the list shows it. */
export const shortOid = (oid: string): string => oid.slice(0, 8);

/** The version stored just before `oid` — what "Compare" compares it with — or null for the oldest.
 *  The list is newest first, as oxidized gives it. */
export function previousOf(versions: readonly ConfigVersion[], oid: string): string | null {
  const i = versions.findIndex((v) => v.oid === oid);
  return i >= 0 && i + 1 < versions.length ? versions[i + 1].oid : null;
}

/** How long a status read from oxidized may be old before the screen says it is stale. The leader
 *  reads it every five minutes, so three missed reads. */
export const STATUS_STALE_MS = 15 * 60 * 1000;

/** Whether what oxidized last said was read so long ago that oxidized is probably not answering. */
export function isStale(observedAt: string, nowMs: number): boolean {
  const t = Date.parse(observedAt);
  return Number.isFinite(t) && nowMs - t > STATUS_STALE_MS;
}

/** The colour a last result is drawn in. */
export function resultTone(status: string | null | undefined): 'ok' | 'bad' | 'none' {
  if (status === 'success') return 'ok';
  if (status == null || status === 'never') return 'none';
  return 'bad';
}
