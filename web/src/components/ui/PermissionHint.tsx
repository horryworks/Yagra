// SPDX-License-Identifier: AGPL-3.0-only
// The one line that goes where a write control would have been, on the screens where removing it
// outright would leave a titled panel with nothing in it (ADR-056 Inc.2).
//
// Controls the caller cannot use are *not drawn* — that is the rule, and it needs no text. This is
// for the other case: a `Card` whose entire body is the action (Configuration bundle's export and
// import panels, System settings' three forms). An empty card reads as a broken screen, so it says
// which of the two reasons applies instead:
//
//   - not signed in     → the screen's own "sign in to…" sentence, unchanged from before
//   - signed in, denied → the privilege it needs, named from the server's own catalogue
//
// Those two used to be one message. A signed-in Viewer was told to sign in — advice that cannot
// work, because they already had.
import { useTranslation } from 'react-i18next';
import type { Permission } from '../../types/api';
import { useAuthStore, usePermissionLabel } from '../../store';

interface Props {
  /** The privilege the hidden control needs. */
  permission: Permission;
  /** What this screen says to someone who is not signed in at all. */
  signInHint: string;
}

export function PermissionHint({ permission, signInHint }: Props) {
  const { t } = useTranslation('common');
  const authed = useAuthStore((s) => s.authed);
  const label = usePermissionLabel(permission);
  return (
    <p className="muted">
      {authed ? t('loadBlock.forbiddenPermission', { permission: label }) : signInHint}
    </p>
  );
}
