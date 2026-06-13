// Routing & notifications (Alerts ▸ Routing & notifications). Two things: notification
// CHANNELS (where alerts can go — webhook/email; the connection config is a secret, sealed
// server-side and never returned) and routing RULES (which alerts, by severity, fan out to
// which channels). The notifier snapshots these (refreshed ~30s) so edits take effect live;
// any env-configured channel stays an always-on default route.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
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
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import './CrudList.css';
import './RoutingPage.css';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

const SEVERITY_TONE: Record<Severity, 'critical' | 'warning' | 'neutral'> = {
  critical: 'critical',
  warning: 'warning',
  info: 'neutral',
};

export function RoutingPage() {
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
        <PageHeader title="Routing & notifications" trail={[{ label: 'Alerts' }, { label: 'Routing & notifications' }]} />
        <Card>
          <p className="muted">Notification routing is unavailable in skeleton mode.</p>
        </Card>
      </div>
    );
  }

  return (
    <div>
      <PageHeader
        title="Routing & notifications"
        trail={[{ label: 'Alerts' }, { label: 'Routing & notifications' }]}
        note="Channels are where alerts go; rules pick which alerts (by severity) reach which channels."
      />
      {error && <p className="form-error">{error}</p>}
      <ChannelsCard
        channels={channels}
        authed={authed}
        loading={loading}
        onChange={load}
        onError={(m) => setError(m)}
      />
      <RulesCard
        rules={rules}
        channels={channels}
        authed={authed}
        loading={loading}
        onChange={load}
        onError={(m) => setError(m)}
      />
    </div>
  );
}

