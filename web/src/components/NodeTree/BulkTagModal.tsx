// SPDX-License-Identifier: AGPL-3.0-only
// Tag many nodes at once, from the inventory tree's working set (ADR-135).
//
// 🚨 **This MERGES and the dialog has to say so.** The operator picked rows in the tree and knows
// one label they want on all of them; they have no idea what else each of those nodes carries.
// `POST /api/v1/nodes/tags` adds and removes only the labels it names — unlike the single-node
// Edit dialog, which replaces the whole list because it is *showing* the whole list. Saying "adds
// to what each node already has" in the dialog is what stops someone reading this as "set the
// tags to".
//
// ⚠️ **This is now the second way to label several nodes, and the weaker one.** Since ADR-135
// inc. 2 a folder carries labels that every node beneath it inherits, which keeps applying to
// nodes discovered later — something a copy onto today's rows cannot do. This dialog is for the
// case a folder cannot express: a handful of nodes spread across folders.
//
// The judgement lives in `ui/labelRules.ts`, shared with every other label control.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import type { NodeSummary } from '../../types/api';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { ChipInput } from '../ui/ChipInput';
import { labelsAreValid } from '../ui/labelRules';

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
  const [add, setAdd] = useState<string[]>([]);
  /** Labels to strip from every selected node. A separate list from `add` because the two are
   *  independent instructions, and because this one is `lenient`: a label already stored may
   *  predate the rules a new one follows, and refusing to let it be typed would make exactly the
   *  labels somebody wants gone impossible to name. */
  const [remove, setRemove] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  // In flight. The call is idempotent, so a double click is harmless to the data — but it would
  // report twice and close on the first answer.
  const [busy, setBusy] = useState(false);

  const nothingToDo = add.length === 0 && remove.length === 0;

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .bulkTagNodes(
        targets.map((n) => n.id),
        add,
        remove,
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
            disabled={busy || nothingToDo || !labelsAreValid(add)}
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
          <ChipInput
            value={add}
            onChange={setAdd}
            placeholder={t('field.tagPlaceholder')}
            inputLabel={t('bulkTag.add')}
          />
        </div>

        <div className="modal-field nd-tags">
          <span className="modal-field-label">{t('bulkTag.remove')}</span>
          <ChipInput
            value={remove}
            onChange={setRemove}
            placeholder={t('bulkTag.removePlaceholder')}
            inputLabel={t('bulkTag.remove')}
            lenient
          />
          <span className="form-hint">{t('bulkTag.removeHint')}</span>
        </div>

        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
