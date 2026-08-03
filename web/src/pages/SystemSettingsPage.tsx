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
import type { RetentionPolicy, RetentionRow } from '../types/api';
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
      <RetentionCard authed={authed} />
    </div>
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
