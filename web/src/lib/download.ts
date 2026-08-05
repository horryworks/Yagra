// SPDX-License-Identifier: AGPL-3.0-only
// Saving a fetched attachment to the user's disk.
//
// Downloads in this app go through `fetch` rather than an anchor `href`, because an anchor sends no
// Authorization header and 401s on any auth-enabled deployment. That leaves the browser with no
// Content-Disposition to act on, so the filename has to be read out of the response and handed to
// `a.download` — which is what this module does.
//
// Reading the name from the header rather than rebuilding it client-side is deliberate: the server
// already decides what a support bundle or a report export is called, and a second implementation
// of that naming rule here would be a mirror with nothing pinning the two together.

/**
 * The filename from a `Content-Disposition` header, or `null` when there is nothing usable.
 *
 * Prefers RFC 5987's `filename*` (which can carry non-ASCII) over plain `filename`. Any directory
 * component is stripped: `a.download` ignores paths anyway, but a name is easier to reason about
 * when it cannot contain one.
 */
export function filenameFromDisposition(header: string | null): string | null {
  if (!header) return null;
  const extended = /filename\*\s*=\s*[^']*'[^']*'([^;]+)/i.exec(header);
  if (extended) {
    try {
      return sanitize(decodeURIComponent(extended[1].trim()));
    } catch {
      // A malformed percent-escape falls through to the plain form below rather than throwing —
      // losing the filename must never lose the download.
    }
  }
  const plain = /filename\s*=\s*"([^"]*)"|filename\s*=\s*([^;]+)/i.exec(header);
  const raw = plain?.[1] ?? plain?.[2];
  return raw ? sanitize(raw.trim()) : null;
}

/** Strip any path component and reject an empty or dot-only result. */
function sanitize(name: string): string | null {
  const base = name.split(/[/\\]/).pop()?.trim() ?? '';
  return base === '' || base === '.' || base === '..' ? null : base;
}

/**
 * Save a blob under `filename`, via a temporary object URL.
 *
 * The revoke is what stops the blob leaking for the life of the tab — a support bundle is tens of
 * megabytes, so an un-revoked URL is a real cost, not a tidiness point.
 */
export function saveBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}
