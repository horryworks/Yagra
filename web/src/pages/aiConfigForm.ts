// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ AI analysis form rules (ADR-029). Pure (no React) so the two decisions that are easy
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
  /** The "replace the stored credential" toggle. */
  replaceKey: boolean;
  apiKey: string;
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

/** Whether the credential input is shown (and therefore whether its value is submitted). */
export function keyIsEditable(stored: LlmConfigView | null, form: AiFormState): boolean {
  return !hasUsableStoredKey(stored, form.provider) || form.replaceKey;
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

/** Validation-problem codes; the caller turns these into localized text. */
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
  const n = Number(form.maxTokens.trim());
  if (!Number.isInteger(n) || n < MIN_TOKENS || n > MAX_TOKENS) return 'tokensRange';
  return null;
}

/**
 * The PUT body for the current form.
 *
 * `api_key` is three-valued and the distinction matters: **omitted** keeps the stored credential
 * (so editing the model does not mean re-typing a key), **empty** clears it, a value replaces it.
 */
export function toConfigInput(stored: LlmConfigView | null, form: AiFormState): LlmConfigInput {
  return {
    provider: form.provider,
    model: form.model.trim(),
    project: form.project.trim(),
    location: form.location.trim(),
    enabled: form.enabled,
    max_output_tokens: Number(form.maxTokens.trim()),
    ...(keyIsEditable(stored, form) ? { api_key: form.apiKey } : {}),
  };
}
