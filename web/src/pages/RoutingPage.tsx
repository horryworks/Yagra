// SPDX-License-Identifier: AGPL-3.0-only
// Notification routing (Alerts ▸ Notification routing). Two things: notification
// CHANNELS (where alerts can go — webhook/email; the connection config is a secret, sealed
// server-side and never returned) and routing RULES (which alerts, by severity, fan out to
// which channels). The notifier snapshots these (refreshed ~30s) so edits take effect live;
// any env-configured channel stays an always-on default route.
//
// Data-table standard v2: each list is a section header + toolbar (count + "+ Add …") over the
// shared `.ytable`. Add via modal; enable/disable is an inline icon toggle; delete confirms in a
// modal. Channel kind and rule severity are neutral/status chips (categorical vs status).

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type {
  ChannelConfigInput,
  ChannelKind,
  NotificationChannel,
  RoutingRule,
  Severity,
} from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon, PowerIcon } from '../components/ui/icons';
import { severityLabel } from '../lib/format';
import './RoutingPage.css';

const SEVERITY_TONE: Record<Severity, 'critical' | 'warning' | 'neutral'> = {
  critical: 'critical',
  warning: 'warning',
  info: 'neutral',
};

const CHANNEL_COLS = '1.6fr 140px 130px 96px';
const RULE_COLS = '1.4fr 130px 1fr 130px 96px';

/** Inline status (dot + label) shared by channels and rules. */
function EnabledStatus({ enabled }: { enabled: boolean }) {
  const { t } = useTranslation('alertsConfig');
  return (
    <span className={enabled ? 'yt-status enabled' : 'yt-status disabled'}>
      <span className="yt-status-dot" />
      {enabled ? t('status.enabled') : t('status.disabled')}
    </span>
  );
}

export function RoutingPage() {
  const { t } = useTranslation('alertsConfig');
  const authed = useAuthStore((s) => s.authed);
  const [channels, setChannels] = useState<NotificationChannel[]>([]);
  const [rules, setRules] = useState<RoutingRule[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    Promise.all([api.listNotificationChannels(), api.listRoutingRules()])
      .then(([ch, ru]) => {
        setChannels(ch);
        setRules(ru);
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

  if (unavailable) {
    return (
      <div>
        <PageHeader
          title={t('nav:alerts.routing')}
          trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.routing') }]}
        />
        <Card>
          <p className="muted">{t('routing.unavailable')}</p>
        </Card>
      </div>
    );
  }

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.routing')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.routing') }]}
        note={t('routing.note')}
      />
      {error && <p className="form-error routing-error">{error}</p>}
      <ChannelsSection
        channels={channels}
        authed={authed}
        loading={loading}
        onChange={load}
        onError={setError}
      />
      <RulesSection
        rules={rules}
        channels={channels}
        authed={authed}
        loading={loading}
        onChange={load}
        onError={setError}
      />
    </div>
  );
}

// ── Channels ─────────────────────────────────────────────────────────────────

