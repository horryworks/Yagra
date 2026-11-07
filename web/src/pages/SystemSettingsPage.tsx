// SPDX-License-Identifier: AGPL-3.0-only
// System settings (Settings ▸ System settings). System-wide monitoring defaults persisted
// server-side via /config. Currently: the global default polling interval (seconds), and the data
// retention policy (ADR-040). Per-profile overrides (Nodes ▸ Device profiles) take precedence over
// the polling default. ManageConfig for the polling and discovery cards, ManageSystem for
// retention (ADR-057) — inputs are disabled (and a hint shown) without it; the
// scheduler and the prune loops re-read their values each round so a change applies without a
// restart. A retention save that shortens a window is confirmed first: it deletes data on the next
// sweep (ADR-200 moved that warning from a card note to the moment it applies).
//
// The retention card shows every retained subject, not only the ones Yagra can change. Two rows are
// owned by a store's own start flag (VictoriaMetrics / VictoriaLogs `-retentionPeriod`), which has
// no runtime API — those render read-only with the value the store itself reported, and name the
// knob that does change them. Judgement lives in `retentionSettings.ts` so it can be tested.

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import { PageHeader } from '../components/ui/PageHeader';
import { Page } from '../components/ui/Page';
import { Card } from '../components/ui/Card';
import { CheckField, SaveNote, SettingGroup, SettingRow } from '../components/ui/FormLayout';
import { Button } from '../components/ui/Button';
import { PermissionHint } from '../components/ui/PermissionHint';
import { TextInput } from '../components/ui/Field';
import { InfoPress, InfoTip } from '../components/ui/InfoTip';
import { ScreenLink } from '../components/ui/ScreenLink';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import type {
  NeighborConfig,
  RetentionPolicy,
  RetentionRow,
  RetentionValues,
} from '../types/api';
import {
  describeCadence,
  discoveryFormFrom,
  isDiscoveryDirty,
  parseDiscoveryForm,
  parseScanLimit,
  scanLimitFrom,
  MIN_SCAN_MAX_TARGETS,
  MAX_SCAN_MAX_TARGETS,
  DISCOVERY_WALKS,
  MAX_NEIGHBOR_INTERVAL_SECS,
  MIN_NEIGHBOR_INTERVAL_SECS,
  WALK_INFO,
  type DiscoveryForm,
  type DiscoveryWalk,
} from './neighborSettings';
import {
  bandFor,
  formFromValues,
  isDirty,
  parseRetentionForm,
  rowField,
  rowMode,
  shortenedFields,
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
  const canConfig = useCan('manage_config');
  const canSystem = useCan('manage_system');
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
    <Page layout="sections">
      <PageHeader
        title={t('nav:settings.system')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.system') }]}
      />
      <Card
        title={t('settings.polling.title')}
        footer={
          canConfig && (
            <>
              {error && <SaveNote tone="error">{error}</SaveNote>}
              {saved && <SaveNote tone="ok">{t('settings.saved')}</SaveNote>}
              <Button variant="primary" onClick={save} disabled={busy || !loaded}>
                {t('common:actions.save')}
              </Button>
            </>
          )
        }
      >
        <SettingRow
          label={t('settings.polling.intervalName')}
          info={
            <InfoTip infoKey="system:settings.polling.info" label={t('settings.polling.intervalName')} />
          }
        >
          <TextInput
            className="sys-setting-input"
            value={value}
            onChange={(e) => {
              setValue(e.target.value);
              setSaved(false);
            }}
            inputMode="numeric"
            disabled={!canConfig || !loaded || busy}
            aria-label={t('settings.polling.intervalAria')}
          />
          <span className="muted">{t('settings.polling.seconds')}</span>
          <span className="muted">{t('settings.retention.band', { min: MIN, max: MAX })}</span>
        </SettingRow>
        {!canConfig && <PermissionHint permission="manage_config" />}
      </Card>
      <NeighborCard canConfig={canConfig} />
      {/* Retention is how long each store keeps data — a deployment decision, not a monitoring
          one, so it asks for ManageSystem while the two cards above stay ManageConfig (ADR-057). */}
      <RetentionCard canSystem={canSystem} />
    </Page>
  );
}

