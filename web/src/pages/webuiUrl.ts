// SPDX-License-Identifier: AGPL-3.0-only
// Settings > System settings > Notification links (ADR-260): what the field shows and what Save
// sends. The server normalizes and validates the address (`api/webui_url.rs::normalize`); this only
// turns the field into the body and decides whether anything changed.

import type { WebuiUrl } from '../types/api';

/** The field's text for a stored address: empty when none is set. */
export function webuiUrlDraft(stored: WebuiUrl | null): string {
  return stored?.url ?? '';
}

/** The body Save sends. An empty field clears the setting, which the server reads from `null`. */
export function webuiUrlBody(draft: string): WebuiUrl {
  const url = draft.trim();
  return { url: url === '' ? null : url };
}

/** Whether Save would change anything. Compared after trimming, as the server stores it. */
export function isWebuiUrlDirty(stored: WebuiUrl | null, draft: string): boolean {
  return webuiUrlBody(draft).url !== (stored?.url ?? null);
}

/** The example the empty field shows: the address this page was opened at, which is what most
 *  deployments will want to type. */
export function webuiUrlPlaceholder(origin: string): string {
  return origin.replace(/\/+$/, '');
}
