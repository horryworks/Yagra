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
import {
  adoptWidths,
  clearColumn,
  clearTable,
  setWidths,
  type ColumnWidthDoc,
  type TableColumnWidths,
} from './lib/columnWidths';
import type { TableId } from './lib/tableIds';

/** Coalesce a burst of adjustments into one write. Same value and same reason as the dashboard's
 *  (`dashboard/layoutStore.ts`): a drag emits a value per frame, and **every** PUT writes an audit
 *  row — the backend has no per-route opt-out, so debouncing here is a contract, not a nicety. */
const SAVE_DEBOUNCE_MS = 800;

/** The document this browser reads and writes. Every field optional: an older or newer WebUI may
 *  have written the row, so a missing key must read as "not set", never as an error. */
interface ServerPrefsDoc {
  /** Node-detail Interfaces chart dock height, px (issue #65). */
  interfaceDockHeight?: number;
  /** Table column widths, keyed by table id then by column key (ADR-129). */
  tableColumnWidths?: ColumnWidthDoc;
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
  if (doc.tableColumnWidths !== undefined) {
    // `adoptWidths` does the selecting — it is in a `.ts` beside its tests because "survive
    // anything" is a claim that needs examples, and the branch above is the shape that has none.
    usePrefsStore.getState().setTableColumnWidths(adoptWidths(doc.tableColumnWidths));
  }
}

/** The document to send: the account-scoped subset of `prefs.ts`. */
function currentDoc(): ServerPrefsDoc {
  const { interfaceDockHeight, tableColumnWidths } = usePrefsStore.getState();
  const doc: ServerPrefsDoc = {};
  if (interfaceDockHeight != null) doc.interfaceDockHeight = interfaceDockHeight;
  // Omitted while empty rather than sent as `{}`: the account row has a 16 KiB ceiling every
  // preference shares, and an operator who never drags a column should cost it nothing.
  if (tableColumnWidths && Object.keys(tableColumnWidths).length > 0) {
    doc.tableColumnWidths = tableColumnWidths;
  }
  return doc;
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

/** The document as it stands, for the three setters below. */
function widthDoc(): ColumnWidthDoc {
  return usePrefsStore.getState().tableColumnWidths ?? {};
}

/**
 * Record what one gesture did to a table's columns: locally now, on the account shortly (ADR-129).
 *
 * Takes the whole map rather than one column because a drag freezes every column at the width it
 * already had (`freezeTracks`) — that is one gesture, and it must be one write.
 *
 * ⚠️ Call it on **gesture end**, not per pointer event — see `SAVE_DEBOUNCE_MS`. Keyboard steps are
 * discrete and fine to send as they happen; the debounce coalesces a held arrow key anyway.
 */
export function mergeTableColumnWidths(tableId: TableId, widths: TableColumnWidths): void {
  usePrefsStore.getState().setTableColumnWidths(setWidths(widthDoc(), tableId, widths));
  scheduleSave();
}

/** Put one column back to the width its author declared (the grip's double-click). */
export function clearTableColumnWidth(tableId: TableId, columnKey: string): void {
  usePrefsStore.getState().setTableColumnWidths(clearColumn(widthDoc(), tableId, columnKey));
  scheduleSave();
}

/** Put every column of one table back (the reset control in its header row). */
export function clearTableColumnWidths(tableId: TableId): void {
  usePrefsStore.getState().setTableColumnWidths(clearTable(widthDoc(), tableId));
  scheduleSave();
}
