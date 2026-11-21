// SPDX-License-Identifier: AGPL-3.0-only
// Notification delivery (Alerts ▸ Notification delivery). Three things: notification
// CHANNELS (where alerts can go — webhook/email; the connection config is a secret, sealed
// server-side and never returned) and routing RULES (which alerts, by severity, fan out to
// which channels). The notifier snapshots these (refreshed ~30s) so edits take effect live;
// any env-configured channel stays an always-on default route, shown as one line above the rules
// (ADR-200 Inc.28). After the rules sit the alert grouping (ADR-236, `AlertGroupingSection.tsx`)
// (ADR-236). A rule keeps its own hours since ADR-262; what is left of delivery hours is the
// default zone and the schedules an earlier version kept (`RoutingRulesFooter.tsx`). The delivery log is its own page
// under Alerts ▸ Monitor since ADR-262 (`DeliveryLog.tsx`); a channel row links to it.
//
// A `sections` page (ADR-246): each list is its toolbar, which carries the section's title, the
// count and "+ Add …" on one row, then its table, grouped in one `Stack`. Add via modal; enable/disable and delete sit in the row's
// menu; delete confirms in a modal. Channel kind and rule severity are neutral/status chips
// (categorical vs status).

import { useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import {
  type ChannelConfigInput,
  type ChannelKind,
  type ChannelTestResult,
  type NotificationChannel,
  type NotificationDefaultRoute,
  type NodeGroup,
  type NodeTags,
  type RoutingRule,
  type Severity,
} from '../types/api';
import { channelKindOptions } from '../lib/channelKinds';
import { PageHeader } from '../components/ui/PageHeader';
import { ClippedText } from '../components/ui/ClippedText';
import { Button } from '../components/ui/Button';
import { EmptyState } from '../components/ui/EmptyState';
import { Modal } from '../components/ui/Modal';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Field, TextInput, Select } from '../components/ui/Field';
import { CheckField, FieldGroup, Fields } from '../components/ui/FormLayout';
import { Inline, Page, Stack } from '../components/ui/Page';
import { Badge } from '../components/ui/Badge';
import { AppliesToCell, AppliesToPicker } from '../components/AppliesTo/AppliesTo';
import {
  appliesToBody,
  appliesToFrom,
  namesNothing,
  type AppliesTo,
} from '../components/AppliesTo/appliesToTarget';
import { groupOptions } from '../lib/nodeTree';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { ListToolbar } from '../components/ui/ListToolbar';
import { columnLabels } from '../lib/listToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { useClientFilters } from '../lib/useClientFilters';
import { useClientSort } from '../lib/useClientSort';
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
import { AlertGroupingSection } from './AlertGroupingSection';
import { CatchAllLine, DefaultZoneSelect, OldSchedules } from './RoutingRulesFooter';
import { RoutingCoverage } from './RoutingCoverage';
import { WEEKDAYS, weekdayName } from './deliveryHours';
import { WHEN_MODES, whenBodyFrom, whenDraftFrom, whenText, type WhenDraft } from './ruleWhen';
import { TimeOfDayInput } from '../components/ui/WallTimeInput';
import { Segmented } from '../components/ui/Segmented';
import { defaultRouteChannels } from './defaultRoute';
import { deliveryLogFor } from './deliveryLogQuery';

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
  // Apart from the read above: the default route is a fact about this core's environment, and a
  // failure to read it must not take the rule list with it. It stays `null`, and nothing is drawn.
  const { data: defaultRoute } = useLoad<NotificationDefaultRoute | null>(
    () => api.getNotificationDefaultRoute(),
    [],
    { initial: null },
  );
  // The tags nodes carry (ADR-259 Inc.2): what a rule's tags are picked from, and what marks a
  // rule tag nobody carries. Apart from the rule read for the reason the default route is: a
  // failure leaves it `null`: nothing is marked, and the rule dialog lists only the tags the rule
  // already names (so a save keeps them) - there is no free entry, so a NEW tag cannot be picked
  // until the list reads again.
  const { data: nodeTags } = useLoad<NodeTags | null>(() => api.listNodeTags(), [rules], {
    initial: null,
  });
  // The folders a rule may name (ADR-262), apart for the same reason: a failure leaves the list
  // empty, so the dialog offers no folder and the list shows a named one as gone.
  const { data: groups } = useLoad<NodeGroup[]>(() => api.listNodeGroups(), [rules], {
    initial: [],
  });

  // A channel row opens the delivery log narrowed to that channel (ADR-262 decision 7).
  const navigate = useNavigate();
  const showLog = (channelId: string) => navigate(deliveryLogFor(channelId));

  return (
    <Page layout="sections">
      <PageHeader
        title={t('nav:alerts.routing')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.routing') }]}
      />
      <LoadGate load={routing} permission="manage_system">
        {error && <p className="form-error">{error}</p>}
        <ChannelsSection
          channels={channels}
          canSystem={canSystem}
          loading={loading}
          onChange={load}
          onError={setError}
          onShowLog={showLog}
        />
        <OldSchedules groups={groups} canSystem={canSystem} />
        <RulesSection
          rules={rules}
          nodeTags={nodeTags}
          groups={groups}
          defaultRoute={defaultRoute}
          channels={channels}
          canSystem={canSystem}
          loading={loading}
          onChange={load}
          onError={setError}
        />
        <RoutingCoverage rules={rules} channels={channels} />
        <AlertGroupingSection canSystem={canSystem} onError={setError} />
      </LoadGate>
    </Page>
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
        render: (c) => <ClippedText className="yt-name-txt" text={c.name} />,
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
  const sorting = useClientSort(columns, filtering.shown, { prefix: CHANNEL_FILTER_PREFIX });

  return (
    <Stack as="section" gap={12}>
      <ListToolbar
        title={t('routing.channels.title')}
        list={filtering}
        labels={columnLabels(columns)}
        count={{
          shown: shown.length,
          total: channels.length,
          noun: (n) => t('noun.channel', { count: n }),
        }}
      >
        {canSystem && (
          <Button variant="primary" onClick={() => setAdding(true)}>
            {t('routing.channels.add')}
          </Button>
        )}
      </ListToolbar>

      <DataTable
        tableId="settings.notificationChannels"
        rows={sorting.rows}
        columns={sorting.columns}
        sort={sorting.sort}
        onSortChange={sorting.setSort}
        rowKey={(c) => c.id}
        rowClass={(c) => (c.enabled ? undefined : 'is-muted')}
        filters={filters}
        onFiltersChange={setFilters}
        filterCounts={counts}
        loading={loading}
        empty={
          anyFiltered ? (
            t('common:filter.noMatch')
          ) : (
            <EmptyState
              text={t('routing.channels.empty')}
              action={
                canSystem && (
                  <Button variant="primary" onClick={() => setAdding(true)}>
                    {t('routing.channels.add')}
                  </Button>
                )
              }
            />
          )
        }
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
    </Stack>
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
      <Stack gap={12}>
        {/* Only what the test will do that the operator cannot see from here: it pages someone,
            the payload is not marked as a test, the channel is off. Each is one sentence
            (ADR-200). */}
        {warn.pages && <p className="routing-test-warn">{t('routing.test.pages')}</p>}
        {warn.unmarkedBody && (
          <p className="routing-test-warn">{t('routing.test.unmarkedBody')}</p>
        )}
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
      </Stack>
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
      <Fields>
        <Field label={t('routing.channelModal.name')}>
          <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </Field>
        <Field label={t('routing.channelModal.kind')}>
          <Select value={kind} onChange={(e) => setKind(e.target.value as ChannelKind)}>
            {/* Derived from `CHANNEL_KINDS`, which is what its doc comment always claimed and what
                nothing actually did — these were four `<option>` literals, so the union and the
                list an operator can pick from were two copies. A fifth kind is now a compile error
                in `lib/channelKinds.ts` rather than an option nobody adds. */}
            {channelKindOptions().map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </Select>
        </Field>
        {kind === 'webhook' && (
          <Field label={t('routing.channelModal.webhookUrl')}>
            <TextInput
              className="mono"
              placeholder="https://hooks.example/…"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
          </Field>
        )}
        {kind === 'email' && (
          <>
            <Field label={t('routing.channelModal.smtpHost')}>
              <TextInput value={host} onChange={(e) => setHost(e.target.value)} />
            </Field>
            <Field label={t('routing.channelModal.from')}>
              <TextInput value={from} onChange={(e) => setFrom(e.target.value)} />
            </Field>
            <Field label={t('routing.channelModal.to')}>
              <TextInput value={to} onChange={(e) => setTo(e.target.value)} />
            </Field>
          </>
        )}
        {kind === 'pagerduty' && (
          <>
            <Field label={t('routing.channelModal.routingKey')}>
              <TextInput
                className="mono"
                placeholder="R0XXXXXXXXXXXXXXXXXXXXXXXXX"
                value={routingKey}
                onChange={(e) => setRoutingKey(e.target.value)}
              />
            </Field>
            <Field label={t('routing.channelModal.region')}>
              <Select
                value={pdRegion}
                onChange={(e) => setPdRegion(e.target.value as 'us' | 'eu')}
              >
                <option value="us">{t('routing.channelModal.regionUs')}</option>
                <option value="eu">{t('routing.channelModal.regionEu')}</option>
              </Select>
            </Field>
          </>
        )}
        {kind === 'jsm' && (
          <>
            <Field label={t('routing.channelModal.apiUrl')}>
              <TextInput
                className="mono"
                value={jsmUrl}
                onChange={(e) => setJsmUrl(e.target.value)}
              />
            </Field>
            <Field label={t('routing.channelModal.apiKey')}>
              <TextInput
                className="mono"
                value={jsmKey}
                onChange={(e) => setJsmKey(e.target.value)}
              />
            </Field>
          </>
        )}
        <FormError form={form} />
      </Fields>
    </Modal>
  );
}

// ── Rules ────────────────────────────────────────────────────────────────────

function RulesSection({
  rules,
  nodeTags,
  groups,
  defaultRoute,
  channels,
  canSystem,
  loading,
  onChange,
  onError,
}: {
  rules: RoutingRule[];
  nodeTags: NodeTags | null;
  groups: NodeGroup[];
  defaultRoute: NotificationDefaultRoute | null;
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
    const paths = new Map(groupOptions(groups).map((o) => [o.id, o.path]));
    const folderPath = (id: string) => paths.get(id) ?? id;
    const specs = routingRuleFilters(t, severityLabel, folderPath);
    const cols: Column<RoutingRule>[] = [
      {
        key: 'name',
        header: t('routing.rules.cols.name'),
        width: '1.4fr',
        render: (r) => <ClippedText className="yt-name-txt" text={r.name} />,
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
        key: 'target',
        header: t('appliesTo.title'),
        width: '1.2fr',
        sortValue: (r) =>
          [...(r.group_ids ?? []).map(folderPath), ...r.tags].join(', ') || null,
        render: (r) => (
          <AppliesToCell
            groupIds={r.group_ids ?? []}
            tags={r.tags}
            join={r.join ?? 'or'}
            groups={groups}
            nodeTags={nodeTags}
          />
        ),
      },
      {
        key: 'when',
        header: t('routing.rules.cols.when'),
        width: '1fr',
        sortValue: (r) => whenText(t, r.when),
        render: (r) => <ClippedText text={whenText(t, r.when)} />,
      },
      {
        key: 'channels',
        sortValue: (r) => r.channel_ids.map(channelName).join(', ') || null,
        header: t('routing.rules.cols.channels'),
        width: '1fr',
        render: (r) => {
          const names = r.channel_ids.map(channelName).join(', ') || t('routing.rules.noChannels');
          // The whole list on hover: a rule may name more channels than the column can show.
          return <ClippedText className="muted" text={names} />;
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
  }, [t, canSystem, channels, nodeTags, groups]);

  // In the URL, under `rules.` — see the channels table above.
  const filtering = useClientFilters(columns, rules, { prefix: ROUTING_RULE_FILTER_PREFIX });
  const { filters, setFilters, shown, counts, anyFiltered } = filtering;
  const sorting = useClientSort(columns, filtering.shown, { prefix: ROUTING_RULE_FILTER_PREFIX });

  return (
    <Stack as="section" gap={12}>
      <ListToolbar
        title={t('routing.rules.title')}
        list={filtering}
        labels={columnLabels(columns)}
        count={{
          shown: shown.length,
          total: rules.length,
          noun: (n) => t('common:noun.rule', { count: n }),
        }}
      >
        {/* Not drawn rather than disabled while there is no channel (ADR-056): a rule names the
            channels it sends to, and the empty list below says what comes first (ADR-209). */}
        <DefaultZoneSelect canSystem={canSystem} onError={onError} />
        {canSystem && channels.length > 0 && (
          <Button variant="primary" onClick={() => setAdding(true)}>
            {t('routing.rules.add')}
          </Button>
        )}
      </ListToolbar>

      <DataTable
        tableId="settings.routingRules"
        rows={sorting.rows}
        columns={sorting.columns}
        sort={sorting.sort}
        onSortChange={sorting.setSort}
        rowKey={(r) => r.id}
        rowClass={(r) => (r.enabled ? undefined : 'is-muted')}
        filters={filters}
        onFiltersChange={setFilters}
        filterCounts={counts}
        loading={loading}
        empty={
          anyFiltered ? (
            t('common:filter.noMatch')
          ) : channels.length === 0 ? (
            t('routing.rules.emptyNoChannels')
          ) : (
            <EmptyState
              text={t('routing.rules.empty')}
              action={
                canSystem && (
                  <Button variant="primary" onClick={() => setAdding(true)}>
                    {t('routing.rules.add')}
                  </Button>
                )
              }
            />

          )
        }
      />

      {/* The two lines after the last rule (ADR-262 decision 5): where an alert goes when no rule
          that matches it is open, and the environment's route, sent every alert whatever the rules
          say. Lines rather than pinned rows, because the rules table is filtered and counted in the
          browser and a row that is not a rule would be filtered and counted with them. */}
      <CatchAllLine channels={channels} canSystem={canSystem} />
      {defaultRoute && (
        <Inline as="p" gap={8} className="routing-default-route">
          <span className="yt-name-txt">{t('routing.rules.alwaysAlso')}</span>
          <Badge tone="neutral">{t('routing.rules.defaultRoute.allAlerts')}</Badge>
          <span className="muted">
            {defaultRouteChannels(defaultRoute) ?? t('routing.rules.defaultRoute.notSet')}
          </span>
        </Inline>
      )}

      {adding && (
        <RuleModal
          channels={channels}
          nodeTags={nodeTags}
          groups={groups}
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
          nodeTags={nodeTags}
          groups={groups}
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
    </Stack>
  );
}

/** Add a routing rule, or — given `rule` — edit one in place (ADR-193). The edit replaces the name,
 *  severity, the folders and tags it applies to (ADR-259, ADR-262) and channels, and leaves the
 *  rule's on/off switch as it was. */
function RuleModal({
  rule,
  channels,
  nodeTags,
  groups,
  onClose,
  onDone,
}: {
  rule?: RoutingRule;
  channels: NotificationChannel[];
  nodeTags: NodeTags | null;
  groups: NodeGroup[];
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [name, setName] = useState(rule?.name ?? '');
  const [severity, setSeverity] = useState<'' | Severity>(rule?.severity ?? '');
  // Built once from the rule as it was opened, so a row unticked under the pointer stays put.
  const [initial] = useState<AppliesTo>(() =>
    appliesToFrom(rule?.group_ids ?? [], rule?.tags ?? [], rule?.join),
  );
  const [target, setTarget] = useState<AppliesTo>(initial);
  const [when, setWhen] = useState<WhenDraft>(() => whenDraftFrom(rule?.when));
  const whenBody = whenBodyFrom(when);
  const toggleDay = (d: number) =>
    setWhen((w) => ({
      ...w,
      days: w.days.includes(d) ? w.days.filter((x) => x !== d) : [...w.days, d],
    }));
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

  const canAdd =
    name.trim() !== '' && selected.size > 0 && !namesNothing(target) && 'when' in whenBody;

  const submit = () => {
    if (!canAdd) return;
    const body = {
      name: name.trim(),
      severity: severity === '' ? null : severity,
      ...appliesToBody(target),
      channel_ids: [...selected],
      when: 'when' in whenBody ? whenBody.when : undefined,
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
      <Fields>
        <Field label={t('routing.ruleModal.name')}>
          <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </Field>
        <Field label={t('routing.ruleModal.severity')}>
          <Select value={severity} onChange={(e) => setSeverity(e.target.value as '' | Severity)}>
            <option value="">{t('routing.ruleModal.anySeverity')}</option>
            <option value="critical">{severityLabel('critical')}</option>
            <option value="warning">{severityLabel('warning')}</option>
            <option value="info">{severityLabel('info')}</option>
          </Select>
        </Field>
        {/* "Every node" is a choice the operator makes, not what an empty list happens to mean
            (ADR-259 decision 17). The API still spells it as no folders and no tags. */}
        <AppliesToPicker
          label={t('appliesTo.title')}
          value={target}
          onChange={setTarget}
          groups={groups}
          nodeTags={nodeTags}
          initial={initial}
          allowAll
        />
        <FieldGroup
          label={t('routing.ruleModal.when')}
          error={'error' in whenBody ? t(`routing.when.err.${whenBody.error}`) : undefined}
        >
          <Segmented
            options={WHEN_MODES.map((m) => ({ value: m, label: t(`routing.when.mode.${m}`) }))}
            value={when.mode}
            onChange={(v) => {
              const mode = WHEN_MODES.find((m) => m === v);
              if (mode) setWhen((w) => ({ ...w, mode }));
            }}
          />
          {when.mode !== 'always' && (
            <>
              <Inline gap={12}>
                {WEEKDAYS.map((d) => (
                  <CheckField
                    key={d}
                    checked={when.days.includes(d)}
                    onChange={() => toggleDay(d)}
                    label={weekdayName(t, d)}
                  />
                ))}
              </Inline>
              <Inline gap={8}>
                <TimeOfDayInput
                  id="rule-when-start"
                  aria-label={t('routing.when.from')}
                  value={when.start}
                  onChange={(start) => setWhen((w) => ({ ...w, start }))}
                />
                <span>–</span>
                <TimeOfDayInput
                  id="rule-when-end"
                  aria-label={t('routing.when.until')}
                  value={when.end}
                  onChange={(end) => setWhen((w) => ({ ...w, end }))}
                />
              </Inline>
            </>
          )}
        </FieldGroup>
        <FieldGroup label={t('routing.ruleModal.channels')}>
          <Inline gap={12}>
            {channels.map((c) => (
              <CheckField
                key={c.id}
                checked={selected.has(c.id)}
                onChange={() => toggle(c.id)}
                label={c.name}
              />
            ))}
          </Inline>
        </FieldGroup>
        <FormError form={form} />
      </Fields>
    </Modal>
  );
}
