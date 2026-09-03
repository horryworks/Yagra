// SPDX-License-Identifier: AGPL-3.0-only
// The Site ID field picker's decisions, as pure functions (ADR-100 decision 9).
//
// Why a `.ts` beside the page rather than inside `NetboxIntegrationPage.tsx`: Vitest's `include`
// is `src/**/*.test.ts` and never loads a `.tsx`, so judgement written there is judgement no test
// can reach (`tsxJudgement.test.ts`). Everything here is a decision, so all of it belongs here.

import { SITE_ID_BUILT_INS, type NetboxSiteIdFields } from '../../types/api';

/** The sentinel for "no prefix". Not a stored value — the column is simply `NULL`. */
export const SITE_ID_NONE = '';

/** The sentinel for "the field I want is not in the list, let me type its key". */
export const SITE_ID_OTHER = '__other__';

/** The `cf:` marker the backend encodes a custom field with (`SiteIdField::as_stored`). */
const CUSTOM_PREFIX = 'cf:';

/** One row of the picker. */
export type SiteIdOption =
  | { kind: 'none' }
  | { kind: 'builtIn'; value: string }
  /** `label` is NetBox's own wording and therefore **not translatable**. */
  | { kind: 'custom'; value: string; label: string }
  | { kind: 'other' };

/**
 * The picker's rows, in the order they are shown.
 *
 * `choices` is `null` before the field list has been fetched (or when it could not be). The
 * built-ins are still offered in that case — they are known from the code, so the picker is never
 * empty — and so is the current value, which matters most in the edit form: a server already set
 * to `cf:site_id` must not silently appear as "none" while the list loads.
 */
export function siteIdOptions(
  choices: NetboxSiteIdFields | null,
  current: string | null,
): SiteIdOption[] {
  const out: SiteIdOption[] = [{ kind: 'none' }];
  for (const v of SITE_ID_BUILT_INS) out.push({ kind: 'builtIn', value: v });

  const seen = new Set<string>();
  for (const f of choices?.custom_fields ?? []) {
    if (seen.has(f.value)) continue;
    seen.add(f.value);
    out.push({ kind: 'custom', value: f.value, label: f.label });
  }
  // The stored value, when the listing does not (or cannot) account for it. Without this a saved
  // setting would be shown as something the operator did not choose.
  if (current && isCustom(current) && !seen.has(current)) {
    out.push({ kind: 'custom', value: current, label: customKey(current) });
  }

  out.push({ kind: 'other' });
  return out;
}

/** Is this stored value a custom field rather than a built-in? */
export function isCustom(stored: string): boolean {
  return stored.startsWith(CUSTOM_PREFIX);
}

/** The bare key inside a `cf:` value, for showing and for editing. */
export function customKey(stored: string): string {
  return stored.startsWith(CUSTOM_PREFIX) ? stored.slice(CUSTOM_PREFIX.length) : stored;
}

/** A typed key, encoded the way the column stores it. */
export function customValue(key: string): string {
  return `${CUSTOM_PREFIX}${key.trim()}`;
}

/**
 * Which row a stored setting selects, and what belongs in the type-it-in box.
 *
 * A custom field the listing knows about selects its own row; one it does not selects `Other` with
 * the key filled in, so a value that arrived from a REST client, an older NetBox, or a token that
 * cannot read the definitions is still editable rather than invisible.
 */
export function selectionFor(
  stored: string | null,
  choices: NetboxSiteIdFields | null,
): { selected: string; customKeyInput: string } {
  if (!stored) return { selected: SITE_ID_NONE, customKeyInput: '' };
  if (!isCustom(stored)) return { selected: stored, customKeyInput: '' };
  const known = (choices?.custom_fields ?? []).some((f) => f.value === stored);
  return known
    ? { selected: stored, customKeyInput: '' }
    : { selected: SITE_ID_OTHER, customKeyInput: customKey(stored) };
}

/**
 * The value to send, or `null` for "no prefix".
 *
 * ⚠️ Mirrors `SiteIdField::parse`'s rule that an empty key is not a field: an `Other` row with
 * nothing typed sends `null` rather than `cf:`, which the API would refuse with a 400 the operator
 * did not earn.
 */
export function siteIdFieldToSend(selected: string, customKeyInput: string): string | null {
  if (selected === SITE_ID_NONE) return null;
  if (selected !== SITE_ID_OTHER) return selected;
  const key = customKeyInput.trim();
  return key === '' ? null : customValue(key);
}

/**
 * Is a typed key one the backend will accept? Mirrors `SiteIdField::parse`'s bounds.
 *
 * ⚠️ **A second copy of a rule, and deliberately the lenient half of it.** The backend is the
 * authority and answers 400; this exists only so the operator is told before pressing Save rather
 * than after. It must never be *stricter* than the backend, or it would refuse something valid
 * with no way to override.
 */
export function customKeyLooksValid(key: string): boolean {
  const k = key.trim();
  return k.length > 0 && k.length <= 64 && /^[A-Za-z0-9_]+$/.test(k);
}

/**
 * How a sync's Site ID outcome reads, or `null` when there is nothing to say.
 *
 * 🚨 The whole reason this is surfaced: with the wrong field chosen, nothing happens — no error,
 * no changed name, no failed sync. "2 of 2 sites have no Site ID" is the sentence that turns that
 * silence into something an operator can act on.
 */
export function siteIdOutcome(
  sites: number,
  without: number,
): { kind: 'ok' | 'partial' | 'none'; without: number; sites: number } | null {
  if (without === 0) return null;
  if (sites > 0 && without >= sites) return { kind: 'none', without, sites };
  return { kind: 'partial', without, sites };
}
