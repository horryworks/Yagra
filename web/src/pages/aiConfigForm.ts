// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ AI analysis form rules (ADR-029). Pure (no React) so the decisions that are easy
// to get quietly wrong — what the write-only `api_key` field means on submit, and when a stored
// credential still applies — are unit-testable without rendering.
//
// These mirror the Rust side: `rca/store.rs` `LlmConfigInput` (three-valued `api_key`) and the
// vendor-scoped `CASE WHEN … llm_config.provider = EXCLUDED.provider` in its upsert. The backend
// re-validates and is the authority; this exists so the form asks for what will actually be needed
// instead of letting an admin discover it as a 400 — or, worse, as a 502 mid-incident.

import type { LlmConfigInput, LlmConfigView, LlmProviderChoice } from '../types/api';

/** Mirror of `rca/store.rs` MIN/MAX_OUTPUT_TOKENS and the CHECK in migration 0053. */
export const MIN_TOKENS = 256;
export const MAX_TOKENS = 65536;
export const DEFAULT_TOKENS = 8192;

/** What the form currently holds. */
export interface AiFormState {
  provider: string;
  model: string;
  project: string;
  location: string;
  maxTokens: string;
  enabled: boolean;
  /** The box of the credential field. Empty while a credential is stored means "keep it". */
  apiKey: string;
  /** Remove the stored credential — offered only where none is needed (Vertex's Workload
   *  Identity). An empty box never clears one; this is the separate decision that does. */
  clearKey: boolean;
}

/**
 * Whether the stored credential still applies to the selected provider.
 *
 * A key belongs to the vendor it was entered for: a Gemini key is not a Claude key. The backend
 * drops it on a vendor switch for that reason, so the form must stop claiming one is on file.
 */
export function hasUsableStoredKey(stored: LlmConfigView | null, provider: string): boolean {
  return stored != null && stored.provider === provider && stored.has_api_key;
}

/**
 * Whether the operator must supply a credential before saving.
 *
 * False for Vertex — an empty credential there selects Workload Identity, which is the better
 * deployment on GKE/GCE and stores no secret at all.
 */
export function keyIsRequired(
  stored: LlmConfigView | null,
  choice: LlmProviderChoice | undefined,
  provider: string,
): boolean {
  return choice != null && !choice.credential_optional && !hasUsableStoredKey(stored, provider);
}

/** Every reason the AI settings form refuses to save. `as const` so the i18n coverage test can walk
 *  it: the page renders `t(`err.${problem}`)` with no fallback. */
export const AI_FORM_PROBLEMS = [
  'modelRequired',
  'projectRequired',
  'keyRequired',
  'tokensRange',
] as const;

export type AiFormProblem = (typeof AI_FORM_PROBLEMS)[number];

/** The first problem with the form, or `null` when it can be submitted. */
export function validateAiForm(
  stored: LlmConfigView | null,
  choice: LlmProviderChoice | undefined,
  form: AiFormState,
): AiFormProblem | null {
  if (form.model.trim() === '') return 'modelRequired';
  if (choice?.needs_project && (form.project.trim() === '' || form.location.trim() === ''))
    return 'projectRequired';
  if (keyIsRequired(stored, choice, form.provider) && form.apiKey.trim() === '')
    return 'keyRequired';
  // Clearing is offered only where a credential is optional; a form that asks for it elsewhere
  // would leave a key-only provider with none.
  if (form.clearKey && !choice?.credential_optional) return 'keyRequired';
  const n = Number(form.maxTokens.trim());
  if (!Number.isInteger(n) || n < MIN_TOKENS || n > MAX_TOKENS) return 'tokensRange';
  return null;
}

/**
 * The PUT body for the current form.
 *
 * `api_key` is three-valued and the distinction matters: **omitted** keeps the stored credential
 * (so editing the model does not mean re-typing a key), **empty** clears it, a value replaces it.
 * With a credential stored for this vendor, an empty box keeps it (the `SecretInput` rule) and only
 * `clearKey` sends the empty value. With none stored, the box is sent as it is — empty is how Vertex
 * selects Workload Identity, and after a vendor switch it is what drops the old vendor's key.
 */
export function toConfigInput(stored: LlmConfigView | null, form: AiFormState): LlmConfigInput {
  return {
    provider: form.provider,
    model: form.model.trim(),
    project: form.project.trim(),
    location: form.location.trim(),
    enabled: form.enabled,
    max_output_tokens: Number(form.maxTokens.trim()),
    ...apiKeyToSend(stored, form),
  };
}

function apiKeyToSend(
  stored: LlmConfigView | null,
  form: AiFormState,
): Pick<LlmConfigInput, 'api_key'> {
  if (!hasUsableStoredKey(stored, form.provider)) return { api_key: form.apiKey };
  if (form.clearKey) return { api_key: '' };
  // Never trimmed when sent: a secret's format is not ours. Blank only decides "keep".
  return form.apiKey.trim() === '' ? {} : { api_key: form.apiKey };
}

/** The form as the stored configuration would fill it. */
export function formFromStored(stored: LlmConfigView): AiFormState {
  return {
    provider: stored.provider,
    model: stored.model,
    project: stored.project,
    location: stored.location,
    maxTokens: String(stored.max_output_tokens),
    enabled: stored.enabled,
    apiKey: '',
    clearKey: false,
  };
}

/** Whether a save would change anything. Asked of the payload, so it cannot disagree with what
 *  Save sends: trailing spaces and an untouched secret box are not changes. `true` when nothing is
 *  stored yet. */
export function aiFormChanged(stored: LlmConfigView | null, form: AiFormState): boolean {
  if (stored == null) return true;
  const now = toConfigInput(stored, form);
  const saved = toConfigInput(stored, formFromStored(stored));
  return JSON.stringify(now) !== JSON.stringify(saved);
}

/** Whether Test may be pressed. The test sends a prompt with the **stored** configuration, so it
 *  waits for a first save and for every later edit to be saved — a result would otherwise appear to
 *  describe what is on screen. This replaced the sentence "Save first" (ADR-200). */
export function canTestAi(stored: LlmConfigView | null, form: AiFormState): boolean {
  return stored != null && !aiFormChanged(stored, form);
}

/**
 * Whether saving needs the operator to confirm that incident data will leave their boundary.
 *
 * Asked when the save switches sending on for a vendor outside the operator's cloud: enabling it,
 * or moving an enabled configuration to such a vendor. A save that leaves an already-enabled
 * configuration on the same vendor is not asked again (ADR-200 kind 3: the warning at the moment
 * of the choice, in place of a paragraph always on screen).
 */
export function needsEgressConfirm(
  stored: LlmConfigView | null,
  choice: LlmProviderChoice | undefined,
  form: AiFormState,
): boolean {
  if (!form.enabled || choice?.leaves_operator_boundary !== true) return false;
  return !(stored?.enabled === true && stored.provider === form.provider);
}
