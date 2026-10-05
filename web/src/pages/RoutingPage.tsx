// SPDX-License-Identifier: AGPL-3.0-only
// Notification delivery (Alerts ▸ Notification delivery). Three things: notification
// CHANNELS (where alerts can go — webhook/email; the connection config is a secret, sealed
// server-side and never returned) and routing RULES (which alerts, by severity, fan out to
// which channels). The notifier snapshots these (refreshed ~30s) so edits take effect live;
// any env-configured channel stays an always-on default route. The third section is the delivery
// log (ADR-195, `DeliveryLog.tsx`): what each delivery did, and on whose side a failure was.
//
// Data-table standard v2: each list is a section header + toolbar (count + "+ Add …") over the
// shared `.ytable`. Add via modal; enable/disable is an inline icon toggle; delete confirms in a
// modal. Channel kind and rule severity are neutral/status chips (categorical vs status).

import { useMemo, useRef, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import {
  type ChannelConfigInput,
  type ChannelKind,
  type ChannelTestResult,
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
import { ListToolbar } from '../components/ui/ListToolbar';
import { columnLabels } from '../lib/listToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { useClientFilters } from '../lib/useClientFilters';
import {
  CHANNEL_FILTER_PREFIX,
  channelFilters,
  ROUTING_RULE_FILTER_PREFIX,
  routingRuleFilters,
} from './routingFilters';
import { TrashIcon, PowerIcon, EditIcon, BellIcon, SearchIcon } from '../components/ui/icons';
import { SEVERITY_TONE, severityLabel } from '../lib/format';
import { ChannelTemplateModal } from './ChannelTemplateModal';
import { hasTemplate } from './channelTemplate';
import { useLoad } from '../lib/useLoad';
import { rowActionsWidth } from '../lib/rowActions';
import { LoadGate } from '../components/ui/LoadGate';
import './RoutingPage.css';
import { done, step } from '../lib/submitState';
import { testVerdict, testWarnings, VERDICT_KEYS, verdictOk } from './channelTest';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { DeliveryLog } from './DeliveryLog';
import { DELIVERY_FILTER_PREFIX, deliveryFilters } from './deliveryLogQuery';
import { specColumns } from '../lib/columnFilter';
import { useFilterParams } from '../lib/useFilterParams';

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
  // A notification channel holds a PagerDuty / JSM token, and a routing rule decides where every
  // alert goes; ADR-057 keeps both with the administrator.
  const canSystem = useCan('manage_system');
  const [error, setError] = useState<string | null>(null);

  // One read for both tables: a rule names its channel, so the two are only ever shown together.
  const routing = useLoad(
    () =>
      Promise.all([api.listNotificationChannels(), api.listRoutingRules()]).then(
        ([channels, rules]) => ({ channels, rules }),
      ),
    [],
    { initial: { channels: [] as NotificationChannel[], rules: [] as RoutingRule[] } },
  );
  const {
    data: { channels, rules },
    loading,
    reload: load,
  } = routing;

  // The delivery log's filters live here rather than in its section so a channel row can narrow
  // the log to that channel (ADR-195). In the URL under `log.`.
  const logCols = useMemo(() => specColumns(deliveryFilters(t, channels)), [t, channels]);
  const logFilters = useFilterParams(logCols, DELIVERY_FILTER_PREFIX);
  const logRef = useRef<HTMLElement>(null);
  const showLog = (channelId: string) => {
    logFilters.setFilters({ ...logFilters.filters, channel: channelId });
    logRef.current?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  };

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.routing')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.routing') }]}
      />
      <LoadGate load={routing} permission="manage_system">
        {error && <p className="form-error routing-error">{error}</p>}
        <ChannelsSection
          channels={channels}
          canSystem={canSystem}
          loading={loading}
          onChange={load}
          onError={setError}
          onShowLog={showLog}
        />
        <RulesSection
          rules={rules}
          channels={channels}
          canSystem={canSystem}
          loading={loading}
          onChange={load}
          onError={setError}
        />
        <DeliveryLog channels={channels} filterState={logFilters} sectionRef={logRef} />
      </LoadGate>
    </div>
  );
}

// ── Channels ─────────────────────────────────────────────────────────────────

