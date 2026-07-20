// SPDX-License-Identifier: AGPL-3.0-only
// URL-param codec for the Events page's node filter. The node_id filter lives in the URL (not
// local state) so a deep-link from a node — /alerts/events?node_id=<id> — lands pre-filtered and
// so the filter survives a reload. Pure + unit-tested, mirroring RangeControl's param helpers.

/** Read a node_id filter from URL params: a trimmed, non-empty id, or null when absent/blank. */
export function readNodeIdParam(params: URLSearchParams): string | null {
  const v = params.get('node_id')?.trim();
  return v ? v : null;
}

/** Write (or clear) the node_id filter: set it when present, delete the key when null so the URL
 *  stays clean (no dangling `?node_id=`). */
export function writeNodeIdParam(params: URLSearchParams, nodeId: string | null): void {
  if (nodeId) params.set('node_id', nodeId);
  else params.delete('node_id');
}
