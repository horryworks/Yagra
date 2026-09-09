// SPDX-License-Identifier: AGPL-3.0-only
// Move nodes into a folder (or ungroup them). A focused-edit modal shared by the inventory tree
// (context menu / "Move…" picker), the selection bar, and the node-detail header, so the
// assign-to-folder flow is identical wherever it is reached.
//
// It takes a **list** since ADR-124, and one node is simply a list of one — rather than a second
// `MoveNodesModal` beside this one, which is how fifteen copies of the delete dialog happened.
// The single request path is the same too: even one node goes through `moveNodes`, so there is one
// answer to "what happens when you move something" and one place a mistake in it can live.
//
// ⚠️ **A partial result does not close the dialog.** `moved < requested` means some ids named
// nodes that are gone (or outside this token's scope), and closing on it would report success for
// work that did not happen. The tree is refreshed either way — what is on screen is true — but the
// operator is told, and dismisses it themselves.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import type { NodeGroup, NodeSummary } from '../../types/api';
import { groupOptions } from '../../lib/nodeTree';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { GroupPicker } from '../ui/GroupPicker';
import './MoveNodeModal.css';

export function MoveNodeModal({
  targets,
  groups,
  onClose,
  onMoved,
}: {
  /** The nodes to move. One is the ordinary case; the selection bar passes many. */
  targets: readonly NodeSummary[];
  groups: NodeGroup[];
  onClose: () => void;
  /** Refresh the inventory. **Does not close** — this dialog decides that itself. */
  onMoved: () => void;
}) {
  const { t } = useTranslation('nodes');
  // Preselect the folder they are already in, but only when they agree: offering one node's folder
  // as the destination for twelve would make the safest-looking choice a no-op for eleven of them.
  const shared = targets.every((n) => (n.group_id ?? '') === (targets[0]?.group_id ?? ''))
    ? (targets[0]?.group_id ?? '')
    : '';
  const [target, setTarget] = useState<string>(shared);
  const [error, setError] = useState<string | null>(null);
  const [partial, setPartial] = useState<{ requested: number; moved: number } | null>(null);
  const [busy, setBusy] = useState(false);

  const save = () => {
    setBusy(true);
    setError(null);
    setPartial(null);
    api
      .moveNodes(
        targets.map((n) => n.id),
        target || null,
      )
      .then((r) => {
        onMoved();
        if (r.moved < r.requested) {
          setPartial(r);
          setBusy(false);
        } else {
          onClose();
        }
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('err.moveNode')));
        setBusy(false);
      });
  };

  const one = targets.length === 1 ? targets[0] : null;

  return (
    <Modal
      title={
        one ? t('moveNode.title', { name: one.name }) : t('moveNode.titleMany', { count: targets.length })
      }
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {partial ? t('common:actions.close') : t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={busy || targets.length === 0}>
            {t('moveNode.move')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        {!one && (
          <div className="movenode-targets">
            <ul className="movenode-list scroll-y">
              {targets.map((n) => (
                <li key={n.id}>{n.name}</li>
              ))}
            </ul>
          </div>
        )}
        <label className="form-label" htmlFor="movenode-group">
          {t('field.group')}
          <GroupPicker
            id="movenode-group"
            options={groupOptions(groups)}
            value={target}
            onChange={setTarget}
            emptyOption={t('moveNode.ungroupedOption')}
            disabled={busy}
          />
        </label>
        {partial && (
          <p className="form-error">
            {t('moveNode.partial', { moved: partial.moved, requested: partial.requested })}
          </p>
        )}
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
