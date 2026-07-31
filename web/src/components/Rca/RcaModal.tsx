// SPDX-License-Identifier: AGPL-3.0-only
// "Explain this incident" (ADR-029): asks the configured language model to read the evidence Yagra
// already assembled and write the sentence a human would write at the end — likely cause, whether
// the other alerts are consequences of it, and what to look at next.
//
// Two display rules are not negotiable here:
//   1. The evidence the model was given is shown alongside the answer, always. An explanation
//      without its evidence is an assertion, and the reader has to be able to check it.
//   2. Model output is rendered as TEXT. No `dangerouslySetInnerHTML`, no markdown→HTML. Device
//      output is untrusted (security.md) and the model was itself reading device output, so its
//      reply inherits that status.
//
// Generation is AckAlerts-gated and rate-limited server-side; a repeat click for unchanged evidence
// is served from the store (`cached`) rather than billed again.

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import i18n from '../../i18n';
import { api } from '../../services/api';
import { formatExactTime, formatTimestamp, LIVENESS_METRIC } from '../../lib/format';
import type { RcaEvidence, RcaReport, RcaReportBody } from '../../types/api';
import { formatWindow, nodeLine, refusalText } from './rcaText';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import './RcaModal.css';

interface Props {
  /** The alerting node the operator clicked. The backend hops to its root cause if there is one. */
  node: string;
  check: string;
  onClose: () => void;
}

