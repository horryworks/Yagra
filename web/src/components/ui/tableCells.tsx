// Reusable table-cell content for the data-table standard (styles in styles/table.css).
// These encode the cell vocabulary the design defines once and every list reuses: sealed
// secrets, copyable mono ids, HTTP status, method chips, two-line timestamps, monograms.

import { useState } from 'react';
import {
  formatExactTime,
  httpStatusLabel,
  httpStatusTone,
  initials,
  relativeTime,
} from '../../lib/format';
import { IconButton } from './IconButton';
import { CopyIcon, LockIcon } from './icons';

/** Sealed-secret indicator — secret values are never shown or returned by the API. */
export function SealedSecret() {
  return (
    <span className="yt-sealed" title="Encrypted at rest — never displayed">
      <LockIcon />
      <span className="yt-dots">••••••</span>
    </span>
  );
}

/** Mono id, truncated, with a copy button that appears on row hover and flashes "Copied". */
export function CopyableId({ id }: { id: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(id);
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };
  return (
    <span className="yt-copy">
      <span className="yt-copy-id">{id}</span>
      <IconButton
        className="yt-copy-btn"
        title={copied ? 'Copied' : 'Copy id'}
        onClick={copy}
      >
        <CopyIcon />
      </IconButton>
    </span>
  );
}

/** HTTP status: a dot + mono code + short label, toned on the status palette (2xx/4xx/5xx). */
export function HttpStatus({ status }: { status: number }) {
  const tone = httpStatusTone(status);
  return (
    <span className={`yt-http ${tone}`}>
      <span className="yt-http-dot" />
      <span className="yt-http-code">{status}</span>
      <span className="yt-http-label">{httpStatusLabel(status)}</span>
    </span>
  );
}

/** Neutral mono chip for an HTTP method / pseudo-action (categorical — never tone-colored). */
export function MethodChip({ label }: { label: string }) {
  return <span className="yt-method">{label}</span>;
}

/** Two-line timestamp: exact (mono, primary) + relative (muted). For forensic/log screens. */
export function TimeCell({ iso }: { iso: string }) {
  return (
    <span className="yt-time">
      <span className="yt-time-exact">{formatExactTime(iso)}</span>
      <span className="yt-time-rel">{relativeTime(iso)}</span>
    </span>
  );
}

/** Monogram avatar. `role` colors the ring (categorical); `system` dashes it (unknown user). */
export function Monogram({
  name,
  role,
  lg,
  system,
}: {
  name: string;
  role?: string;
  lg?: boolean;
  system?: boolean;
}) {
  const cls = ['yt-avatar', lg ? 'lg' : '', role ? `role-${role}` : '', system ? 'system' : '']
    .filter(Boolean)
    .join(' ');
  return (
    <span className={cls} title={role ?? name}>
      <span className="yt-avatar-txt">{initials(name)}</span>
    </span>
  );
}
