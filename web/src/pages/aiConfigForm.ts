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
  /** The server's address, for a provider that `needs_base_url` (ADR-247). */
  baseUrl: string;
  maxTokens: string;
  enabled: boolean;
  /** The box of the credential field. Empty while a credential is stored means "keep it". */
  apiKey: string;
  /** Remove the stored credential — offered only where none is needed (Vertex's Workload
   *  Identity). An empty box never clears one; this is the separate decision that does. */
  clearKey: boolean;
}

/** Placeholder for the server address: the shape, not a default (an empty box is refused). */
export const BASE_URL_EXAMPLE = 'http://llm.example.com:8080/v1';

/** The address as core stores it: trimmed, without a trailing `/` (`rca/openai.rs`). */
export function normalizeBaseUrl(raw: string): string {
  return raw.trim().replace(/\/+$/, '');
}

/**
 * Whether the stored credential still applies to the selected provider.
 *
 * A key belongs to the vendor it was entered for: a Gemini key is not a Claude key. The backend
 * drops it on a vendor switch for that reason, so the form must stop claiming one is on file. An
 * operator's own server's key belongs to its address too (ADR-247): typing another URL drops it.
 * `baseUrl` is the form's box, and is read only when the stored provider has an address.
 */
export function hasUsableStoredKey(
  stored: LlmConfigView | null,
  provider: string,
  baseUrl = '',
): boolean {
  if (stored == null || stored.provider !== provider || !stored.has_api_key) return false;
  return stored.base_url === '' || stored.base_url === normalizeBaseUrl(baseUrl);
}

/** Whether the box holds something core would take as a server address (`validate_base_url`). */
export function isUsableBaseUrl(raw: string): boolean {
  let url: URL;
  try {
    url = new URL(raw.trim());
  } catch {
    return false;
  }
  return (
    (url.protocol === 'http:' || url.protocol === 'https:') &&
    url.username === '' &&
    url.password === '' &&
    url.search === '' &&
    url.hash === '' &&
    url.hostname !== ''
  );
}

/**
 * Whether a prompt sent to `raw` leaves the operator's own network — the same answer as
 * `rca/openai.rs::url_leaves_boundary`, and both run `fixtures/llmBaseUrl.cases.json`.
 *
 * Inside only when the host is a literal private, loopback, link-local or IPv6 unique-local
 * address. A name always answers "leaves": what it resolves to is not the form's to vouch for.
 */
export function localUrlLeavesBoundary(raw: string): boolean {
  let host: string;
  try {
    host = new URL(raw.trim()).hostname;
  } catch {
    return true;
  }
  // The URL parser has already rewritten any IPv4 spelling (`0x7f.1`) as dotted decimal and any
  // IPv6 one in its compressed hex form, which is all the two readers below need to accept.
  const v4 = parseIpv4(host);
  if (v4) return !ipv4Inside(v4);
  if (host.startsWith('[') && host.endsWith(']')) {
    const v6 = parseIpv6(host.slice(1, -1));
    if (v6) return !ipv6Inside(v6);
  }
  return true;
}

function parseIpv4(host: string): number[] | null {
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
  if (!m) return null;
  const octets = m.slice(1).map(Number);
  return octets.every((o) => o <= 255) ? octets : null;
}

function ipv4Inside([a, b]: number[]): boolean {
  return (
    a === 10 ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168) ||
    a === 127 ||
    (a === 169 && b === 254)
  );
}

function parseIpv6(text: string): number[] | null {
  const halves = text.split('::');
  if (halves.length > 2) return null;
  const read = (part: string): number[] | null => {
    if (part === '') return [];
    const groups = part.split(':').map((g) => (/^[0-9a-f]{1,4}$/i.test(g) ? parseInt(g, 16) : NaN));
    return groups.some(Number.isNaN) ? null : groups;
  };
  const head = read(halves[0]);
  const tail = halves.length === 2 ? read(halves[1]) : [];
  if (!head || !tail) return null;
  const fill = 8 - head.length - tail.length;
  if (halves.length === 1 ? fill !== 0 : fill < 1) return null;
  return [...head, ...Array<number>(fill).fill(0), ...tail];
}

function ipv6Inside(g: number[]): boolean {
  const zeros = (n: number) => g.slice(0, n).every((x) => x === 0);
  if (zeros(5) && g[5] === 0xffff) {
    return ipv4Inside([g[6] >> 8, g[6] & 0xff]);
  }
  if (zeros(7) && g[7] === 1) return true;
  return (g[0] & 0xfe00) === 0xfc00 || (g[0] & 0xffc0) === 0xfe80;
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

/** Whether saving this form sends incident data outside the operator's boundary. */
export function formLeavesBoundary(choice: LlmProviderChoice | undefined, form: AiFormState): boolean {
  if (choice?.needs_base_url) return localUrlLeavesBoundary(form.baseUrl);
  return choice?.leaves_operator_boundary === true;
}

/** Every reason the AI settings form refuses to save. `as const` so the i18n coverage test can walk
 *  it: the page renders `t(`err.${problem}`)` with no fallback. */
export const AI_FORM_PROBLEMS = [
  'modelRequired',
  'projectRequired',
  'urlRequired',
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
  if (choice?.needs_base_url && !isUsableBaseUrl(form.baseUrl)) return 'urlRequired';
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
    base_url: normalizeBaseUrl(form.baseUrl),
    enabled: form.enabled,
    max_output_tokens: Number(form.maxTokens.trim()),
    ...apiKeyToSend(stored, form),
  };
}

function apiKeyToSend(
  stored: LlmConfigView | null,
  form: AiFormState,
): Pick<LlmConfigInput, 'api_key'> {
  if (!hasUsableStoredKey(stored, form.provider, form.baseUrl)) return { api_key: form.apiKey };
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
    baseUrl: stored.base_url,
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
 * of the choice, in place of a paragraph always on screen). For an operator's own server, a new
 * address is a new destination and is asked about again (ADR-247).
 */
export function needsEgressConfirm(
  stored: LlmConfigView | null,
  choice: LlmProviderChoice | undefined,
  form: AiFormState,
): boolean {
  if (!form.enabled || !formLeavesBoundary(choice, form)) return false;
  return !(
    stored?.enabled === true &&
    stored.provider === form.provider &&
    stored.base_url === (choice?.needs_base_url ? normalizeBaseUrl(form.baseUrl) : '')
  );
}

/** The host a confirm names when the destination is an operator's own server, else `null`. */
export function egressHost(choice: LlmProviderChoice | undefined, form: AiFormState): string | null {
  if (!choice?.needs_base_url) return null;
  try {
    return new URL(form.baseUrl.trim()).host;
  } catch {
    return null;
  }
}
