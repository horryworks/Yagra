// SPDX-License-Identifier: AGPL-3.0-only
// What an alert is about, rendered. One component because the branch had been written out three
// times — the triage rows, the history table and the flapping-watchlist widget — and the third copy
// was wrong, in the way `extensibility.md` predicts: the copies nobody reads during an incident are
// the copies that rot.
//
// The fault it removes is not cosmetic. An alert's `node` field is a node's UUID **or**
// `pool:<name>` (see `lib/alertSubject`), and the widget read it without asking which. That put
// `pool:tokyo` on screen styled as an unresolvable reference — a poller-pool outage reading as a
// broken row — and fed the same string to the node-name resolver, where it fails to parse as a UUID
// and takes the entire batch's other ids down with it.

import type { HasSubject } from '../lib/alertSubject';
import { alertSubject } from '../lib/alertSubject';
import { useTranslation } from 'react-i18next';
import { EntityName } from '../components/ui/EntityName';

/** Renders the alert's subject: a node's resolved name (UUID on hover), or the poller pool the
 *  alert is about. `nodeName` is the caller's `useEntityNames()` resolver, threaded in so the
 *  visible window still batches into one request. */
export function AlertSubjectName({
  alert,
  nodeName,
}: {
  alert: HasSubject;
  nodeName: (id: string) => string;
}) {
  const { t } = useTranslation('alerts');
  const subject = alertSubject(alert);
  if (subject.kind === 'node') {
    return <EntityName name={nodeName(subject.nodeId)} id={subject.nodeId} />;
  }
  // A pool name is already the human-readable thing — there is no inventory row to resolve it
  // through, and the label is what tells an operator this row is about Yagra's own polling rather
  // than about a device.
  return (
    <span title={t('row.poolSubjectHint')}>{t('row.poolSubject', { pool: subject.name })}</span>
  );
}