function ChannelsSection({
  channels,
  authed,
  loading,
  onChange,
  onError,
}: {
  channels: NotificationChannel[];
  authed: boolean;
  loading: boolean;
  onChange: () => void;
  onError: (m: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<NotificationChannel | null>(null);

  const toggle = (c: NotificationChannel) =>
    api
      .setNotificationChannelEnabled(c.id, !c.enabled)
      .then(onChange)
      .catch((e: unknown) => onError(errMsg(e, t('routing.err.update'))));

  return (
    <section>
      <div className="table-toolbar">
        <h2 className="table-section-title">{t('routing.channels.title')}</h2>
        <TableSpacer />
        <ResultCount shown={channels.length} noun={t('noun.channel', { count: channels.length })} />
        {authed && (
          <Button variant="primary" onClick={() => setAdding(true)}>
            {t('routing.channels.add')}
          </Button>
        )}
      </div>

      <div className="ytable channels-table">
        <div className="ytable-head" style={{ gridTemplateColumns: CHANNEL_COLS }}>
          <div className="ytable-h">{t('routing.channels.cols.name')}</div>
          <div className="ytable-h">{t('routing.channels.cols.kind')}</div>
          <div className="ytable-h">{t('routing.channels.cols.status')}</div>
          <div className="ytable-h right">{t('routing.channels.cols.actions')}</div>
        </div>
        {channels.length === 0 ? (
          <div className="yt-empty">
            <p className="yt-empty-title">
              {loading ? t('common:loading') : t('routing.channels.empty')}
            </p>
            {!loading && <p className="yt-empty-sub">{t('routing.channels.emptySub')}</p>}
          </div>
        ) : (
          channels.map((c) => (
            <div
              className={c.enabled ? 'ytable-row' : 'ytable-row is-muted'}
              style={{ gridTemplateColumns: CHANNEL_COLS }}
              key={c.id}
            >
              <div className="ytable-cell">
                <span className="yt-name-txt">{c.name}</span>
              </div>
              <div className="ytable-cell">
                <Badge tone="neutral">{c.kind}</Badge>
              </div>
              <div className="ytable-cell">
                <EnabledStatus enabled={c.enabled} />
              </div>
              <div className="ytable-cell right">
                {authed && (
                  <span className="ytable-actions">
                    <OverflowMenu
                      actions={[
                        {
                          label: c.enabled
                            ? t('routing.channels.disable')
                            : t('routing.channels.enable'),
                          icon: <PowerIcon />,
                          onClick: () => toggle(c),
                        },
                        {
                          label: t('routing.channels.delete'),
                          icon: <TrashIcon />,
                          danger: true,
                          onClick: () => setDeleting(c),
                        },
                      ]}
                    />
                  </span>
                )}
              </div>
            </div>
          ))
        )}
      </div>

      {adding && (
        <AddChannelModal
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            onChange();
          }}
          onError={onError}
        />
      )}
      {deleting && (
        <ConfirmDeleteModal
          title={t('routing.channels.delete')}
          body={
            <Trans
              t={t}
              i18nKey="routing.channels.deleteBody"
              values={{ name: deleting.name }}
              components={{ strong: <strong /> }}
            />
          }
          run={() => api.deleteNotificationChannel(deleting.id)}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            onChange();
          }}
        />
      )}
    </section>
  );
}

