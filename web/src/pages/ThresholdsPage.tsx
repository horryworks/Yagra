// SPDX-License-Identifier: AGPL-3.0-only
// Alert rules (Alerts ▸ Alert rules). Thresholds resolve by hierarchical
// override — profile → group → node, most-specific wins (§3.3) — so each rule carries a
// scope level + id. CRUD against /thresholds. Rules are evaluated live: the alert engine
// snapshots them (refreshed every ~30s) and checks each matching poll sample through the
// same hysteresis/flapping machinery as liveness, so a breach fires a real alert.
//
// Data-table standard v2: a toolbar (count + "+ Add rule") over the shared `.ytable`; the
// add form and delete confirmation both go through modals. The blue-left-border note card
// above the toolbar keeps the "what a rule is" explainer in view.

import { useCallback, useEffect, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { Direction, ScopeLevel, StoredThreshold } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon } from '../components/ui/icons';
import './ThresholdsPage.css';

const LEVELS: ScopeLevel[] = ['profile', 'group', 'node'];
const DIRECTIONS: Direction[] = ['above', 'below'];
// Metrics the pollers emit today (ICMP every node, SNMP when a community is bound). Offered
// as presets so operators don't have to guess the exact series name; free text is still
// allowed for any other collected metric (e.g. snmp_oid_*).
const METRIC_PRESETS = ['icmp_rtt_ms', 'icmp_loss_pct', 'snmp_sys_uptime_ticks'];

const COLS = '1.6fr 1.4fr 110px 170px 100px 92px';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

