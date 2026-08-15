// SPDX-License-Identifier: AGPL-3.0-only
// Account-scoped WebUI preferences: the sync between `prefs.ts` (this browser) and the server
// (`GET`/`PUT /api/v1/preferences`, ADR-058). What this buys is that a preference set on one machine
// is there on the next one the same person signs in from.
//
// WHY THE VALUE STILL LIVES IN `prefs.ts` AND NOT IN A STORE OF ITS OWN
// The server is authoritative, but it is not *available* at first paint, when signed out, offline,
// or against a core older than ADR-058. Reading through a server-only store would mean every
// consumer handling "not loaded yet" — and the dock would visibly jump when the answer arrived.
// So `prefs.ts` stays the single read path (localStorage, painted immediately) and this module is
// pure machinery: it adopts the account's value on sign-in and mirrors writes back out.
//
// ⚠️ **The failure mode this file exists to prevent is noise, not data loss.** Nothing here surfaces
// an error, ever. A 404 (old core), a 405, a network drop, an expired session — all mean the same
// thing to the operator: their browser-local value keeps working. A toast saying "failed to save
// your chart height" would be worse than the problem.

import { usePrefsStore } from './prefs';
import { api, getToken } from './services/api';

/** Coalesce a burst of adjustments into one write. Same value and same reason as the dashboard's
 *  (`dashboard/layoutStore.ts`): a drag emits a value per frame, and **every** PUT writes an audit
 *  row — the backend has no per-route opt-out, so debouncing here is a contract, not a nicety. */
const SAVE_DEBOUNCE_MS = 800;

/** The document this browser reads and writes. Every field optional: an older or newer WebUI may
 *  have written the row, so a missing key must read as "not set", never as an error. */
interface ServerPrefsDoc {
  /** Node-detail Interfaces chart dock height, px (issue #65). */
  interfaceDockHeight?: number;
}

/** False once the server has told us it does not serve this endpoint, so a drag on a deployment
 *  running an N-1 core does not PUT into a 404 every 800ms for the rest of the session. */
let supported = true;
let saveTimer: ReturnType<typeof setTimeout> | undefined;

/** Read the fields we understand out of whatever the server returned, ignoring the rest.
 *
 *  Defensive because the document is opaque to the backend: it validates only that the body is a
 *  JSON object, so a value of the wrong type is a thing this function must survive rather than a
 *  thing the API prevents. */
function adopt(raw: unknown): void {
  if (raw == null || typeof raw !== 'object') return;
  const doc = raw as ServerPrefsDoc;
  if (typeof doc.interfaceDockHeight === 'number' && Number.isFinite(doc.interfaceDockHeight)) {
    // Straight into the local store — the dock re-clamps it for the container it is actually in
    // (`interfaceDockHeight.ts::resolveDockHeight`), so a height dragged out on a big monitor does
    // not swallow the list on a laptop.
    usePrefsStore.getState().setInterfaceDockHeight(doc.interfaceDockHeight);
  }
}

/** The document to send: the account-scoped subset of `prefs.ts`. */
function currentDoc(): ServerPrefsDoc {
  const { interfaceDockHeight } = usePrefsStore.getState();
  return interfaceDockHeight == null ? {} : { interfaceDockHeight };
}

/**
 * Pull the signed-in account's preferences and adopt them locally. Called once per sign-in.
 *
 * Never rejects and never surfaces anything: an unsupported endpoint, an expired session or a
 * network drop all leave the browser-local values in place, which is the correct outcome.
 */
export async function loadServerPrefs(): Promise<void> {
  if (!getToken()) return;
  try {
    adopt(await api.getPreferences());
    supported = true;
  } catch {
    // 404/405 ⇒ a core older than ADR-058; anything else ⇒ transient. Both mean "keep local".
    // Marking it unsupported on a *transient* failure only costs this session's syncing, whereas
    // retrying against a genuine 404 would PUT into it on every adjustment.
    supported = false;
  }
}

/** Forget the previous account's sync state. Call on sign-out, before the next sign-in. */
export function resetServerPrefs(): void {
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = undefined;
  supported = true;
}

/** Queue a save of the current document after a short quiet period. */
function scheduleSave(): void {
  if (!supported || !getToken()) return;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = undefined;
    // Deliberately unhandled beyond swallowing: see this file's header. A failed save leaves the
    // local value correct for this browser, which is the state the operator can actually see.
    api.putPreferences(currentDoc()).catch(() => undefined);
  }, SAVE_DEBOUNCE_MS);
}

/**
 * Record the Interfaces dock height: locally now, on the account shortly.
 *
 * ⚠️ This is the setter components call. Call it on **gesture end**, not per pointer event — see
 * `SAVE_DEBOUNCE_MS`. Keyboard steps are discrete and fine to send as they happen; the debounce
 * coalesces a held arrow key anyway.
 */
export function setInterfaceDockHeight(px: number): void {
  usePrefsStore.getState().setInterfaceDockHeight(px);
  scheduleSave();
}
