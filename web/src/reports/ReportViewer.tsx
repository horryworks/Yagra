// Report viewer — a full-bleed overlay showing a generated run. The report body is the server-
// rendered HTML (the same artifact the PDF is made from, so screen and PDF match), shown in a
// sandboxed iframe (no scripts execute; device strings are already escaped server-side). The
// toolbar exports the run (HTML/CSV/PDF) and prints. While a run is still generating it polls until
// the result is ready.

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '../components/ui/Button';
import { Badge } from '../components/ui/Badge';
import { Modal } from '../components/ui/Modal';
import { api, ApiError } from '../services/api';
import { formatTimestamp } from '../lib/format';
import type { ReportRunDetail } from '../types/api';
import './reports.css';

interface Props {
  runId: string;
  onClose: () => void;
}

export function ReportViewer({ runId, onClose }: Props) {
  const { t } = useTranslation('reports');
  const [detail, setDetail] = useState<ReportRunDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const iframeRef = useRef<HTMLIFrameElement>(null);

  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const load = () => {
      api
        .getReportRun(runId)
        .then((d) => {
          if (!alive) return;
          setDetail(d);
          // Still generating → poll until it reaches a terminal state.
          if (d.state === 'running' || d.state === 'queued') {
            timer = setTimeout(load, 2500);
          }
        })
        .catch((e: unknown) => {
          if (alive) setError(e instanceof ApiError ? e.message : t('viewer.err.loadFailed'));
        });
    };
    load();
    return () => {
      alive = false;
      if (timer) clearTimeout(timer);
    };
  }, [runId, t]);

  async function download(format: 'html' | 'csv' | 'pdf') {
    setBusy(format);
    setError(null);
    try {
      const blob = await api.exportReportRun(runId, format);
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `report-${runId}.${format}`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : t('viewer.err.exportFailed', { format: format.toUpperCase() }),
      );
    } finally {
      setBusy(null);
    }
  }

  function print() {
    iframeRef.current?.contentWindow?.print();
  }

  const ready = detail?.state === 'succeeded' && detail.result_html;

  const title = (
    <span className="rp-viewer-title-inline">
      {detail?.name ?? t('viewer.fallbackTitle')}
      {detail && (
        <span className="muted rp-viewer-meta">
          {' · '}
          {formatTimestamp(detail.created_ms)} · {t(`trigger.${detail.trigger}`)}
        </span>
      )}
    </span>
  );

  const footer = ready ? (
    <>
      <Button variant="ghost" disabled={busy != null} onClick={() => download('csv')}>
        {busy === 'csv' ? '…' : 'CSV'}
      </Button>
      <Button variant="ghost" disabled={busy != null} onClick={() => download('pdf')}>
        {busy === 'pdf' ? '…' : 'PDF'}
      </Button>
      <Button variant="ghost" disabled={busy != null} onClick={() => download('html')}>
        {busy === 'html' ? '…' : 'HTML'}
      </Button>
      <Button variant="ghost" onClick={print}>
        {t('viewer.print')}
      </Button>
    </>
  ) : undefined;

  return (
    <Modal title={title} onClose={onClose} footer={footer} size="wide">
      {error && <div className="rp-viewer-error">{error}</div>}
      <div className="rp-viewer-body">
        {!detail && !error && <div className="rp-viewer-state">{t('common:loading')}</div>}
        {detail && (detail.state === 'running' || detail.state === 'queued') && (
          <div className="rp-viewer-state">
            <div className="rp-progress">
              <div className="rp-progress-bar" style={{ width: `${detail.pct}%` }} />
            </div>
            <p className="muted">{t('viewer.generating', { pct: detail.pct })}</p>
          </div>
        )}
        {detail?.state === 'failed' && (
          <div className="rp-viewer-state">
            <Badge tone="critical">{t('viewer.failed')}</Badge>
            <p className="muted">{detail.error ?? t('viewer.genFailed')}</p>
          </div>
        )}
        {ready && (
          <iframe
            ref={iframeRef}
            className="rp-frame"
            title={detail?.name ?? t('viewer.fallbackTitle')}
            sandbox="allow-same-origin allow-modals"
            srcDoc={detail.result_html ?? ''}
          />
        )}
      </div>
    </Modal>
  );
}