/** Create a threshold rule (focused-editing modal). */
function AddThresholdModal({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation('alertsConfig');
  const [level, setLevel] = useState<ScopeLevel>('profile');
  const [scopeId, setScopeId] = useState('');
  const [metric, setMetric] = useState('');
  const [direction, setDirection] = useState<Direction>('above');
  const [warning, setWarning] = useState('');
  const [critical, setCritical] = useState('');
  const [dwell, setDwell] = useState('3');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const ready = metric.trim() !== '' && scopeId.trim() !== '';

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    const num = (s: string) => (s.trim() === '' ? undefined : Number(s));
    api
      .createThreshold({
        scope_level: level,
        scope_id: scopeId.trim(),
        metric: metric.trim(),
        direction,
        warning: num(warning),
        critical: num(critical),
        dwell_samples: num(dwell),
      })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('thresholds.err.add')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('thresholds.addModal.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {t('thresholds.addModal.add')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.scopeLevel')}</label>
        <Select value={level} onChange={(e) => setLevel(e.target.value as ScopeLevel)}>
          {LEVELS.map((l) => (
            <option key={l} value={l}>
              {t(`thresholds.scopeLevel.${l}`)}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.scopeId')}</label>
        <TextInput
          className="mono"
          placeholder={t(`thresholds.addModal.scopeIdPlaceholder.${level}`)}
          value={scopeId}
          onChange={(e) => setScopeId(e.target.value)}
          autoFocus
        />
        <span className="modal-hint">
          {t('thresholds.addModal.scopeIdHint', {
            noun: t(`thresholds.addModal.scopeIdNoun.${level}`),
          })}
        </span>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.metric')}</label>
        <TextInput
          className="mono"
          placeholder={t('thresholds.addModal.metricPlaceholder')}
          list="metric-presets"
          value={metric}
          onChange={(e) => setMetric(e.target.value)}
        />
        <datalist id="metric-presets">
          {METRIC_PRESETS.map((m) => (
            <option key={m} value={m} />
          ))}
        </datalist>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.direction')}</label>
        <Select value={direction} onChange={(e) => setDirection(e.target.value as Direction)}>
          {DIRECTIONS.map((d) => (
            <option key={d} value={d}>
              {t(`thresholds.direction.${d}`)}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.boundsDwell')}</label>
        <div className="thresholds-bounds">
          <TextInput
            className="thresholds-num"
            placeholder={t('thresholds.addModal.warnPlaceholder')}
            value={warning}
            onChange={(e) => setWarning(e.target.value)}
          />
          <TextInput
            className="thresholds-num"
            placeholder={t('thresholds.addModal.critPlaceholder')}
            value={critical}
            onChange={(e) => setCritical(e.target.value)}
          />
          <TextInput
            className="thresholds-num"
            placeholder={t('thresholds.addModal.dwellPlaceholder')}
            value={dwell}
            onChange={(e) => setDwell(e.target.value)}
            title={t('thresholds.addModal.dwellTitle')}
          />
        </div>
        <span className="modal-hint">{t('thresholds.addModal.boundsHint')}</span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Confirm + delete a threshold rule (destructive-consent modal). */
function DeleteThresholdModal({
  rule,
  onClose,
  onDone,
}: {
  rule: StoredThreshold;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteThreshold(rule.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('thresholds.err.delete')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('thresholds.deleteModal.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            {t('common:actions.delete')}
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        <Trans
          t={t}
          i18nKey="thresholds.deleteModal.body"
          values={{
            level: t(`thresholds.scopeLevel.${rule.scope_level}`),
            metric: rule.metric,
            scope: rule.scope_id,
          }}
          components={{ strong: <strong />, mono: <strong className="mono" /> }}
        />
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

export function ThresholdsPage() {
  const { t } = useTranslation('alertsConfig');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<StoredThreshold[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<StoredThreshold | null>(null);
  const { scopeName } = useEntityNames();

  const load = useCallback(() => {
    api
      .listThresholds()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.rules')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.rules') }]}
        note={t('thresholds.note')}
      />

      <Card className="thresholds-note-card">
        <p className="thresholds-note">{t('thresholds.explainer')}</p>
      </Card>

      {unavailable ? (
        <Card>
          <p className="muted">{t('thresholds.unavailable')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <TableSpacer />
            <ResultCount shown={rows.length} noun={t('common:noun.rule', { count: rows.length })} />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('thresholds.add')}
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <div className="ytable">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">{t('thresholds.cols.scope')}</div>
              <div className="ytable-h">{t('thresholds.cols.metric')}</div>
              <div className="ytable-h">{t('thresholds.cols.direction')}</div>
              <div className="ytable-h">{t('thresholds.cols.bounds')}</div>
              <div className="ytable-h">{t('thresholds.cols.dwell')}</div>
              <div className="ytable-h right">{t('thresholds.cols.actions')}</div>
            </div>

            {rows.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading ? t('common:loading') : t('thresholds.empty')}
                </p>
                {!loading && <p className="yt-empty-sub">{t('thresholds.emptySub')}</p>}
              </div>
            ) : (
              rows.map((row) => (
                <div className="ytable-row" style={{ gridTemplateColumns: COLS }} key={row.id}>
                  <div className="ytable-cell">
                    <Badge tone="neutral">{t(`thresholds.scopeLevel.${row.scope_level}`)}</Badge>
                    <EntityName name={scopeName(row.scope_level, row.scope_id)} id={row.scope_id} />
                  </div>
                  <div className="ytable-cell mono">{row.metric}</div>
                  <div className="ytable-cell muted">{t(`thresholds.direction.${row.direction}`)}</div>
                  <div className="ytable-cell">
                    <span className="thresholds-bounds">
                      {row.warning != null && (
                        <Badge tone="warning">
                          {t('thresholds.warnShort')} {row.warning}
                        </Badge>
                      )}
                      {row.critical != null && (
                        <Badge tone="critical">
                          {t('thresholds.critShort')} {row.critical}
                        </Badge>
                      )}
                    </span>
                  </div>
                  <div className="ytable-cell muted">
                    {t('thresholds.dwellValue', { n: row.dwell_samples })}
                  </div>
                  <div className="ytable-cell right">
                    {authed && (
                      <span className="ytable-actions">
                        <IconButton
                          title={t('common:actions.delete')}
                          danger
                          onClick={() => setDeleting(row)}
                        >
                          <TrashIcon />
                        </IconButton>
                      </span>
                    )}
                  </div>
                </div>
              ))
            )}
          </div>
        </>
      )}

      {adding && <AddThresholdModal onClose={() => setAdding(false)} onSaved={load} />}
      {deleting && (
        <DeleteThresholdModal
          rule={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            setError(null);
            load();
          }}
        />
      )}
    </div>
  );
}