/** Settings ▸ System settings ▸ Discovery walks (ADR-038 / ADR-043).
 *
 *  Deployment-wide rather than per node or per profile: every OID involved is a fixed standard
 *  (LLDP-MIB / CISCO-CDP-MIB, RFC 1213 / RFC 4293), so there is nothing to tune per device — only
 *  whether to walk and how often.
 *
 *  Rendered from `DISCOVERY_WALKS` rather than as one block per walk, so adding one is a registry
 *  entry plus its locale strings — which is exactly what the routing walk (ADR-043 Increment 4)
 *  cost. All judgement is in `neighborSettings.ts` — Vitest never runs a .tsx (testing.md). */
function NeighborCard({ canConfig }: { canConfig: boolean }) {
  const { t } = useTranslation('system');
  const [saved, setSavedCfg] = useState<NeighborConfig | null>(null);
  const [form, setForm] = useState<DiscoveryForm | null>(null);
  // The sweep limit (ADR-219) is not a walk, so it sits beside the walks' form rather than in it.
  const [limit, setLimit] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);

  const load = useCallback(() => {
    api
      .getNeighborSettings()
      .then((cfg) => {
        setSavedCfg(cfg);
        setForm(discoveryFormFrom(cfg));
        setLimit(scanLimitFrom(cfg));
      })
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const patch = (walk: DiscoveryWalk, next: Partial<{ enabled: boolean; intervalSecs: string }>) => {
    setForm((f) => (f ? { ...f, [walk]: { ...f[walk], ...next } } : f));
    setOk(false);
  };

  const save = () => {
    if (!form || limit === null) return;
    const parsed = parseDiscoveryForm(form, {
      min: saved?.min_interval_secs,
      max: saved?.max_interval_secs,
    });
    if (!parsed.ok) {
      setError(
        t('settings.neighbors.err.rangeFor', {
          walk: t(`settings.neighbors.walk.${parsed.walk}.name`),
          min: parsed.min,
          max: parsed.max,
        }),
      );
      setOk(false);
      return;
    }
    const sweep = parseScanLimit(limit, {
      min: saved?.scan_max_targets_min,
      max: saved?.scan_max_targets_max,
    });
    if (!sweep.ok) {
      setError(t('settings.neighbors.err.scanLimit', { min: sweep.min, max: sweep.max }));
      setOk(false);
      return;
    }
    setBusy(true);
    setError(null);
    setOk(false);
    api
      .setNeighborSettings({ ...parsed.values, scan_max_targets: sweep.value })
      .then(() => {
        setOk(true);
        load();
      })
      .catch((e: unknown) => setError(errMsg(e, t('settings.neighbors.err.save'))))
      .finally(() => setBusy(false));
  };

  const dirty =
    saved && form && limit !== null
      ? isDiscoveryDirty(form, saved) || limit.trim() !== scanLimitFrom(saved)
      : false;

  return (
    <Card
      title={t('settings.neighbors.title')}
      footer={
        canConfig && (
          <>
            {error && <SaveNote tone="error">{error}</SaveNote>}
            {ok && <SaveNote tone="ok">{t('settings.saved')}</SaveNote>}
            <Button variant="primary" onClick={save} disabled={busy || !form || !dirty}>
              {t('common:actions.save')}
            </Button>
          </>
        )
      }
    >
      {DISCOVERY_WALKS.map((walk) => (
        <SettingGroup key={walk}>
          <SettingRow
            label={
              <InfoPress infoKey={WALK_INFO[walk]}>{t(`settings.neighbors.walk.${walk}.name`)}</InfoPress>
            }
            // The one walk whose cost scales with the network rather than the device: say so only
            // once it is switched on, beside where its results land.
            warn={
              walk === 'arp' &&
              form?.arp.enabled && (
                <>
                  {t('settings.neighbors.walk.arp.warn')} <ScreenLink to="/nodes/discovery" />
                </>
              )
            }
          >
            <CheckField
              checked={form?.[walk].enabled ?? false}
              disabled={!canConfig || !form || busy}
              onChange={(next) => patch(walk, { enabled: next })}
              label={
                form?.[walk].enabled ? t('settings.neighbors.enabledOn') : t('settings.neighbors.enabledOff')
              }
            />
          </SettingRow>
          <SettingRow
            label={t('settings.neighbors.intervalName')}
            detail={describeCadence(Number(form?.[walk].intervalSecs ?? 0), t)}
          >
            <TextInput
              className="sys-setting-input"
              value={form?.[walk].intervalSecs ?? ''}
              onChange={(e) => patch(walk, { intervalSecs: e.target.value })}
              inputMode="numeric"
              disabled={!canConfig || !form || busy}
              aria-label={`${t(`settings.neighbors.walk.${walk}.name`)} — ${t('settings.neighbors.intervalName')}`}
            />
            <span className="muted">{t('settings.polling.seconds')}</span>
            <span className="muted">
              {t('settings.retention.band', {
                min: saved?.min_interval_secs ?? MIN_NEIGHBOR_INTERVAL_SECS,
                max: saved?.max_interval_secs ?? MAX_NEIGHBOR_INTERVAL_SECS,
              })}
            </span>
          </SettingRow>
        </SettingGroup>
      ))}
      <SettingRow label={t('settings.neighbors.scanLimit.name')}>
        <TextInput
          className="sys-setting-input"
          value={limit ?? ''}
          onChange={(e) => {
            setLimit(e.target.value);
            setOk(false);
          }}
          inputMode="numeric"
          disabled={!canConfig || limit === null || busy}
          aria-label={t('settings.neighbors.scanLimit.name')}
        />
        <span className="muted">{t('settings.neighbors.scanLimit.unit')}</span>
        <span className="muted">
          {t('settings.retention.band', {
            min: saved?.scan_max_targets_min || MIN_SCAN_MAX_TARGETS,
            max: saved?.scan_max_targets_max || MAX_SCAN_MAX_TARGETS,
          })}
        </span>
      </SettingRow>
      {!canConfig && <PermissionHint permission="manage_config" />}
    </Card>
  );
}

/** Settings ▸ System settings ▸ Data retention (ADR-040). */
function RetentionCard({ canSystem }: { canSystem: boolean }) {
  const { t } = useTranslation('system');
  const [policy, setPolicy] = useState<RetentionPolicy | null>(null);
  const [form, setForm] = useState<RetentionForm | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  // A save that shortens any window deletes data on the next sweep, so it is confirmed first; the
  // parsed body waits here while the dialog is open.
  const [confirming, setConfirming] = useState<RetentionValues | null>(null);

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

  const send = (values: RetentionValues) =>
    api.updateRetention(values).then(() => {
      setSaved(true);
      // Re-read rather than assume: the flow row's TTL is applied by the server, and a store-owned
      // row could have changed underneath us.
      load();
    });

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
    setError(null);
    setSaved(false);
    if (policy && shortenedFields(parsed.values, policy.settings).length > 0) {
      setConfirming(parsed.values);
      return;
    }
    setBusy(true);
    send(parsed.values)
      .catch((e: unknown) => setError(errMsg(e, t('settings.retention.err.save'))))
      .finally(() => setBusy(false));
  };

  const dirty = policy && form ? isDirty(form, policy.settings) : false;

  return (
    <Card
      title={t('settings.retention.title')}
      footer={
        canSystem && (
          <>
            {error && <SaveNote tone="error">{error}</SaveNote>}
            {saved && <SaveNote tone="ok">{t('settings.saved')}</SaveNote>}
            <Button variant="primary" onClick={save} disabled={busy || !form || !dirty}>
              {t('common:actions.save')}
            </Button>
          </>
        )
      }
    >
      {(policy?.rows ?? []).map((row) => (
        <RetentionRowView
          key={row.subject}
          row={row}
          form={form}
          disabled={!canSystem || !form || busy}
          onChange={(field, v) => {
            setForm((f) => (f ? { ...f, [field]: v } : f));
            setSaved(false);
          }}
        />
      ))}
      {!canSystem && <PermissionHint permission="manage_system" />}
      {confirming && (
        <ConfirmDeleteModal
          title={t('settings.retention.title')}
          confirmLabel={t('settings.retention.confirmSave')}
          onConfirm={() => send(confirming)}
          errorFallback={t('settings.retention.err.save')}
          onClose={() => setConfirming(null)}
          onDone={() => setConfirming(null)}
        >
          {t('settings.retention.confirm')}
        </ConfirmDeleteModal>
      )}
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
        <span className="muted">{t(`settings.retention.unit.${row.unit || 'days'}`)}</span>
        <span className="muted">{t('settings.retention.band', { min, max })}</span>
      </>
    );
  } else if (mode === 'store-owned') {
    const v = storeValue(row);
    control = (
      <span className="muted">
        {v.kind === 'reported'
          ? t('settings.retention.storeValue', { value: v.value })
          : v.kind === 'not-configured'
            ? t('settings.retention.notConfigured')
            : t('settings.retention.storeUnknown')}
      </span>
    );
  } else {
    control = <span className="muted">{t('settings.retention.indefinite')}</span>;
  }

  return (
    <SettingRow
      label={name}
      detail={
        <>
          {row.store} — {row.note}
        </>
      }
    >
      {control}
    </SettingRow>
  );
}