/** The evidence the model was given, rendered so every claim above can be checked against it. */
function Evidence({ ev }: { ev: RcaEvidence }) {
  const { t } = useTranslation('rca');
  const alert = ev.alert;
  return (
    <div className="rca-evidence">
      <div className="rca-ev-block">
        <div className="rca-ev-label">{t('evidence.node')}</div>
        <div className="rca-ev-value mono">{nodeLine(ev.node)}</div>
        {alert.asked_about && (
          <div className="rca-ev-note">{t('evidence.hopped', { symptom: alert.asked_about })}</div>
        )}
      </div>

      <div className="rca-ev-block">
        <div className="rca-ev-label">{t('evidence.alert')}</div>
        <div className="rca-ev-value">
          {alert.metric === LIVENESS_METRIC || alert.metric === ''
            ? t('evidence.alertLineLiveness', {
                severity: alert.severity,
                at: formatTimestamp(alert.at_unix_ms),
              })
            : t('evidence.alertLine', {
                severity: alert.severity,
                metric: alert.metric,
                at: formatTimestamp(alert.at_unix_ms),
              })}
          {alert.flapping && <span className="rca-flap">{t('evidence.flapping')}</span>}
        </div>
        {alert.breach && (
          <div className="rca-ev-note mono">
            {t('evidence.breach', {
              value: alert.breach.value,
              threshold: alert.breach.threshold ?? '—',
              direction: alert.breach.direction,
            })}
          </div>
        )}
      </div>

      {ev.dependents.total > 0 && (
        <div className="rca-ev-block">
          <div className="rca-ev-label">
            {t('evidence.dependents')} ({ev.dependents.total})
          </div>
          <div className="rca-ev-value mono">{ev.dependents.named.join(', ')}</div>
          {ev.dependents.total > ev.dependents.named.length && (
            <div className="rca-ev-note">
              {t('evidence.dependentsMore', {
                count: ev.dependents.total - ev.dependents.named.length,
              })}
            </div>
          )}
        </div>
      )}

      {ev.upstream.length > 0 && (
        <div className="rca-ev-block">
          <div className="rca-ev-label">{t('evidence.upstream')}</div>
          <div className="rca-ev-value mono">{ev.upstream.map((n) => n.name).join(' → ')}</div>
        </div>
      )}

      <div className="rca-ev-block">
        <div className="rca-ev-label">
          {t('evidence.timeline', { window: formatWindow(ev.window_secs) })}
        </div>
        {ev.timeline.length === 0 ? (
          <div className="rca-ev-note">{t('evidence.timelineEmpty')}</div>
        ) : (
          <ul className="rca-timeline">
            {ev.timeline.map((s, i) => (
              <li key={`${s.at_s}-${s.kind}-${i}`}>
                <span className="rca-tl-time mono">{formatTimestamp(s.at_s * 1000)}</span>
                <span className="rca-tl-kind">{s.kind}</span>
                <span className="rca-tl-label">{s.label}</span>
              </li>
            ))}
          </ul>
        )}
      </div>

      <div className="rca-ev-block">
        <div className="rca-ev-label">{t('evidence.changes')}</div>
        {ev.recent_changes.length === 0 ? (
          <div className="rca-ev-note">{t('evidence.changesEmpty')}</div>
        ) : (
          <ul className="rca-timeline">
            {ev.recent_changes.map((c, i) => (
              <li key={`${c.at}-${i}`}>
                <span className="rca-tl-time mono">{formatExactTime(c.at)}</span>
                <span className="rca-tl-kind">{c.username}</span>
                <span className="rca-tl-label mono">{c.action}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

export function RcaModal({ node, check, onClose }: Props) {
  const { t } = useTranslation('rca');
  const [report, setReport] = useState<RcaReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const generate = useCallback(
    (force: boolean) => {
      setLoading(true);
      setError(null);
      api
        // The instructions stay English server-side; only the answer follows the reader's language.
        .createRca({ node, check, language: i18n.language, force })
        .then(setReport)
        .catch((e: unknown) => setError(refusalText(e, t)))
        .finally(() => setLoading(false));
    },
    [node, check, t],
  );

  useEffect(() => {
    generate(false);
  }, [generate]);

  // `body` is an opaque `serde_json::Value` on the wire — the backend stores and returns the
  // document without interpreting it, so `RcaReportBody` is the WebUI's reading of it.
  const body = report ? (report.body as RcaReportBody) : undefined;
  const answer = body?.answer;
  const evidence = body?.evidence;
  const confidence = answer?.confidence ?? 'unknown';

  return (
    <Modal
      title={t('title')}
      size="wide"
      onClose={onClose}
      footer={
        <>
          <Button
            variant="outline"
            onClick={() => generate(true)}
            disabled={loading}
            title={t('actions.regenerateHint')}
          >
            {t('actions.regenerate')}
          </Button>
          <Button variant="primary" onClick={onClose}>
            {t('common:actions.close')}
          </Button>
        </>
      }
    >
      {/* Shown before, during and after generation — the caveat is not a footnote to a result. */}
      <p className="rca-disclaimer">{t('disclaimer')}</p>

      {loading && <p className="rca-loading muted">{t('loading')}</p>}
      {error && <p className="form-error">{error}</p>}

      {!loading && report && answer && (
        <>
          <p className="rca-summary">{answer.summary}</p>
          <div className="rca-meta">
            <span className={`rca-conf ${confidence}`}>{t(`confidence.${confidence}`)}</span>
            {report.cached && (
              <span className="rca-cached" title={t('meta.cachedHint')}>
                {t('meta.cached')}
              </span>
            )}
            <span className="muted">
              {t('meta.model', { provider: report.provider, model: report.model })}
            </span>
            <span className="muted">
              {t('meta.generated', {
                at: formatExactTime(report.generated_at),
                by: report.created_by,
              })}
            </span>
          </div>

          {answer.raw ? (
            // The model did not answer in the contracted shape. Showing its text as written beats
            // rendering four empty sections and pretending it did.
            <section className="rca-section">
              <h3 className="rca-h">{t('section.raw')}</h3>
              <p className="rca-note muted">{t('rawNote')}</p>
              <pre className="rca-raw">{answer.raw}</pre>
            </section>
          ) : (
            <>
              {answer.root_cause && (
                <section className="rca-section">
                  <h3 className="rca-h">{t('section.rootCause')}</h3>
                  <p className="rca-text">{answer.root_cause}</p>
                </section>
              )}
              {answer.dependents && (
                <section className="rca-section">
                  <h3 className="rca-h">{t('section.dependents')}</h3>
                  <p className="rca-text">{answer.dependents}</p>
                </section>
              )}
              {answer.next_steps.length > 0 && (
                <section className="rca-section">
                  <h3 className="rca-h">{t('section.nextSteps')}</h3>
                  <ol className="rca-steps">
                    {answer.next_steps.map((s, i) => (
                      <li key={i}>{s}</li>
                    ))}
                  </ol>
                </section>
              )}
            </>
          )}

          <section className="rca-section">
            <h3 className="rca-h">{t('section.evidence')}</h3>
            {evidence ? (
              <Evidence ev={evidence} />
            ) : (
              <p className="rca-note muted">{t('evidence.noneRecorded')}</p>
            )}
          </section>
        </>
      )}
    </Modal>
  );
}