function AddChannelModal({
  onClose,
  onDone,
  onError,
}: {
  onClose: () => void;
  onDone: () => void;
  onError: (m: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [name, setName] = useState('');
  const [kind, setKind] = useState<ChannelKind>('webhook');
  const [url, setUrl] = useState('');
  const [host, setHost] = useState('');
  const [from, setFrom] = useState('');
  const [to, setTo] = useState('');
  // PagerDuty: routing key + region (region maps to the api_url override).
  const [routingKey, setRoutingKey] = useState('');
  const [pdRegion, setPdRegion] = useState<'us' | 'eu'>('us');
  // JSM: integration base URL + GenieKey.
  const [jsmUrl, setJsmUrl] = useState('https://api.atlassian.com/jsm/ops/integration/v2');
  const [jsmKey, setJsmKey] = useState('');
  const [busy, setBusy] = useState(false);

  const canAdd =
    name.trim() !== '' &&
    (kind === 'webhook'
      ? url.trim() !== ''
      : kind === 'email'
        ? host.trim() !== '' && from.trim() !== '' && to.trim() !== ''
        : kind === 'pagerduty'
          ? routingKey.trim() !== ''
          : jsmUrl.trim() !== '' && jsmKey.trim() !== '');

  const buildConfig = (): ChannelConfigInput => {
    switch (kind) {
      case 'webhook':
        return { kind: 'webhook', url: url.trim() };
      case 'email':
        return { kind: 'email', host: host.trim(), from: from.trim(), to: to.trim() };
      case 'pagerduty':
        return {
          kind: 'pagerduty',
          routing_key: routingKey.trim(),
          api_url:
            pdRegion === 'eu' ? 'https://events.eu.pagerduty.com/v2/enqueue' : undefined,
        };
      case 'jsm':
        return { kind: 'jsm', api_url: jsmUrl.trim(), api_key: jsmKey.trim() };
    }
  };

  const submit = () => {
    if (!canAdd) return;
    setBusy(true);
    api
      .createNotificationChannel({ name: name.trim(), config: buildConfig() })
      .then(onDone)
      .catch((e: unknown) => {
        onError(errMsg(e, t('routing.err.addChannel')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('routing.channelModal.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!canAdd || busy}>
            {t('routing.channelModal.add')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('routing.channelModal.name')}</label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('routing.channelModal.kind')}</label>
        <Select value={kind} onChange={(e) => setKind(e.target.value as ChannelKind)}>
          <option value="webhook">webhook</option>
          <option value="email">email</option>
          <option value="pagerduty">PagerDuty</option>
          <option value="jsm">Jira Service Management</option>
        </Select>
      </div>
      {kind === 'webhook' && (
        <div className="modal-field">
          <label className="modal-field-label">{t('routing.channelModal.webhookUrl')}</label>
          <TextInput
            className="mono"
            placeholder="https://hooks.example/…"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
          />
          <span className="modal-hint">{t('routing.channelModal.webhookSealed')}</span>
        </div>
      )}
      {kind === 'email' && (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('routing.channelModal.smtpHost')}</label>
            <TextInput value={host} onChange={(e) => setHost(e.target.value)} />
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('routing.channelModal.from')}</label>
            <TextInput value={from} onChange={(e) => setFrom(e.target.value)} />
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('routing.channelModal.to')}</label>
            <TextInput value={to} onChange={(e) => setTo(e.target.value)} />
          </div>
        </>
      )}
      {kind === 'pagerduty' && (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('routing.channelModal.routingKey')}</label>
            <TextInput
              className="mono"
              placeholder="R0XXXXXXXXXXXXXXXXXXXXXXXXX"
              value={routingKey}
              onChange={(e) => setRoutingKey(e.target.value)}
            />
            <span className="modal-hint">{t('routing.channelModal.routingKeyHint')}</span>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('routing.channelModal.region')}</label>
            <Select value={pdRegion} onChange={(e) => setPdRegion(e.target.value as 'us' | 'eu')}>
              <option value="us">{t('routing.channelModal.regionUs')}</option>
              <option value="eu">{t('routing.channelModal.regionEu')}</option>
            </Select>
          </div>
        </>
      )}
      {kind === 'jsm' && (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('routing.channelModal.apiUrl')}</label>
            <TextInput
              className="mono"
              value={jsmUrl}
              onChange={(e) => setJsmUrl(e.target.value)}
            />
            <span className="modal-hint">{t('routing.channelModal.jsmUrlHint')}</span>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('routing.channelModal.apiKey')}</label>
            <TextInput
              className="mono"
              value={jsmKey}
              onChange={(e) => setJsmKey(e.target.value)}
            />
            <span className="modal-hint">{t('routing.channelModal.sealed')}</span>
          </div>
        </>
      )}
    </Modal>
  );
}

// ── Rules ────────────────────────────────────────────────────────────────────