function ChannelsCard({
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
  const [name, setName] = useState('');
  const [kind, setKind] = useState<ChannelKind>('webhook');
  const [url, setUrl] = useState('');
  const [host, setHost] = useState('');
  const [from, setFrom] = useState('');
  const [to, setTo] = useState('');

  const reset = () => {
    setName('');
    setUrl('');
    setHost('');
    setFrom('');
    setTo('');
  };

  const add = () => {
    const config: ChannelConfigInput =
      kind === 'webhook'
        ? { kind: 'webhook', url: url.trim() }
        : { kind: 'email', host: host.trim(), from: from.trim(), to: to.trim() };
    api
      .createNotificationChannel({ name: name.trim(), config })
      .then(() => {
        reset();
        onChange();
      })
      .catch((e: unknown) => onError(errMsg(e, 'failed to add channel')));
  };

  const canAdd =
    name.trim() !== '' &&
    (kind === 'webhook' ? url.trim() !== '' : host.trim() !== '' && from.trim() !== '' && to.trim() !== '');

  return (
    <Card title="Notification channels">
      {authed && (
        <div className="routing-form form-row">
          <TextInput placeholder="name" value={name} onChange={(e) => setName(e.target.value)} />
          <Select value={kind} onChange={(e) => setKind(e.target.value as ChannelKind)}>
            <option value="webhook">webhook</option>
            <option value="email">email</option>
          </Select>
          {kind === 'webhook' ? (
            <TextInput
              className="routing-grow mono"
              placeholder="https://hooks.example/…"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
          ) : (
            <>
              <TextInput placeholder="smtp host" value={host} onChange={(e) => setHost(e.target.value)} />
              <TextInput placeholder="from" value={from} onChange={(e) => setFrom(e.target.value)} />
              <TextInput placeholder="to" value={to} onChange={(e) => setTo(e.target.value)} />
            </>
          )}
          <Button variant="primary" onClick={add} disabled={!canAdd}>
            Add channel
          </Button>
        </div>
      )}
      {channels.length === 0 ? (
        <p className="muted">
          {loading ? 'Loading…' : 'No channels yet. Add a webhook or email destination.'}
        </p>
      ) : (
        <div className="crud-list">
          {channels.map((c) => (
            <div className="crud-row" key={c.id}>
              <span className="crud-name">{c.name}</span>
              <Badge tone="neutral">{c.kind}</Badge>
              <Badge tone={c.enabled ? 'up' : 'neutral'}>{c.enabled ? 'enabled' : 'disabled'}</Badge>
              {authed && (
                <span className="routing-actions">
                  <Button
                    variant="ghost"
                    onClick={() =>
                      api
                        .setNotificationChannelEnabled(c.id, !c.enabled)
                        .then(onChange)
                        .catch((e: unknown) => onError(errMsg(e, 'failed to update')))
                    }
                  >
                    {c.enabled ? 'Disable' : 'Enable'}
                  </Button>
                  <Button
                    variant="ghost"
                    onClick={() =>
                      api
                        .deleteNotificationChannel(c.id)
                        .then(onChange)
                        .catch((e: unknown) => onError(errMsg(e, 'failed to delete')))
                    }
                  >
                    Delete
                  </Button>
                </span>
              )}
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}

function RulesCard({
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
  const [name, setName] = useState('');
  const [severity, setSeverity] = useState<'' | Severity>('');
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const toggle = (id: string) =>
    setSelected((cur) => {
      const next = new Set(cur);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const add = () => {
    api
      .createRoutingRule({
        name: name.trim(),
        severity: severity === '' ? null : severity,
        channel_ids: [...selected],
      })
      .then(() => {
        setName('');
        setSeverity('');
        setSelected(new Set());
        onChange();
      })
      .catch((e: unknown) => onError(errMsg(e, 'failed to add rule')));
  };

  const channelName = (id: string) => channels.find((c) => c.id === id)?.name ?? id;

  return (
    <Card title="Routing rules" className="routing-rules-card">
      {authed && (
        <div className="routing-form form-row">
          <TextInput placeholder="rule name" value={name} onChange={(e) => setName(e.target.value)} />
          <Select value={severity} onChange={(e) => setSeverity(e.target.value as '' | Severity)}>
            <option value="">any severity</option>
            <option value="critical">critical</option>
            <option value="warning">warning</option>
            <option value="info">info</option>
          </Select>
          <span className="routing-channel-picks">
            {channels.length === 0 ? (
              <span className="muted">add a channel first</span>
            ) : (
              channels.map((c) => (
                <label key={c.id} className="routing-pick">
                  <input type="checkbox" checked={selected.has(c.id)} onChange={() => toggle(c.id)} />
                  {c.name}
                </label>
              ))
            )}
          </span>
          <Button variant="primary" onClick={add} disabled={!name.trim() || selected.size === 0}>
            Add rule
          </Button>
        </div>
      )}
      {rules.length === 0 ? (
        <p className="muted">
          {loading
            ? 'Loading…'
            : 'No routing rules. Without a rule, alerts only use the env default route.'}
        </p>
      ) : (
        <div className="crud-list">
          {rules.map((r) => (
            <div className="crud-row" key={r.id}>
              <span className="crud-name">{r.name}</span>
              <Badge tone={r.severity ? SEVERITY_TONE[r.severity] : 'neutral'}>
                {r.severity ?? 'any'}
              </Badge>
              <span className="muted routing-rule-channels">
                → {r.channel_ids.map(channelName).join(', ') || '(no channels)'}
              </span>
              <Badge tone={r.enabled ? 'up' : 'neutral'}>{r.enabled ? 'enabled' : 'disabled'}</Badge>
              {authed && (
                <span className="routing-actions">
                  <Button
                    variant="ghost"
                    onClick={() =>
                      api
                        .setRoutingRuleEnabled(r.id, !r.enabled)
                        .then(onChange)
                        .catch((e: unknown) => onError(errMsg(e, 'failed to update')))
                    }
                  >
                    {r.enabled ? 'Disable' : 'Enable'}
                  </Button>
                  <Button
                    variant="ghost"
                    onClick={() =>
                      api
                        .deleteRoutingRule(r.id)
                        .then(onChange)
                        .catch((e: unknown) => onError(errMsg(e, 'failed to delete')))
                    }
                  >
                    Delete
                  </Button>
                </span>
              )}
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}
