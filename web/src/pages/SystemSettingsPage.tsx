// SPDX-License-Identifier: AGPL-3.0-only
// System settings (Settings ▸ System settings). System-wide monitoring defaults persisted
// server-side via /config. Currently: the global default polling interval (seconds), and the data
// retention policy (ADR-040). Per-profile overrides (Nodes ▸ Device profiles) take precedence over
// the polling default. ManageConfig-gated — inputs are disabled (and a hint shown) without it; the
// scheduler and the prune loops re-read their values each round so a change applies without a
// restart.
//
// The retention card shows every retained subject, not only the ones Yagra can change. Two rows are
// owned by a store's own start flag (VictoriaMetrics / VictoriaLogs `-retentionPeriod`), which has
// no runtime API — those render read-only with the value the store itself reported, and name the
// knob that does change them. Judgement lives in `retentionSettings.ts` so it can be tested.

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useAuthStore } from '../store';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput } from '../components/ui/Field';
import type { NeighborConfig, RetentionPolicy, RetentionRow } from '../types/api';
import {
  describeCadence,
  isNeighborDirty,
  neighborFormFrom,
  parseNeighborForm,
  MAX_NEIGHBOR_INTERVAL_SECS,
  MIN_NEIGHBOR_INTERVAL_SECS,
  type NeighborForm,
} from './neighborSettings';
import {
  bandFor,
  formFromValues,
  isDirty,
  parseRetentionForm,
  rowField,
  rowMode,
  storeValue,
  type RetentionForm,
} from './retentionSettings';
import './SystemSettingsPage.css';

// Mirror the backend bounds (yagra-core config.rs MIN/MAX_POLL_INTERVAL_SECS); the server
// re-validates and is authoritative.
const MIN = 10;
const MAX = 3600;

