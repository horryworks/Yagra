// SPDX-License-Identifier: AGPL-3.0-only
// Is this error "a code-split chunk could not be loaded" (ADR-212)?
//
// The usual cause is an upgrade: the browser still holds the previous index.html, asks for a
// hashed chunk the new image no longer has, and nginx answers 404 (`location /assets/`). A dropped
// VPN looks the same from here. Either way the answer is a reload, which is all
// `ChunkErrorBoundary` offers — so this only has to recognise the failure, not explain it.
//
// Each engine words it differently, and none sets a distinguishing `name` (the dynamic import
// rejects with a plain TypeError), so the message is what there is to match.

const CHUNK_LOAD_MESSAGES: readonly RegExp[] = [
  /Failed to fetch dynamically imported module/i, // Chromium
  /error loading dynamically imported module/i, // Firefox
  /Importing a module script failed/i, // Safari
  /Unable to preload CSS/i, // Vite's preload helper, for a chunk's stylesheet
  /Expected a JavaScript.* MIME type/i, // an HTML page served where a module was asked for
];

/** True when `err` says a lazily loaded chunk (or its CSS) could not be fetched. */
export function isChunkLoadError(err: unknown): boolean {
  if (!(err instanceof Error)) return false;
  if (err.name === 'ChunkLoadError') return true;
  return CHUNK_LOAD_MESSAGES.some((re) => re.test(err.message));
}
