// SPDX-License-Identifier: AGPL-3.0-only
// The pane a screen shows *instead of* its toolbar and table when it could not load (ADR-056).
//
// "Instead of" is the whole design. Rendering a forbidden notice *above* an empty table would keep
// the lie on screen — the operator would read "0 credentials" underneath the explanation — and it
// would leave every write control mounted. Replacing the pane is what makes the enabled
// `+ Add credential` button on a Viewer's screen disappear without adding a permission check to
// each button, which is the copy-per-button shape ADR-056 decision 2 refuses.
//
// `unavailable` text stays per-screen (it names the feature: "Credential management is unavailable
// in skeleton mode…"). `forbidden` is one shared sentence, optionally naming the privilege the
// screen needs — the `403` body does not say which one, so the name comes from the server's own
// catalogue (`GET /api/v1/roles`) via `usePermissionLabel`, never from a list kept here. Telling
// the operator which privilege to ask for is the point: "no permission" leaves them and their
// administrator guessing at which of seven.
import { useTranslation } from 'react-i18next';
import { Card } from './Card';
import type { LoadBlock } from '../../lib/loadState';
import type { Permission } from '../../types/api';
import { usePermissionLabel } from '../../store';

interface Props {
  block: LoadBlock;
  /** What this screen calls itself when the deployment has no admin state. Required: only the
   *  screen knows which feature is missing. */
  unavailable: string;
  /** The privilege this screen's read requires — the same one its `useCan` asks for. Named in the
   *  refusal so the operator knows what to request. Omit only where the screen genuinely cannot
   *  say which one the server wanted. */
  permission?: Permission;
  /** Optional override for the refusal sentence, for screens that already say something better. */
  forbidden?: string;
}

export function LoadBlockNotice({ block, unavailable, permission, forbidden }: Props) {
  const { t } = useTranslation('common');
  const label = usePermissionLabel(permission ?? 'view');
  const refused =
    forbidden ??
    (permission ? t('loadBlock.forbiddenPermission', { permission: label }) : t('loadBlock.forbidden'));
  return (
    <Card>
      <p className="muted">{block === 'unavailable' ? unavailable : refused}</p>
    </Card>
  );
}
