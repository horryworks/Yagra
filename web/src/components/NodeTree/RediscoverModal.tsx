// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Rediscover (ADR-186): re-read one monitored node with its own credential, show what the
// node holds beside what the device now says, and write only the rows the person ticks.
//
// The judgement — which phase to show, which rows may be applied, what Apply sends — is in
// `rediscoverState.ts`, where a test runs. This file starts the re-read, asks again every two
// seconds while it is in flight, and draws the answer.
import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ApiError, api, errMsg } from '../../services/api';
import type { RediscoverView } from '../../types/api';
import { pollWhileVisible } from '../../lib/sharedPoll';
import { done } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { Modal } from '../ui/Modal';
import { FormError, FormFooter } from '../ui/FormFooter';
import {
  applicable,
  applyBody,
  keepPolling,
  phaseOf,
  REDISCOVER_POLL_MS,
  refusalKey,
  rereadOn,
  rowsOf,
  type RediscoverField,
} from './rediscoverState';
import './RediscoverModal.css';

export function RediscoverModal({
  nodeId,
  name,
  onClose,
  onApplied,
}: {
  nodeId: string;
  name: string;
  onClose: () => void;
  /** Something was written: re-read the tree and the detail pane. */
  onApplied: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [scanId, setScanId] = useState<string | null>(null);
  const [startError, setStartError] = useState<string | null>(null);
  const [view, setView] = useState<RediscoverView | null>(null);
  const [lost, setLost] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [chosen, setChosen] = useState<Set<RediscoverField> | null>(null);
  const startedAt = useRef(0);

  const words = (e: unknown): string | null => {
    const key = refusalKey(e instanceof ApiError ? e.code : undefined);
    return key ? t(key) : null;
  };

  // Start once. StrictMode mounts twice in development; the ref keeps that to one re-read.
  const started = useRef(false);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    api.startRediscovery(nodeId).then(
      (r) => {
        startedAt.current = Date.now();
        setScanId(r.scan_id);
      },
      (e: unknown) => setStartError(words(e) ?? errMsg(e, t('rediscover.err.start'))),
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nodeId]);

  const phase = phaseOf(view, lost, elapsed);
  const polling = scanId !== null && keepPolling(phase);

  useEffect(() => {
    if (!scanId || !polling) return;
    let alive = true;
    const read = () => {
      setElapsed(Date.now() - startedAt.current);
      api.getRediscovery(nodeId, scanId).then(
        (v) => {
          if (alive) setView(v);
        },
        (e: unknown) => {
          // Gone from the core: say so and stop. Anything else is a hiccup; the next tick retries.
          if (alive && e instanceof ApiError && e.status === 404) setLost(true);
        },
      );
    };
    read();
    const stop = pollWhileVisible(read, REDISCOVER_POLL_MS);
    return () => {
      alive = false;
      stop();
    };
  }, [nodeId, scanId, polling]);

  const comparison = phase === 'answered' ? (view?.comparison ?? null) : null;
  const canApply = useMemo(() => (comparison ? applicable(comparison) : []), [comparison]);
  // Every applicable row starts ticked: the person came here to fix the node.
  const ticked = chosen ?? new Set(canApply);
  const body = comparison && scanId ? applyBody(scanId, comparison, ticked) : null;

  const form = useSubmit({
    errorFallback: t('rediscover.err.apply'),
    describeError: words,
    onDone: onApplied,
  });
  const apply = () => {
    if (!body) return;
    const scan = body.scan_id;
    form.submit(() =>
      api.applyRediscovery(nodeId, body).then(
        () => done(),
        (e: unknown) => {
          // Stale comparison: read it again (the server re-compares, nothing is re-scanned) so the
          // table and Apply reflect the node as it is now. The refusal still shows as the error.
          if (e instanceof ApiError && rereadOn(e.code)) {
            api.getRediscovery(nodeId, scan).then(
              (v) => {
                setView(v);
                setChosen(null);
              },
              (err: unknown) => {
                if (err instanceof ApiError && err.status === 404) setLost(true);
              },
            );
          }
          throw e;
        },
      ),
    );
  };

  const toggle = (f: RediscoverField) => {
    const next = new Set(ticked);
    if (next.has(f)) next.delete(f);
    else next.add(f);
    setChosen(next);
  };

  const status = (() => {
    switch (phase) {
      case 'starting':
        return startError ? null : t('rediscover.phase.starting');
      case 'waiting':
        return t('rediscover.phase.waiting');
      case 'waitingLong':
        return t('rediscover.phase.waitingLong');
      case 'reading':
        return t('rediscover.phase.reading');
      case 'answered':
        return canApply.length === 0 ? t('rediscover.phase.nothingToApply') : null;
      case 'noSnmpAnswer':
        return t('rediscover.phase.noSnmpAnswer');
      case 'noAnswer':
        return t('rediscover.phase.noAnswer');
      case 'stopped':
        return t('rediscover.phase.stopped');
      case 'lost':
        return t('rediscover.err.lost');
      default: {
        const unknown: never = phase;
        void unknown;
        return null;
      }
    }
  })();

  const verdictLabel = {
    same: t('rediscover.verdict.same'),
    differs: t('rediscover.verdict.differs'),
    undetermined: t('rediscover.verdict.undetermined'),
    locked: t('rediscover.verdict.locked'),
  } as const;
  const fieldLabel = {
    profile: t('rediscover.field.profile'),
    vendor: t('rediscover.field.vendor'),
    model: t('rediscover.field.model'),
  } as const;

  return (
    <Modal
      title={t('rediscover.title', { name })}
      onClose={onClose}
      size="wide"
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={apply}
          submitLabel={t('rediscover.apply')}
          canSubmit={body !== null}
        />
      }
    >
      <div className="form-stack">
        <p className="muted">{t('rediscover.intro')}</p>
        {startError && (
          <p className="form-error" role="alert">
            {startError}
          </p>
        )}
        {status && <p className={polling ? 'muted' : 'form-warning'}>{status}</p>}
        {comparison && (
          <>
            {comparison.sys_descr && (
              <p className="rediscover-descr mono" title={comparison.sys_descr}>
                {comparison.sys_descr}
              </p>
            )}
            <div className="rediscover-grid" role="table">
              <div className="rediscover-head" role="row">
                <span role="columnheader" />
                <span role="columnheader">{t('rediscover.col.field')}</span>
                <span role="columnheader">{t('rediscover.col.current')}</span>
                <span role="columnheader">{t('rediscover.col.found')}</span>
                <span role="columnheader">{t('rediscover.col.verdict')}</span>
              </div>
              {rowsOf(comparison).map((r) => {
                const can = canApply.includes(r.field);
                return (
                  <label key={r.field} className="rediscover-row" role="row">
                    <input
                      type="checkbox"
                      checked={can && ticked.has(r.field)}
                      disabled={!can || form.busy}
                      onChange={() => toggle(r.field)}
                      aria-label={fieldLabel[r.field]}
                    />
                    <span role="cell">{fieldLabel[r.field]}</span>
                    <span role="cell" title={r.current ?? ''}>
                      {r.current ?? '—'}
                    </span>
                    <span role="cell" title={r.found ?? ''}>
                      {r.found ?? '—'}
                    </span>
                    <span role="cell" className={`rediscover-verdict is-${r.verdict}`}>
                      {verdictLabel[r.verdict]}
                    </span>
                  </label>
                );
              })}
            </div>
            <p className="muted">{t('rediscover.applyHint')}</p>
          </>
        )}
        <FormError form={form} />
      </div>
    </Modal>
  );
}
