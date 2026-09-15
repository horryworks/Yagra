// SPDX-License-Identifier: AGPL-3.0-only
// The detail pane's pin toggle (ADR-146). It is the way to pin on a touch screen, which has no
// right-click; the tree's context menu is the other way. Drawn only once the account's pins have
// loaded — a core without the endpoint gets no button rather than one that presses into a 404.

import { useTranslation } from 'react-i18next';
import { usePinsStore } from '../../pinsStore';
import { Button } from '../ui/Button';
import { PinIcon } from '../ui/icons';
import './PinButton.css';

interface Props {
  kind: 'node' | 'group';
  id: string;
  /** The server refused. The mark is already back where it was; this says why. */
  onError: (e: unknown) => void;
}

export function PinButton({ kind, id, onError }: Props) {
  const { t } = useTranslation('nodes');
  const ready = usePinsStore((s) => s.status === 'ready');
  const pinned = usePinsStore((s) => (kind === 'node' ? s.nodeIds : s.groupIds).has(id));
  const setNodePinned = usePinsStore((s) => s.setNodePinned);
  const setGroupPinned = usePinsStore((s) => s.setGroupPinned);
  if (!ready) return null;
  const toggle = () => {
    const call = kind === 'node' ? setNodePinned(id, !pinned) : setGroupPinned(id, !pinned);
    call.catch(onError);
  };
  return (
    <Button
      variant="outline"
      className={pinned ? 'nd-pin on' : 'nd-pin'}
      aria-pressed={pinned}
      onClick={toggle}
    >
      <PinIcon width={14} height={14} />
      {pinned ? t('detail.unpin') : t('detail.pin')}
    </Button>
  );
}
