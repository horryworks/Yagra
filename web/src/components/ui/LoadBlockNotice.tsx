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
// in skeleton mode…"). `forbidden` defaults to one shared sentence, because the `403` body does not
// say which permission was required — a screen cannot honestly be more specific than the server
// was. `forbidden` is overridable for the screens that already wrote something better, and ADR-056
// Increment 2 replaces the default with the permission's own name.
import { useTranslation } from 'react-i18next';
import { Card } from './Card';
import type { LoadBlock } from '../../lib/loadState';

interface Props {
  block: LoadBlock;
  /** What this screen calls itself when the deployment has no admin state. Required: only the
   *  screen knows which feature is missing. */
  unavailable: string;
  /** Optional override for the permission refusal; defaults to the shared sentence. */
  forbidden?: string;
}

export function LoadBlockNotice({ block, unavailable, forbidden }: Props) {
  const { t } = useTranslation('common');
  return (
    <Card>
      <p className="muted">
        {block === 'unavailable' ? unavailable : (forbidden ?? t('loadBlock.forbidden'))}
      </p>
    </Card>
  );
}
