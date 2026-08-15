// SPDX-License-Identifier: AGPL-3.0-only
// Notification delivery (Alerts ▸ Notification delivery). Two things: notification
// CHANNELS (where alerts can go — webhook/email; the connection config is a secret, sealed
// server-side and never returned) and routing RULES (which alerts, by severity, fan out to
// which channels). The notifier snapshots these (refreshed ~30s) so edits take effect live;
// any env-configured channel stays an always-on default route.
//
// Data-table standard v2: each list is a section header + toolbar (count + "+ Add …") over the
// shared `.ytable`. Add via modal; enable/disable is an inline icon toggle; delete confirms in a
// modal. Channel kind and rule severity are neutral/status chips (categorical vs status).

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import {
  type ChannelConfigInput,
  type ChannelKind,
  type NotificationChannel,
  type RoutingRule,
  type Severity,
} from '../types/api';
import { channelKindOptions } from '../lib/channelKinds';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import { channelFilters, routingRuleFilters } from './routingFilters';
import { TrashIcon, PowerIcon, EditIcon } from '../components/ui/icons';
import { severityLabel } from '../lib/format';
import { ChannelTemplateModal } from './ChannelTemplateModal';
import { hasTemplate } from './channelTemplate';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';
import './RoutingPage.css';

const SEVERITY_TONE: Record<Severity, 'critical' | 'warning' | 'neutral'> = {
  critical: 'critical',
  warning: 'warning',
  info: 'neutral',
};

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
  const canConfig = useCan('manage_config');
  const [channels, setChannels] = useState<NotificationChannel[]>([]);
  const [rules, setRules] = useState<RoutingRule[]>([]);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    Promise.all([api.listNotificationChannels(), api.listRoutingRules()])
      .then(([ch, ru]) => {
        setChannels(ch);
        setRules(ru);
        setBlock(null);
      })
      .catch((e: unknown) => setBlock(classifyLoadError(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  if (block) {
    return (
      <div>
        <PageHeader
          title={t('nav:alerts.routing')}
          trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.routing') }]}
        />
        <LoadBlockNotice
          permission="manage_config"
          block={block}
          unavailable={t('routing.unavailable')}
        />
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
        canConfig={canConfig}
        loading={loading}
        onChange={load}
        onError={setError}
      />
      <RulesSection
        rules={rules}
        channels={channels}
        canConfig={canConfig}
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
  canConfig,
  loading,
  onChange,
  onError,
}: {
  channels: NotificationChannel[];
  canConfig: boolean;
  loading: boolean;
  onChange: () => void;
  onError: (m: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<NotificationChannel | null>(null);
  const [templating, setTemplating] = useState<NotificationChannel | null>(null);

  const toggle = (c: NotificationChannel) =>
    api
      .setNotificationChannelEnabled(c.id, !c.enabled)
      .then(onChange)
      .catch((e: unknown) => onError(errMsg(e, t('routing.err.update'))));

  // Client-side: the channel list is bounded by what an operator configured, not by fleet size
  // (ui-conventions). The judgement lives in `routingFilters.ts`.
  const [sheet, setSheet] = useState(false);
  const columns = useMemo<Column<NotificationChannel>[]>(() => {
    const kinds = [...new Set(channels.map((c) => c.kind))].sort();
    const specs = channelFilters(t, kinds);
    const cols: Column<NotificationChannel>[] = [
      {
        key: 'name',
        header: t('routing.channels.cols.name'),
        width: '1.6fr',
        render: (c) => <span className="yt-name-txt">{c.name}</span>,
      },
      {
        key: 'kind',
        header: t('routing.channels.cols.kind'),
        width: '140px',
        render: (c) => (
          <>
            <Badge tone="neutral">{c.kind}</Badge>
            {hasTemplate(c) && (
              <Badge tone="neutral" title={t('routing.channels.templatedHint')}>
                {t('routing.channels.templated')}
              </Badge>
            )}
          </>
        ),
      },
      {
        key: 'status',
        header: t('routing.channels.cols.status'),
        width: '130px',
        render: (c) => <EnabledStatus enabled={c.enabled} />,
      },
      {
        key: 'actions',
        header: t('routing.channels.cols.actions'),
        width: '96px',
        align: 'right',
        render: (c) =>
          canConfig ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  {
                    label: t('routing.channels.template'),
                    icon: <EditIcon />,
                    onClick: () => setTemplating(c),
                  },
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
          ) : null,
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
    // `toggle` is rebuilt every render; listing it would rebuild the columns on every keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t, canConfig, channels]);

  // ⚠️ **Component state, not the URL.** The column key IS the URL key (ADR-053 decision 12), and
  // this route has two tables — both with a `name` and a `status` column. URL-backing either one
  // would make the two filter each other.
  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    columns,
    channels,
  );

  return (
    <section>
      <div className="table-toolbar">
        <h2 className="table-section-title">{t('routing.channels.title')}</h2>
        <FilterButton columns={filterCols} filters={filters} onOpen={() => setSheet(true)} />
        <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
        <TableSpacer />
        <ResultCount
          shown={shown.length}
          total={anyFiltered ? channels.length : undefined}
          noun={t('noun.channel', { count: shown.length })}
        />
        {canConfig && (
          <Button variant="primary" onClick={() => setAdding(true)}>
            {t('routing.channels.add')}
          </Button>
        )}
      </div>

      <DataTable
        rows={shown}
        columns={columns}
        rowKey={(c) => c.id}
        rowClass={(c) => (c.enabled ? undefined : 'is-muted')}
        filters={filters}
        onFiltersChange={setFilters}
        filterCounts={counts}
        loading={loading}
        empty={anyFiltered ? t('common:filter.noMatch') : t('routing.channels.empty')}
      />
      {sheet && (
        <MobileFilterSheet
          columns={filterCols}
          filters={filters}
          onChange={setFilters}
          counts={counts}
          labels={Object.fromEntries(columns.map((c) => [c.key, String(c.header)]))}
          onClose={() => setSheet(false)}
        />
      )}

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
      {templating && (
        <ChannelTemplateModal
          channel={templating}
          onClose={() => setTemplating(null)}
          onDone={() => {
            setTemplating(null);
            onChange();
          }}
          onError={onError}
        />
      )}
      {deleting && (
        <ConfirmDeleteModal
          title={t('routing.channels.delete')}
          onConfirm={() => api.deleteNotificationChannel(deleting.id)}
          errorFallback={t('routing.err.delete')}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            onChange();
          }}
        >
          <Trans
            t={t}
            i18nKey="routing.channels.deleteBody"
            values={{ name: deleting.name }}
            components={{ strong: <strong /> }}
          />
        </ConfirmDeleteModal>
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
          {/* Derived from `CHANNEL_KINDS`, which is what its doc comment always claimed and what
              nothing actually did — these were four `<option>` literals, so the union and the list
              an operator can pick from were two copies. A fifth kind is now a compile error in
              `lib/channelKinds.ts` rather than an option nobody adds. */}
          {channelKindOptions().map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
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
  canConfig,
  loading,
  onChange,
  onError,
}: {
  rules: RoutingRule[];
  channels: NotificationChannel[];
  canConfig: boolean;
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

  // Client-side, same reason as the channels table above.
  const [sheet, setSheet] = useState(false);
  const columns = useMemo<Column<RoutingRule>[]>(() => {
    const specs = routingRuleFilters(t, severityLabel);
    const cols: Column<RoutingRule>[] = [
      {
        key: 'name',
        header: t('routing.rules.cols.name'),
        width: '1.4fr',
        render: (r) => <span className="yt-name-txt">{r.name}</span>,
      },
      {
        key: 'severity',
        header: t('routing.rules.cols.severity'),
        width: '130px',
        render: (r) => (
          <Badge tone={r.severity ? SEVERITY_TONE[r.severity] : 'neutral'}>
            {r.severity ? severityLabel(r.severity) : t('routing.rules.any')}
          </Badge>
        ),
      },
      {
        key: 'channels',
        header: t('routing.rules.cols.channels'),
        width: '1fr',
        render: (r) => (
          <span className="muted ellipsis">
            {r.channel_ids.map(channelName).join(', ') || t('routing.rules.noChannels')}
          </span>
        ),
      },
      {
        key: 'status',
        header: t('routing.rules.cols.status'),
        width: '130px',
        render: (r) => <EnabledStatus enabled={r.enabled} />,
      },
      {
        key: 'actions',
        header: t('routing.rules.cols.actions'),
        width: '96px',
        align: 'right',
        render: (r) =>
          canConfig ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  {
                    label: r.enabled ? t('routing.rules.disable') : t('routing.rules.enable'),
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
          ) : null,
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
    // `channelName` and `toggle` are rebuilt every render; what they read is listed instead.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t, canConfig, channels]);

  // Component state, not the URL — see the channels table above for why this route cannot use it.
  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    columns,
    rules,
  );

  return (
    <section className="routing-rules-section">
      <div className="table-toolbar">
        <h2 className="table-section-title">{t('routing.rules.title')}</h2>
        <FilterButton columns={filterCols} filters={filters} onOpen={() => setSheet(true)} />
        <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
        <TableSpacer />
        <ResultCount
          shown={shown.length}
          total={anyFiltered ? rules.length : undefined}
          noun={t('common:noun.rule', { count: shown.length })}
        />
        {canConfig && (
          <Button variant="primary" onClick={() => setAdding(true)} disabled={channels.length === 0}>
            {t('routing.rules.add')}
          </Button>
        )}
      </div>

      <DataTable
        rows={shown}
        columns={columns}
        rowKey={(r) => r.id}
        rowClass={(r) => (r.enabled ? undefined : 'is-muted')}
        filters={filters}
        onFiltersChange={setFilters}
        filterCounts={counts}
        loading={loading}
        empty={anyFiltered ? t('common:filter.noMatch') : t('routing.rules.empty')}
      />
      {sheet && (
        <MobileFilterSheet
          columns={filterCols}
          filters={filters}
          onChange={setFilters}
          counts={counts}
          labels={Object.fromEntries(columns.map((c) => [c.key, String(c.header)]))}
          onClose={() => setSheet(false)}
        />
      )}

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
          onConfirm={() => api.deleteRoutingRule(deleting.id)}
          errorFallback={t('routing.err.delete')}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            onChange();
          }}
        >
          <Trans
            t={t}
            i18nKey="routing.rules.deleteBody"
            values={{ name: deleting.name }}
            components={{ strong: <strong /> }}
          />
        </ConfirmDeleteModal>
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