function RulesSection({
  rules,
  channels,
  authed,
  loading,
  onChange,
  onError,
}: {
  rules: RoutingRule[];
  channels: NotificationChannel[];
  authed: boolean;
  loading: boolean;
  onChange: () => void;
  onError: (m: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<RoutingRule | null>(null);

  const channelName = (id: string) => channels.find((c) => c.id === id)?.name ?? id;

  const toggle = (r: RoutingRule) =>
    api
      .setRoutingRuleEnabled(r.id, !r.enabled)
      .then(onChange)
      .catch((e: unknown) => onError(errMsg(e, t('routing.err.update'))));

  return (
    <section className="routing-rules-section">
      <div className="table-toolbar">
        <h2 className="table-section-title">{t('routing.rules.title')}</h2>
        <TableSpacer />
        <ResultCount shown={rules.length} noun={t('common:noun.rule', { count: rules.length })} />
        {authed && (
          <Button variant="primary" onClick={() => setAdding(true)} disabled={channels.length === 0}>
            {t('routing.rules.add')}
          </Button>
        )}
      </div>

      <div className="ytable rules-table">
        <div className="ytable-head" style={{ gridTemplateColumns: RULE_COLS }}>
          <div className="ytable-h">{t('routing.rules.cols.name')}</div>
          <div className="ytable-h">{t('routing.rules.cols.severity')}</div>
          <div className="ytable-h">{t('routing.rules.cols.channels')}</div>
          <div className="ytable-h">{t('routing.rules.cols.status')}</div>
          <div className="ytable-h right">{t('routing.rules.cols.actions')}</div>
        </div>
        {rules.length === 0 ? (
          <div className="yt-empty">
            <p className="yt-empty-title">
              {loading ? t('common:loading') : t('routing.rules.empty')}
            </p>
            {!loading && <p className="yt-empty-sub">{t('routing.rules.emptySub')}</p>}
          </div>
        ) : (
          rules.map((r) => (
            <div
              className={r.enabled ? 'ytable-row' : 'ytable-row is-muted'}
              style={{ gridTemplateColumns: RULE_COLS }}
              key={r.id}
            >
              <div className="ytable-cell">
                <span className="yt-name-txt">{r.name}</span>
              </div>
              <div className="ytable-cell">
                <Badge tone={r.severity ? SEVERITY_TONE[r.severity] : 'neutral'}>
                  {r.severity ? severityLabel(r.severity) : t('routing.rules.any')}
                </Badge>
              </div>
              <div className="ytable-cell ellipsis">
                <span className="muted">
                  {r.channel_ids.map(channelName).join(', ') || t('routing.rules.noChannels')}
                </span>
              </div>
              <div className="ytable-cell">
                <EnabledStatus enabled={r.enabled} />
              </div>
              <div className="ytable-cell right">
                {authed && (
                  <span className="ytable-actions">
                    <OverflowMenu
                      actions={[
                        {
                          label: r.enabled
                            ? t('routing.rules.disable')
                            : t('routing.rules.enable'),
                          icon: <PowerIcon />,
                          onClick: () => toggle(r),
                        },
                        {
                          label: t('routing.rules.delete'),
                          icon: <TrashIcon />,
                          danger: true,
                          onClick: () => setDeleting(r),
                        },
                      ]}
                    />
                  </span>
                )}
              </div>
            </div>
          ))
        )}
      </div>

      {adding && (
        <AddRuleModal
          channels={channels}
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            onChange();
          }}
          onError={onError}
        />
      )}
      {deleting && (
        <ConfirmDeleteModal
          title={t('routing.rules.deleteTitle')}
          body={
            <Trans
              t={t}
              i18nKey="routing.rules.deleteBody"
              values={{ name: deleting.name }}
              components={{ strong: <strong /> }}
            />
          }
          run={() => api.deleteRoutingRule(deleting.id)}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            onChange();
          }}
        />
      )}
    </section>
  );
}

function AddRuleModal({
  channels,
  onClose,
  onDone,
  onError,
}: {
  channels: NotificationChannel[];
  onClose: () => void;
  onDone: () => void;
  onError: (m: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [name, setName] = useState('');
  const [severity, setSeverity] = useState<'' | Severity>('');
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);

  const toggle = (id: string) =>
    setSelected((cur) => {
      const next = new Set(cur);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const canAdd = name.trim() !== '' && selected.size > 0;

  const submit = () => {
    if (!canAdd) return;
    setBusy(true);
    api
      .createRoutingRule({
        name: name.trim(),
        severity: severity === '' ? null : severity,
        channel_ids: [...selected],
      })
      .then(onDone)
      .catch((e: unknown) => {
        onError(errMsg(e, t('routing.err.addRule')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('routing.ruleModal.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!canAdd || busy}>
            {t('routing.ruleModal.add')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('routing.ruleModal.name')}</label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('routing.ruleModal.severity')}</label>
        <Select value={severity} onChange={(e) => setSeverity(e.target.value as '' | Severity)}>
          <option value="">{t('routing.ruleModal.anySeverity')}</option>
          <option value="critical">{severityLabel('critical')}</option>
          <option value="warning">{severityLabel('warning')}</option>
          <option value="info">{severityLabel('info')}</option>
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('routing.ruleModal.channels')}</label>
        <div className="routing-picks">
          {channels.map((c) => (
            <label key={c.id} className="routing-pick">
              <input type="checkbox" checked={selected.has(c.id)} onChange={() => toggle(c.id)} />
              {c.name}
            </label>
          ))}
        </div>
      </div>
    </Modal>
  );
}

// ── Shared confirm-delete modal ────────────────────────────────────────────────

function ConfirmDeleteModal({
  title,
  body,
  run,
  onClose,
  onDone,
}: {
  title: string;
  body: ReactNode;
  run: () => Promise<void>;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    run()
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('routing.err.delete')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={title}
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
      <p className="modal-confirm-text">{body}</p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
