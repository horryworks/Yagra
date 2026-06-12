// Audit log (Settings ▸ Audit log). Admin-only: every mutating API call (who, what,
// when, response status) plus login events. Newest first with keyset "load more"
// pagination — the log is append-only and can grow without bound.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { AuditRow } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Badge } from '../components/ui/Badge';
import './AuditPage.css';

const PAGE_SIZE = 100;

const fmtTime = (iso: string) =>
  new Date(iso).toLocaleString(undefined, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });

function statusTone(status: number): 'up' | 'warning' | 'critical' {
  if (status < 300) return 'up';
  if (status < 500) return 'warning';
  return 'critical';
}

export function AuditPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<AuditRow[]>([]);
  const [exhausted, setExhausted] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadFirst = useCallback(() => {
    setError(null);
    api
      .listAudit({ limit: PAGE_SIZE })
      .then((page) => {
        setRows(page);
        setExhausted(page.length < PAGE_SIZE);
      })
      .catch((e: unknown) =>
        setError(e instanceof ApiError ? e.message : 'failed to load the audit log'),
      );
  }, []);

  useEffect(() => {
    if (authed) loadFirst();
  }, [authed, loadFirst]);

  const loadMore = () => {
    const last = rows[rows.length - 1];
    if (!last) return;
    api
      .listAudit({ limit: PAGE_SIZE, before: last.at })
      .then((page) => {
        setRows((cur) => [...cur, ...page]);
        setExhausted(page.length < PAGE_SIZE);
      })
      .catch((e: unknown) =>
        setError(e instanceof ApiError ? e.message : 'failed to load more'),
      );
  };

  return (
    <div>
      <PageHeader
        title="Audit log"
        trail={[{ label: 'Settings' }, { label: 'Audit log' }]}
        note="Every configuration change and login event: who, what, when."
      />

      <Card title="Entries">
        {!authed ? (
          <p className="muted">Sign in as an admin to view the audit log.</p>
        ) : (
          <>
            {error && <p className="form-error">{error}</p>}
            {rows.length === 0 ? (
              <p className="muted">No audit entries yet.</p>
            ) : (
              <div className="audit-table">
                <div className="audit-head">
                  <span className="audit-h">Time</span>
                  <span className="audit-h">User</span>
                  <span className="audit-h">Action</span>
                  <span className="audit-h">Status</span>
                </div>
                {rows.map((r) => (
                  <div className="audit-row" key={r.id}>
                    <span className="mono audit-time">{fmtTime(r.at)}</span>
                    <span className="audit-user">{r.username}</span>
                    <span className="mono audit-action">{r.action}</span>
                    <Badge tone={statusTone(r.status)}>{r.status}</Badge>
                  </div>
                ))}
              </div>
            )}
            {!exhausted && rows.length > 0 && (
              <div className="audit-more">
                <Button variant="ghost" onClick={loadMore}>
                  Load older entries
                </Button>
              </div>
            )}
          </>
        )}
      </Card>
    </div>
  );
}
