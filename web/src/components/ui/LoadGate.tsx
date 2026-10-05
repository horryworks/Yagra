// SPDX-License-Identifier: AGPL-3.0-only
// The notice a list screen shows *instead of* its toolbar and table, decided from its `useLoad`
// (ADR-056, ADR-184).
//
// `LoadBlockNotice` says what to show; this is the "instead of". Twenty screens spelled the
// either/or out as `{block ? <LoadBlockNotice …/> : <>…</>}` by hand, and that is the half that
// goes wrong when it goes wrong — a notice rendered *above* the table keeps "0 credentials" on
// screen and every write control mounted. Wrapping the list in this makes that shape unwritable.
import type { ReactNode } from 'react';
import type { LoadState } from '../../lib/loadState';
import type { Permission } from '../../types/api';
import { LoadBlockNotice } from './LoadBlockNotice';

interface Props {
  load: Pick<LoadState<unknown>, 'block'>;
  /** What to say in skeleton mode, when the shared sentence is not enough. See `LoadBlockNotice`. */
  unavailable?: string;
  /** The privilege the screen's read needs, named in the refusal. See `LoadBlockNotice`. */
  permission?: Permission;
  /** A screen's own refusal sentence, where it already says something better. */
  forbidden?: string;
  children: ReactNode;
}

export function LoadGate({ load, unavailable, permission, forbidden, children }: Props) {
  if (load.block) {
    return (
      <LoadBlockNotice
        block={load.block}
        unavailable={unavailable}
        permission={permission}
        forbidden={forbidden}
      />
    );
  }
  return <>{children}</>;
}
