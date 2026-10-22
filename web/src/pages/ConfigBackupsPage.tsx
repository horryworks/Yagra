// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Config backups (ADR-210 stage 3): every node whose configuration the bundled oxidized
// backs up, and how that is going — the login that works or is being tried, the last attempt, the
// last success, and when the lockout limits allow the next try. A node's own settings are its
// Config tab; this list is where to see at a glance which nodes are backed up and which need
// attention. The state word is `lib/configBackupMarks.ts`, the same one the tree's mark draws.

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../services/api';
import { useCan } from '../store';
import type { ConfigBackupRow } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { DataTable, type Column } from '../components/ui/DataTable';
import { EmptyState } from '../components/ui/EmptyState';
import { EntityName } from '../components/ui/EntityName';
import { useEntityNames } from '../components/ui/entityNames';
import { TimeCell } from '../components/ui/tableCells';
import { useLoad } from '../lib/useLoad';
import { LoadGate } from '../components/ui/LoadGate';
import { nodeHref } from '../lib/entityHref';
import { isCliCredentialKind } from '../lib/credentialKinds';
import { markOf } from '../lib/configBackupMarks';
import { attemptLine, loginLabel } from '../components/NodeDetail/configBackupForm';
import './ConfigBackupsPage.css';

export function ConfigBackupsPage() {
  const { t } = useTranslation('nodes');
  const canCreds = useCan('manage_credentials');
  const { nodeName } = useEntityNames();
  const list = useLoad(() => api.listConfigBackups(), [], {
    initial: [] as ConfigBackupRow[],
    intervalMs: 30_000,
    errorFallback: t('config.err.list'),
  });
  const creds = useLoad(() => api.listCredentials(), [], { initial: [], enabled: canCreds });
  const names = useMemo(
    () =>
      new Map(creds.data.filter((c) => isCliCredentialKind(c.kind)).map((c) => [c.id, c.name])),
    [creds.data],
  );

  const columns = useMemo<Column<ConfigBackupRow>[]>(() => {
    const login = (id: string | null | undefined) => {
      const l = loginLabel(id, names, []);
      return l.kind === 'name' ? l.name : '—';
    };
    return [
      {
        key: 'node',
        header: t('config.list.cols.node'),
        width: '1fr',
        render: (r) => (
          <EntityName name={nodeName(r.node_id)} id={r.node_id} to={`${nodeHref(r.node_id)}?tab=config`} />
        ),
      },
      {
        key: 'state',
        header: t('config.list.cols.state'),
        width: '150px',
        render: (r) => {
          const mark = markOf(r);
          return <span className={`cfgbl-mark cfgbl-mark-${mark}`}>{t(`config.list.mark.${mark}`)}</span>;
        },
      },
      {
        key: 'last',
        header: t('config.list.cols.last'),
        width: '1.4fr',
        render: (r) => {
          if (!r.last) return '—';
          const line = attemptLine(r.last);
          return t(line.key, { login: login(line.login) });
        },
      },
      ...(canCreds
        ? [
            {
              key: 'login',
              header: t('config.list.cols.login'),
              width: '160px',
              render: (r: ConfigBackupRow) => login(r.credential_id),
            },
          ]
        : []),
      {
        key: 'success',
        header: t('config.list.cols.success'),
        width: '200px',
        render: (r) => (r.last_success_at ? <TimeCell iso={r.last_success_at} /> : '—'),
      },
      {
        key: 'next',
        header: t('config.list.cols.next'),
        width: '200px',
        render: (r) => (r.try_after ? <TimeCell iso={r.try_after} /> : '—'),
      },
    ];
  }, [t, nodeName, names, canCreds]);

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.configBackups')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.configBackups') }]}
      />
      <LoadGate load={list}>
        <DataTable
          tableId="nodes.configBackups"
          rows={list.data}
          columns={columns}
          rowKey={(r) => r.node_id}
          loading={list.loading}
          empty={list.error ?? <EmptyState text={t('config.list.empty')} />}
        />
      </LoadGate>
    </div>
  );
}
