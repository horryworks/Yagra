// SPDX-License-Identifier: AGPL-3.0-only
// Where config backups are pushed (ADR-210 stage 4): the judgement behind Settings ▸ Integrations ▸
// Git, the catalogue tile and the line above Nodes ▸ Config backups.
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`): what the last push means, which chip
// the tile wears, and what the form may send are all here.

import type { GitAuth, PushFailure, PushStatusView } from '../../types/api';
import type { Chip } from './chip';

/** The page's route; the alert row and the Config backups line link here. */
export const GIT_REMOTE_PATH = '/settings/integrations/git';

/** How the last push went, as one line: its i18n key and what it interpolates. */
export type PushLine =
  | { key: 'git.push.none' }
  | { key: 'git.push.waiting' }
  | { key: 'git.push.ok'; at: string; head: string }
  | { key: 'git.push.failed'; at: string; failure: PushFailure };

/** The last push, in the order an operator needs it: a failure outranks an earlier success. */
export function pushLine(s: PushStatusView): PushLine {
  if (!s.configured) return { key: 'git.push.none' };
  if (s.failure && s.failed_at) return { key: 'git.push.failed', at: s.failed_at, failure: s.failure };
  if (s.pushed_at && s.pushed_head) {
    return { key: 'git.push.ok', at: s.pushed_at, head: s.pushed_head.slice(0, 7) };
  }
  return { key: 'git.push.waiting' };
}

/** The catalogue tile's chip. */
export function gitRemoteChip(s: PushStatusView): Chip {
  const line = pushLine(s);
  switch (line.key) {
    case 'git.push.none':
      return { labelKey: 'integrations.status.notConfigured', tone: 'idle' };
    case 'git.push.waiting':
      return { labelKey: 'git.status.waiting', tone: 'paused' };
    case 'git.push.ok':
      return { labelKey: 'git.status.ok', tone: 'ok' };
    case 'git.push.failed':
      return { labelKey: 'git.status.failed', tone: 'muted' };
  }
}

/** The way a URL is reached, read from its shape: `https://` takes a token, anything else a key.
 *  `null` for a URL the server would refuse anyway (it says so on save). */
export function authForUrl(url: string): GitAuth | null {
  const u = url.trim();
  if (u.startsWith('https://')) return 'https_token';
  if (u.startsWith('ssh://') || /^[^\s/@:]+@[^\s/:]+:\S+$/.test(u)) return 'ssh_key';
  return null;
}

/** The scheme and authority of an HTTPS URL, lower-cased; `null` for any other. The server keeps a
 *  sealed token only while this stays the same (`config_remote.rs::https_origin`). */
export function httpsOrigin(url: string): string | null {
  const u = url.trim();
  if (!u.startsWith('https://')) return null;
  return `https://${u.slice('https://'.length).split('/')[0].toLowerCase()}`;
}

/** Whether the stored token may be kept for `url`: there is one, and the URL still names the host
 *  and port it was given for. */
export function storedTokenUsable(
  storedUrl: string | null,
  storedAuth: string | null,
  url: string,
): boolean {
  if (storedAuth !== 'https_token' || !storedUrl) return false;
  const was = httpsOrigin(storedUrl);
  return was !== null && was === httpsOrigin(url);
}

/** The form a person fills in. */
export interface GitRemoteForm {
  url: string;
  branch: string;
  username: string;
  /** Empty means "keep the sealed token". */
  token: string;
}

/** The PUT body for `form`. `null` while the URL's shape says nothing about how it is reached. */
export function remoteBody(form: GitRemoteForm): {
  url: string;
  branch: string | null;
  auth: GitAuth;
  username?: string;
  token?: string;
} | null {
  const auth = authForUrl(form.url);
  if (!auth) return null;
  const branch = form.branch.trim() === '' ? null : form.branch.trim();
  if (auth === 'ssh_key') return { url: form.url.trim(), branch, auth };
  const token = form.token.trim();
  return {
    url: form.url.trim(),
    branch,
    auth,
    username: form.username.trim(),
    ...(token === '' ? {} : { token }),
  };
}

/** Whether Save can be pressed: a URL of a known shape, and for a token a user name and — unless
 *  one is already sealed for a token destination — the token. */
export function canSaveRemote(form: GitRemoteForm, tokenStored: boolean): boolean {
  const auth = authForUrl(form.url);
  if (!auth) return false;
  if (auth === 'ssh_key') return true;
  return form.username.trim() !== '' && (tokenStored || form.token.trim() !== '');
}
