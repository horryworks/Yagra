// SPDX-License-Identifier: AGPL-3.0-only
// System settings (Settings ▸ System settings). System-wide monitoring defaults persisted
// server-side via /config. Currently: the global default polling interval (seconds). Per-profile
// overrides (Nodes ▸ Device profiles) take precedence over this default. ManageConfig-gated — the
// input is disabled (and a hint shown) without it; the scheduler re-reads the value each round so
// a change applies without a restart.

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput } from '../components/ui/Field';
import './SystemSettingsPage.css';

// Mirror the backend bounds (yagra-core config.rs MIN/MAX_POLL_INTERVAL_SECS); the server
// re-validates and is authoritative.
const MIN = 10;
const MAX = 3600;

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

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
    </div>
  );
}