function ChannelsSection({
  channels,
  canSystem,
  loading,
  onChange,
  onError,
  onShowLog,
}: {
  channels: NotificationChannel[];
  canSystem: boolean;
  loading: boolean;
  onChange: () => void;
  onError: (m: string) => void;
  onShowLog: (channelId: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<NotificationChannel | null>(null);
  const [templating, setTemplating] = useState<NotificationChannel | null>(null);
  const [testing, setTesting] = useState<NotificationChannel | null>(null);

  const toggle = (c: NotificationChannel) =>
    api
      .setNotificationChannelEnabled(c.id, !c.enabled)
      .then(onChange)
      .catch((e: unknown) => onError(errMsg(e, t('routing.err.update'))));

  // Client-side: the channel list is bounded by what an operator configured, not by fleet size
  // (ui-conventions). The judgement lives in `routingFilters.ts`.
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
        // Test, delivery log, template, on/off, delete.
        width: rowActionsWidth(5),
        align: 'right',
        render: (c) =>
          canSystem ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  {
                    label: t('routing.channels.test'),
                    icon: <BellIcon />,
                    onClick: () => setTesting(c),
                  },
                  {
                    label: t('routing.channels.showLog'),
                    icon: <SearchIcon />,
                    onClick: () => onShowLog(c.id),
                  },
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
    // `toggle` and `onShowLog` are rebuilt every render; listing them would rebuild the columns on
    // every keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t, canSystem, channels]);

  // In the URL, under `channels.` (ADR-153). This route has two tables and both have a `name` and a
  // `status` column, so each carries its own prefix — which is what used to keep both of them out of
  // the URL altogether, and a reload threw the filters away.
  const filtering = useClientFilters(columns, channels, { prefix: CHANNEL_FILTER_PREFIX });
  const { filters, setFilters, shown, counts, anyFiltered } = filtering;

  return (
    <section>
      <ListToolbar
        list={filtering}
        labels={columnLabels(columns)}
        count={{
          shown: shown.length,
          total: channels.length,
          noun: (n) => t('noun.channel', { count: n }),
        }}
        leading={
          <h2 className="table-section-title">{t('routing.channels.title')}</h2>
        }
      >
        {canSystem && (
          <Button variant="primary" onClick={() => setAdding(true)}>
            {t('routing.channels.add')}
          </Button>
        )}
      </ListToolbar>

      <DataTable
        tableId="settings.notificationChannels"
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

      {adding && (
        <AddChannelModal
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            onChange();
          }}
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
        />
      )}
      {testing && <TestChannelModal channel={testing} onClose={() => setTesting(null)} />}
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

/** Send one test notification through a channel and show what happened (ADR-192). Sending writes
 *  nothing, so it is a `step()`: the dialog stays open on the answer and can send again. */
function TestChannelModal({
  channel,
  onClose,
}: {
  channel: NotificationChannel;
  onClose: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [result, setResult] = useState<ChannelTestResult | null>(null);
  const form = useSubmit({ errorFallback: t('routing.err.test'), onDone: onClose });
  const warn = testWarnings(channel);
  const verdict = result ? testVerdict(result) : null;

  const send = () => {
    setResult(null);
    form.submit(() =>
      api.testNotificationChannel(channel.id).then((r) => {
        setResult(r);
        return step();
      }),
    );
  };

  return (
    <Modal
      title={t('routing.test.title', { name: channel.name })}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={send}
          submitLabel={result ? t('routing.test.again') : t('routing.test.send')}
          busyLabel={t('routing.test.sending')}
        />
      }
    >
      {/* Only what the test will do that the operator cannot see from here: it pages someone, the
          payload is not marked as a test, the channel is off. Each is one sentence (ADR-200). */}
      {warn.pages && <p className="routing-test-warn">{t('routing.test.pages')}</p>}
      {warn.unmarkedBody && <p className="routing-test-warn">{t('routing.test.unmarkedBody')}</p>}
      {warn.disabled && <p className="routing-test-warn">{t('routing.test.disabled')}</p>}
      {verdict && (
        <p
          className={verdictOk(verdict) ? 'routing-test-result' : 'routing-test-result form-error'}
          role="status"
        >
          {t(VERDICT_KEYS[verdict], { error: result?.error ?? '' })}
        </p>
      )}
      <FormError form={form} />
    </Modal>
  );
}

function AddChannelModal({
  onClose,
  onDone,
}: {
  onClose: () => void;
  onDone: () => void;
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
  // A failure is said in the dialog. It used to go to the page, behind the overlay (F4).
  const form = useSubmit({ errorFallback: t('routing.err.addChannel'), onDone });

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
    form.submit(() =>
      api
        .createNotificationChannel({ name: name.trim(), config: buildConfig() })
        .then(() => done()),
    );
  };

  return (
    <Modal
      title={t('routing.channelModal.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('routing.channelModal.add')}
          canSubmit={canAdd}
        />
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
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('routing.channelModal.apiKey')}</label>
            <TextInput
              className="mono"
              value={jsmKey}
              onChange={(e) => setJsmKey(e.target.value)}
            />
          </div>
        </>
      )}
      <FormError form={form} />
    </Modal>
  );
}

// ── Rules ────────────────────────────────────────────────────────────────────

function RulesSection({
  rules,
  channels,
  canSystem,
  loading,
  onChange,
  onError,
}: {
  rules: RoutingRule[];
  channels: NotificationChannel[];
  canSystem: boolean;
  loading: boolean;
  onChange: () => void;
  onError: (m: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<RoutingRule | null>(null);
  const [deleting, setDeleting] = useState<RoutingRule | null>(null);

  const channelName = (id: string) => channels.find((c) => c.id === id)?.name ?? id;

  const toggle = (r: RoutingRule) =>
    api
      .setRoutingRuleEnabled(r.id, !r.enabled)
      .then(onChange)
      .catch((e: unknown) => onError(errMsg(e, t('routing.err.update'))));

  // Client-side, same reason as the channels table above.
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
        render: (r) => {
          const names = r.channel_ids.map(channelName).join(', ') || t('routing.rules.noChannels');
          // The whole list on hover: a rule may name more channels than the column can show.
          return (
            <span className="muted ellipsis" title={names}>
              {names}
            </span>
          );
        },
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
        // Edit, on/off, delete.
        width: rowActionsWidth(3),
        align: 'right',
        render: (r) =>
          canSystem ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  {
                    label: t('routing.rules.edit'),
                    icon: <EditIcon />,
                    onClick: () => setEditing(r),
                  },
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
  }, [t, canSystem, channels]);

  // In the URL, under `rules.` — see the channels table above.
  const filtering = useClientFilters(columns, rules, { prefix: ROUTING_RULE_FILTER_PREFIX });
  const { filters, setFilters, shown, counts, anyFiltered } = filtering;

  return (
    <section className="routing-rules-section">
      <ListToolbar
        list={filtering}
        labels={columnLabels(columns)}
        count={{
          shown: shown.length,
          total: rules.length,
          noun: (n) => t('common:noun.rule', { count: n }),
        }}
        leading={
          <h2 className="table-section-title">{t('routing.rules.title')}</h2>
        }
      >
        {canSystem && (
          <Button variant="primary" onClick={() => setAdding(true)} disabled={channels.length === 0}>
            {t('routing.rules.add')}
          </Button>
        )}
      </ListToolbar>

      <DataTable
        tableId="settings.routingRules"
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

      {adding && (
        <RuleModal
          channels={channels}
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            onChange();
          }}
        />
      )}
      {editing && (
        <RuleModal
          rule={editing}
          channels={channels}
          onClose={() => setEditing(null)}
          onDone={() => {
            setEditing(null);
            onChange();
          }}
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

/** Add a routing rule, or — given `rule` — edit one in place (ADR-193). The edit replaces the name,
 *  severity and channels and leaves the rule's on/off switch as it was. */
function RuleModal({
  rule,
  channels,
  onClose,
  onDone,
}: {
  rule?: RoutingRule;
  channels: NotificationChannel[];
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [name, setName] = useState(rule?.name ?? '');
  const [severity, setSeverity] = useState<'' | Severity>(rule?.severity ?? '');
  const [selected, setSelected] = useState<Set<string>>(() => new Set(rule?.channel_ids ?? []));
  // A failure is said in the dialog. It used to go to the page, behind the overlay (F4).
  const form = useSubmit({
    errorFallback: rule ? t('routing.err.updateRule') : t('routing.err.addRule'),
    onDone,
  });

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
    const body = {
      name: name.trim(),
      severity: severity === '' ? null : severity,
      channel_ids: [...selected],
    };
    form.submit(() =>
      (rule ? api.updateRoutingRule(rule.id, body) : api.createRoutingRule(body)).then(() =>
        done(),
      ),
    );
  };

  return (
    <Modal
      title={
        rule ? t('routing.ruleModal.editTitle', { name: rule.name }) : t('routing.ruleModal.title')
      }
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={rule ? t('routing.ruleModal.save') : t('routing.ruleModal.add')}
          canSubmit={canAdd}
        />
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
      <FormError form={form} />
    </Modal>
  );
}