export function SystemSettingsPage() {
  const { t } = useTranslation('system');
  const authed = useAuthStore((s) => s.authed);
  const [value, setValue] = useState('');
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const load = useCallback(() => {
    api
      .getConfig()
      .then((cfg) => setValue(String(cfg.default_poll_interval_secs)))
      .catch(() => undefined)
      .finally(() => setLoaded(true));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const save = () => {
    const n = Number(value.trim());
    if (!Number.isInteger(n) || n < MIN || n > MAX) {
      setError(t('settings.err.range', { min: MIN, max: MAX }));
      setSaved(false);
      return;
    }
    setBusy(true);
    setError(null);
    setSaved(false);
    api
      .updateConfig({ default_poll_interval_secs: n })
      .then(() => setSaved(true))
      .catch((e: unknown) => setError(errMsg(e, t('settings.err.save'))))
      .finally(() => setBusy(false));
  };

  return (
    <div>
      <PageHeader
        title={t('nav:settings.system')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.system') }]}
        note={t('settings.note')}
      />
      <Card title={t('settings.polling.title')}>
        <div className="sys-setting">
          <div className="sys-setting-label">
            <div className="sys-setting-name">{t('settings.polling.intervalName')}</div>
            <div className="sys-setting-help muted">
              {t('settings.polling.intervalHelp', { min: MIN, max: MAX })}
            </div>
          </div>
          <div className="sys-setting-control">
            <TextInput
              className="sys-setting-input"
              value={value}
              onChange={(e) => {
                setValue(e.target.value);
                setSaved(false);
              }}
              inputMode="numeric"
              disabled={!authed || !loaded || busy}
              aria-label={t('settings.polling.intervalAria')}
            />
            <span className="sys-setting-unit muted">{t('settings.polling.seconds')}</span>
            {authed && (
              <Button variant="primary" onClick={save} disabled={busy || !loaded}>
                {t('common:actions.save')}
              </Button>
            )}
          </div>
        </div>
        {!authed && <p className="muted">{t('settings.signInHint')}</p>}
        {error && <p className="form-error">{error}</p>}
        {saved && <p className="sys-setting-saved">{t('settings.saved')}</p>}
      </Card>
      <NeighborCard authed={authed} />
      <RetentionCard authed={authed} />
    </div>
  );
}

/** Settings ▸ System settings ▸ Neighbor discovery (ADR-038).
 *
 *  Deployment-wide rather than per node or per profile: the OIDs are fixed standards (LLDP-MIB /
 *  CISCO-CDP-MIB), so there is nothing to tune per device — only whether to walk and how often. */
function NeighborCard({ authed }: { authed: boolean }) {
  const { t } = useTranslation('system');
  const [saved, setSavedCfg] = useState<NeighborConfig | null>(null);
  const [form, setForm] = useState<NeighborForm | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);

  const load = useCallback(() => {
    api
      .getNeighborSettings()
      .then((cfg) => {
        setSavedCfg(cfg);
        setForm(neighborFormFrom(cfg));
      })
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const save = () => {
    if (!form) return;
    const parsed = parseNeighborForm(form, {
      min: saved?.min_interval_secs,
      max: saved?.max_interval_secs,
    });
    if (!parsed.ok) {
      setError(t('settings.neighbors.err.range', { min: parsed.min, max: parsed.max }));
      setOk(false);
      return;
    }
    setBusy(true);
    setError(null);
    setOk(false);
    api
      .setNeighborSettings(parsed.values)
      .then(() => {
        setOk(true);
        load();
      })
      .catch((e: unknown) => setError(errMsg(e, t('settings.neighbors.err.save'))))
      .finally(() => setBusy(false));
  };

  const dirty = saved && form ? isNeighborDirty(form, saved) : false;

  return (
    <Card title={t('settings.neighbors.title')}>
      <p className="sys-setting-help muted">{t('settings.neighbors.note')}</p>
      <div className="sys-setting">
        <div className="sys-setting-label">
          <div className="sys-setting-name">{t('settings.neighbors.enabledName')}</div>
          <div className="sys-setting-help muted">{t('settings.neighbors.enabledHelp')}</div>
        </div>
        <div className="sys-setting-control">
          <label className="sys-setting-toggle">
            <input
              type="checkbox"
              checked={form?.enabled ?? false}
              disabled={!authed || !form || busy}
              onChange={(e) => {
                setForm((f) => (f ? { ...f, enabled: e.target.checked } : f));
                setOk(false);
              }}
            />
            <span>
              {form?.enabled
                ? t('settings.neighbors.enabledOn')
                : t('settings.neighbors.enabledOff')}
            </span>
          </label>
        </div>
      </div>
      <div className="sys-setting">
        <div className="sys-setting-label">
          <div className="sys-setting-name">{t('settings.neighbors.intervalName')}</div>
          <div className="sys-setting-help muted">
            {t('settings.neighbors.intervalHelp', {
              cadence: describeCadence(Number(form?.intervalSecs ?? 0), t),
            })}
          </div>
        </div>
        <div className="sys-setting-control">
          <TextInput
            className="sys-setting-input"
            value={form?.intervalSecs ?? ''}
            onChange={(e) => {
              const v = e.target.value;
              setForm((f) => (f ? { ...f, intervalSecs: v } : f));
              setOk(false);
            }}
            inputMode="numeric"
            disabled={!authed || !form || busy}
            aria-label={t('settings.neighbors.intervalName')}
          />
          <span className="sys-setting-unit muted">{t('settings.polling.seconds')}</span>
          <span className="sys-setting-band muted">
            {t('settings.retention.band', {
              min: saved?.min_interval_secs ?? MIN_NEIGHBOR_INTERVAL_SECS,
              max: saved?.max_interval_secs ?? MAX_NEIGHBOR_INTERVAL_SECS,
            })}
          </span>
        </div>
      </div>
      {authed && (
        <div className="sys-setting-actions">
          <Button variant="primary" onClick={save} disabled={busy || !form || !dirty}>
            {t('common:actions.save')}
          </Button>
        </div>
      )}
      {!authed && <p className="muted">{t('settings.signInHint')}</p>}
      {error && <p className="form-error">{error}</p>}
      {ok && <p className="sys-setting-saved">{t('settings.saved')}</p>}
    </Card>
  );
}

/** Settings ▸ System settings ▸ Data retention (ADR-040). */
function RetentionCard({ authed }: { authed: boolean }) {
  const { t } = useTranslation('system');
  const [policy, setPolicy] = useState<RetentionPolicy | null>(null);
  const [form, setForm] = useState<RetentionForm | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const load = useCallback(() => {
    api
      .getRetention()
      .then((p) => {
        setPolicy(p);
        setForm(formFromValues(p.settings));
      })
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const save = () => {
    if (!form) return;
    const parsed = parseRetentionForm(form);
    if (!parsed.ok) {
      setError(
        t('settings.retention.err.range', {
          name: t(`settings.retention.field.${parsed.field}`),
          min: parsed.min,
          max: parsed.max,
        }),
      );
      setSaved(false);
      return;
    }
    setBusy(true);
    setError(null);
    setSaved(false);
    api
      .updateRetention(parsed.values)
      .then(() => {
        setSaved(true);
        // Re-read rather than assume: the flow row's TTL is applied by the server, and a store-owned
        // row could have changed underneath us.
        load();
      })
      .catch((e: unknown) => setError(errMsg(e, t('settings.retention.err.save'))))
      .finally(() => setBusy(false));
  };

  const dirty = policy && form ? isDirty(form, policy.settings) : false;

  return (
    <Card title={t('settings.retention.title')}>
      <p className="sys-setting-help muted">{t('settings.retention.note')}</p>
      {(policy?.rows ?? []).map((row) => (
        <RetentionRowView
          key={row.subject}
          row={row}
          form={form}
          disabled={!authed || !form || busy}
          onChange={(field, v) => {
            setForm((f) => (f ? { ...f, [field]: v } : f));
            setSaved(false);
          }}
        />
      ))}
      {authed && (
        <div className="sys-setting-actions">
          <Button variant="primary" onClick={save} disabled={busy || !form || !dirty}>
            {t('common:actions.save')}
          </Button>
        </div>
      )}
      {!authed && <p className="muted">{t('settings.signInHint')}</p>}
      {error && <p className="form-error">{error}</p>}
      {saved && <p className="sys-setting-saved">{t('settings.saved')}</p>}
    </Card>
  );
}

/** One row of the retention table: an input, a read-only store value, or "kept indefinitely". */
function RetentionRowView({
  row,
  form,
  disabled,
  onChange,
}: {
  row: RetentionRow;
  form: RetentionForm | null;
  disabled: boolean;
  onChange: (field: keyof RetentionForm, value: string) => void;
}) {
  const { t } = useTranslation('system');
  const mode = rowMode(row);
  const field = rowField(row);
  // A row from a newer core whose `tunable` this build does not know is skipped rather than guessed
  // at — offering an input that writes nowhere is worse than omitting the line.
  if (mode === 'unknown') return null;

  const name = t(`settings.retention.subject.${row.subject}`, { defaultValue: row.subject });

  let control: React.ReactNode;
  if (mode === 'editable' && field && form) {
    const [min, max] = bandFor(field);
    control = (
      <>
        <TextInput
          className="sys-setting-input"
          value={form[field]}
          onChange={(e) => onChange(field, e.target.value)}
          inputMode="numeric"
          disabled={disabled}
          aria-label={name}
        />
        <span className="sys-setting-unit muted">
          {t(`settings.retention.unit.${row.unit || 'days'}`)}
        </span>
        <span className="sys-setting-band muted">{t('settings.retention.band', { min, max })}</span>
      </>
    );
  } else if (mode === 'store-owned') {
    const v = storeValue(row);
    control = (
      <span className="sys-setting-readonly muted">
        {v.kind === 'reported'
          ? t('settings.retention.storeValue', { value: v.value })
          : v.kind === 'not-configured'
            ? t('settings.retention.notConfigured')
            : t('settings.retention.storeUnknown')}
      </span>
    );
  } else {
    control = (
      <span className="sys-setting-readonly muted">{t('settings.retention.indefinite')}</span>
    );
  }

  return (
    <div className="sys-setting">
      <div className="sys-setting-label">
        <div className="sys-setting-name">{name}</div>
        <div className="sys-setting-help muted">
          {row.store} — {row.note}
        </div>
      </div>
      <div className="sys-setting-control">{control}</div>
    </div>
  );
}
