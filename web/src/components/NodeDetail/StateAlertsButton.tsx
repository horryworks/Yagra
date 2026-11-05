// SPDX-License-Identifier: AGPL-3.0-only
// The state badge beside a node's name. While the node has alerts it is a button that opens them,
// on whichever tab the operator is (ADR-238); with none it stays the plain badge it always was.

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { stateColorVar, stateLabel } from '../../lib/format';
import type { NodeState } from '../../types/api';
import { AnchoredPopover } from '../ui/AnchoredPopover';
import { focusPopoverTrigger } from '../ui/focusPopoverTrigger';
import { ActiveAlertList, type NodeAlert } from './ActiveAlertList';
import { alertJumpOf, offersJump, type AlertJump } from './alertJump';
import { NODE_DETAIL_TAB_META, type NodeDetailTab } from './tabs';

export function StateAlertsButton({
  state,
  alerts,
  activeTab,
  onJump,
}: {
  state: NodeState;
  alerts: readonly NodeAlert[];
  activeTab: NodeDetailTab;
  onJump: (jump: AlertJump) => void;
}) {
  const { t } = useTranslation('nodes');
  const wrapRef = useRef<HTMLSpanElement>(null);
  const [open, setOpen] = useState(false);
  const shut = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) focusPopoverTrigger(wrapRef.current, 'dialog');
  }, []);

  // The last alert cleared while the list was open: close it, or it would reopen by itself when
  // the next alert arrives.
  useEffect(() => {
    if (alerts.length === 0) setOpen(false);
  }, [alerts.length]);

  const color = { color: stateColorVar(state) };
  if (alerts.length === 0) {
    return (
      <span className="nd-statepill" style={color}>
        <span className="nd-statepill-dot" />
        <span className="nd-statepill-label">{stateLabel(state)}</span>
      </span>
    );
  }

  return (
    <span className="nd-statewrap" ref={wrapRef}>
      <button
        type="button"
        className="nd-statepill nd-statepill-btn"
        style={color}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="nd-statepill-dot" />
        <span className="nd-statepill-label">{stateLabel(state)}</span>
        <span className="nd-statepill-n">{alerts.length}</span>
        <span className="nd-statepill-chev" aria-hidden>
          ▾
        </span>
      </button>
      <AnchoredPopover
        open={open}
        anchorRef={wrapRef}
        role="dialog"
        label={t('nav:alerts.active')}
        align="start"
        className="nd-alertpop"
        onDismiss={shut}
      >
        <ActiveAlertList
          alerts={alerts}
          action={(a) => {
            const jump = alertJumpOf(a);
            if (!offersJump(jump, activeTab)) return null;
            return (
              <button
                type="button"
                className="nd-alert-jump"
                onClick={() => {
                  setOpen(false);
                  onJump(jump);
                }}
              >
                {t(NODE_DETAIL_TAB_META[jump.tab].labelKey)} ›
              </button>
            );
          }}
        />
      </AnchoredPopover>
    </span>
  );
}
