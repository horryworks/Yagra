// SPDX-License-Identifier: AGPL-3.0-only
// Resolve a referenced entity (node / group / profile / threshold scope) to its human name for
// table cells. Policy (design-system §4.1 / ui-conventions): the visible primary is ALWAYS the
// name — the raw UUID is reachable only through the hover tooltip, never as the cell's main text.
// There is deliberately no copy button beside the name (ADR-241): nobody copies a node's id out of
// a list, and a column whose content IS an id uses `CopyableId` instead.
// Falls back to the raw id when the name can't be resolved (e.g. a deleted reference). Defined
// once here so every list reuses the same treatment instead of hand-rolling per-page resolvers.
// The resolvers and the `useEntityNames` hook are in `entityNames.ts`; this file renders.

import { Link } from 'react-router-dom';
import { isEntityResolved } from './entityNames';

/** Render a resolved entity name; the raw id is available only in the hover tooltip. When the
 *  name couldn't be resolved (it equals the id, or no id was supplied) the raw handle is shown in
 *  mono so a UUID reads as an id rather than prose.
 *
 *  `to` makes the name a link to the entity's own page (ADR-209 decision 2). An unresolved
 *  reference gets no link — it usually means the entity is gone. */
export function EntityName({ name, id, to }: { name: string; id?: string; to?: string }) {
  if (!isEntityResolved(name, id)) {
    const raw = name || id || '—';
    return (
      <span className="yt-entity-raw mono" title={raw}>
        {raw}
      </span>
    );
  }
  return (
    <span className="yt-entity" title={id}>
      {to ? (
        <Link className="yt-entity-name" to={to}>
          {name}
        </Link>
      ) : (
        <span className="yt-entity-name">{name}</span>
      )}
    </span>
  );
}
