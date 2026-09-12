// SPDX-License-Identifier: AGPL-3.0-only
// Tag many nodes at once, from the inventory tree's working set (ADR-135).
//
// 🚨 **This MERGES and the dialog has to say so.** The operator picked rows in the tree and knows
// one label they want on all of them; they have no idea what else each of those nodes carries.
// `POST /api/v1/nodes/tags` adds and removes only the keys it names — unlike the single-node Edit
// dialog, which replaces the whole map because it is *showing* the whole map. Saying "adds to what
// each node already has" in the dialog is what stops someone reading this as "set the tags to".
//
// The judgement (what a legal key is, what an empty row means) lives in `nodeEditForm.ts` beside
// the single-node editor's, because it is the same rule and the API applies one validator to both.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import type { NodeSummary } from '../../types/api';
import { Button } from '../ui/Button';
import { IconButton } from '../ui/IconButton';
import { Modal } from '../ui/Modal';
import { FieldHint, TextInput } from '../ui/Field';
import {
  tagRowProblem,
  tagsAreValid,
  tagsFromRows,
  TAGS_MAX,
  type TagRow,
} from '../NodeDetail/nodeEditForm';

export function BulkTagModal({
  targets,
  onClose,
  onDone,
}: {
  targets: NodeSummary[];
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [rows, setRows] = useState<TagRow[]>([{ key: '', value: '' }]);
  /** Keys to strip from every selected node. Separate from `rows` because removing is keyed by
   *  name alone — an operator taking `region` off a rack does not know, or care, what value each
   *  node had for it. */
  const [remove, setRemove] = useState('');
  const [error, setError] = useState<string | null>(null);
  // In flight. The call is idempotent, so a double click is harmless to the data — but it would
  // report twice and close on the first answer.
  const [busy, setBusy] = useState(false);

  const add = tagsFromRows(rows);
  const removeKeys = remove
    .split(',')
    .map((k) => k.trim())
    .filter((k) => k !== '');
  const nothingToDo = Object.keys(add).length === 0 && removeKeys.length === 0;

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .bulkTagNodes(
        targets.map((n) => n.id),
        add,
        removeKeys,
      )
      .then((r) => {
        // `applied < requested` is normal, not an error — a node can have been deleted, or lie
        // outside this caller's folder scope. Reporting both numbers rather than claiming the
        // count asked for is the same choice the bulk move makes (ADR-124 決定 7).
        if (r.applied < r.requested) {
          setError(t('bulkTag.partial', { applied: r.applied, requested: r.requested }));
          setBusy(false);
          return;
        }
        onDone();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('bulkTag.err')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('bulkTag.title', { count: targets.length })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button
            variant="primary"
            onClick={submit}
            disabled={busy || nothingToDo || !tagsAreValid(rows)}
          >
            {t('bulkTag.apply')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <p className="nd-muted">{t('bulkTag.note', { count: targets.length })}</p>

        <div className="modal-field nd-tags">
          <span className="modal-field-label">{t('bulkTag.add')}</span>
          {rows.map((row, i) => {
            const problem = tagRowProblem(row);
            return (
              <div className="nd-tag-row" key={i}>
                <TextInput
                  className="mono"
                  value={row.key}
                  placeholder={t('field.tagKeyPlaceholder')}
                  aria-label={t('field.tagKey')}
                  onChange={(e) =>
                    setRows((prev) =>
                      prev.map((r, j) => (j === i ? { ...r, key: e.target.value } : r)),
                    )
                  }
                />
                <span className="nd-tag-eq" aria-hidden="true">
                  =
                </span>
                <TextInput
                  value={row.value}
                  placeholder={t('field.tagValuePlaceholder')}
                  aria-label={t('field.tagValue')}
                  onChange={(e) =>
                    setRows((prev) =>
                      prev.map((r, j) => (j === i ? { ...r, value: e.target.value } : r)),
                    )
                  }
                />
                <IconButton
                  title={t('field.tagRemove', { key: row.key || t('field.tagKey') })}
                  onClick={() => setRows((prev) => prev.filter((_, j) => j !== i))}
                >
                  ✕
                </IconButton>
                {problem && <FieldHint error>{t(`field.tagErr.${problem}`)}</FieldHint>}
              </div>
            );
          })}
          <div className="nd-tag-add">
            <Button
              onClick={() => setRows((prev) => [...prev, { key: '', value: '' }])}
              disabled={rows.length >= TAGS_MAX}
            >
              ＋ {t('field.tagAdd')}
            </Button>
          </div>
        </div>

        <label className="form-label">
          {t('bulkTag.remove')}
          <TextInput
            className="mono"
            value={remove}
            onChange={(e) => setRemove(e.target.value)}
            placeholder={t('bulkTag.removePlaceholder')}
          />
          <span className="form-hint">{t('bulkTag.removeHint')}</span>
        </label>

        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
