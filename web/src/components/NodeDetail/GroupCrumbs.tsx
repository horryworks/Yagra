// SPDX-License-Identifier: AGPL-3.0-only
// A folder path drawn as segments the operator can open (ADR-142). Shared by the node header's
// eyebrow and the group pane's title, so the two cannot disagree about which segment is a link or
// how the separator is spelled.

import { Fragment } from 'react';
import type { GroupCrumb } from '../../lib/nodeTree';

interface Props {
  trail: GroupCrumb[];
  /** Open one folder. Absent ⇒ every segment is plain text: a segment that looks pressable and does
   *  nothing is worse than one that does not look pressable at all. */
  onOpenGroup?: (groupId: string) => void;
  /** Whether the last segment is a link. The node header says yes — its last folder is the node's
   *  parent, not what is open. The group pane says no — its last segment IS the open pane. */
  linkLast: boolean;
}

export function GroupCrumbs({ trail, onOpenGroup, linkLast }: Props) {
  return (
    <span className="nd-crumbs">
      {trail.map((seg, i) => {
        const last = i === trail.length - 1;
        return (
          <Fragment key={seg.id}>
            {i > 0 && (
              <span className="nd-crumb-sep" aria-hidden="true">
                {' / '}
              </span>
            )}
            {onOpenGroup && (!last || linkLast) ? (
              <button type="button" className="nd-crumb" onClick={() => onOpenGroup(seg.id)}>
                {seg.name}
              </button>
            ) : (
              <span>{seg.name}</span>
            )}
          </Fragment>
        );
      })}
    </span>
  );
}
