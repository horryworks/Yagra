// SPDX-License-Identifier: AGPL-3.0-only
// The site's local time and delivery state at the end of a header's sub line (ADR-252): the folder
// pane and the node pane draw the same parts. What to show is `lib/localTime.ts`'s to decide.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { localTimeLine, type DeliveryNow } from '../../lib/localTime';
// The screen's refresh cadence, as the top-bar clock uses: a minute display redrawn at it is never
// more than that late.
import { POLL_INTERVAL_MS } from '../../lib/sharedPoll';
import { Inline } from '../ui/Page';
import { StatusDot } from '../ui/StatusDot';

interface Props {
  zone: string | null | undefined;
  delivery: DeliveryNow | null | undefined;
  /** The delivery answer has run past its own boundary: re-read whatever holds it. */
  onStale?: () => void;
}

export function LocalTimeParts({ zone, delivery, onStale }: Props) {
  const { t, i18n } = useTranslation('nodes');
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), POLL_INTERVAL_MS);
    return () => window.clearInterval(id);
  }, []);
  const line = localTimeLine(zone, delivery, now, t, i18n.language);
  // Once per answer that went out of date; a re-read that brings the same answer asks no more.
  useEffect(() => {
    if (line.stale) onStale?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- on the transition only
  }, [line.stale, delivery]);

  return (
    <>
      {line.clock && (
        <>
          <span className="nd-sep">·</span>
          {/* Named "Local time", as NetBox's Site page names it, so it is not read as the zone the
              rest of the screen - charts included - is shown in (ADR-252 decision 10). */}
          <span>{t('localTime.clock', line.clock)}</span>
        </>
      )}
      {line.delivery && (
        <>
          <span className="nd-sep">·</span>
          <Inline as="span" gap={6}>
            <StatusDot state={line.delivery.open ? 'ok' : 'unknown'} withLabel={false} />
            <span>{line.delivery.text}</span>
          </Inline>
        </>
      )}
    </>
  );
}
