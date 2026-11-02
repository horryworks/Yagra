// SPDX-License-Identifier: AGPL-3.0-only
// The one sentence the shell says when nothing the caller can see is being polled (ADR-227): every
// pool-polled node sits in a pool with no live poller. Shown at once — the pool coverage alert
// waits five minutes before it pages, and the screens in between used to read calm.

import { Trans } from 'react-i18next';
import { useCan } from '../../store';
import { useFleetWatch } from '../../hooks/useFleetWatch';
import { nothingPolled } from '../../lib/watchCounts';
import { ScreenLink } from '../ui/ScreenLink';
import './NothingPolledBanner.css';

export function NothingPolledBanner() {
  // An anonymous visitor has nothing to read, and asking would only answer 401.
  if (!useCan('view')) return null;
  return <Watching />;
}

function Watching() {
  if (!nothingPolled(useFleetWatch())) return null;
  return (
    <p className="nothing-polled" role="status">
      <Trans
        i18nKey="common:watch.nothingPolled"
        components={{ lnk: <ScreenLink to="/settings/pollers" /> }}
      />
    </p>
  );
}
