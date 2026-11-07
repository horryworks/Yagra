// SPDX-License-Identifier: AGPL-3.0-only
// The notice a list screen shows *instead of* its toolbar and table, decided from its `useLoad`
// (ADR-056, ADR-184).
//
// `LoadBlockNotice` says what to show; this is the "instead of". Twenty screens spelled the
// either/or out as `{block ? <LoadBlockNotice …/> : <>…</>}` by hand, and that is the half that
// goes wrong when it goes wrong — a notice rendered *above* the table keeps "0 credentials" on
// screen and every write control mounted. Wrapping the list in this makes that shape unwritable.
//
// `signedIn={false}` is the third "instead of": nobody is signed in, so the screen asks for that
// with the one shared sentence. Five list screens had written that notice themselves, three of
// them in their own words (ADR-246 Inc.5).
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import type { LoadState } from '../../lib/loadState';
import type { Permission } from '../../types/api';
import { Card } from './Card';
import { LoadBlockNotice } from './LoadBlockNotice';

interface Props {
  load: Pick<LoadState<unknown>, 'block'>;
  /** False while nobody is signed in: the shared "sign in" notice replaces the list. */
  signedIn?: boolean;
  /** What to say in skeleton mode, when the shared sentence is not enough. See `LoadBlockNotice`. */
  unavailable?: string;
  /** The privilege the screen's read needs, named in the refusal. See `LoadBlockNotice`. */
  permission?: Permission;
  /** A screen's own refusal sentence, where it already says something better. */
  forbidden?: string;
  children: ReactNode;
}

export function LoadGate({ load, signedIn, unavailable, permission, forbidden, children }: Props) {
  const { t } = useTranslation('common');
  if (signedIn === false) {
    return (
      <Card>
        <p className="muted">{t('loadBlock.signIn')}</p>
      </Card>
    );
  }
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
