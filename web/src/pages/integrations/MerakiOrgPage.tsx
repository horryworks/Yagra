// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Integrations ▸ Cisco Meraki ▸ <organization>. One organization's devices: what Meraki
// lists, which of it is monitored here, where each device is or would be filed, and how new ones
// become nodes (ADR-164 Inc.4/5).
//
// It replaced the import wizard. The wizard asked Meraki for a candidate list every time it opened
// and could only answer "import these now"; this page reads the inventory the periodic sync already
// keeps, so it costs no Dashboard API call to open, and it has somewhere to hold the setting that
// makes the import happen by itself.
//
// 🚨 **An import from here never says how to file.** `POST /meraki/import` reads an absent
// `file_by_prefix` as "the organization's own setting" — the one in the card above the table, which
// is also what the Destination column was computed under. Sending a value would let the press
// disagree with the column the operator just read.
//
// What it does say is which networks to start watching: the ones the chosen devices are in, unless
// the organization imports on its own (`networksToWatchOnImport`, 決定 16). A node in a network that
// is not watched is collected nothing for.
//
// All judgement is in `merakiDevices.ts` and `merakiOrgRow.ts`; Vitest never runs a `.tsx`
// (testing.md), so what is left here is layout, state and the calls.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link, useParams } from 'react-router-dom';
import { api, errMsg } from '../../services/api';
import { useCan } from '../../store';
import type { MerakiDevice, MerakiNetwork, MerakiOrg } from '../../types/api';
import { classifyLoadError, type LoadBlock } from '../../lib/loadState';
import { useClientFilters } from '../../lib/useClientFilters';
import { PageHeader } from '../../components/ui/PageHeader';
import { Card } from '../../components/ui/Card';
import { Button } from '../../components/ui/Button';
import { TextInput } from '../../components/ui/Field';
import { DataTable, type Column } from '../../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../../components/ui/TableToolbar';
import { ClearFilters } from '../../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../../components/ui/MobileFilterSheet';
import { LoadBlockNotice } from '../../components/ui/LoadBlockNotice';
import { EntityName } from '../../components/ui/EntityName';
import { useEntityNames } from '../../components/ui/entityNames';
import { MERAKI_PAGE_PATH, canSyncNow } from './merakiOrgRow';
import { merakiImportMessage } from './merakiImportResult';
import {
  MAX_DEVICES_MAX,
  MAX_DEVICES_MIN,
  MAX_DEVICES_RANGE,
  deviceDestination,
  devicesToImport,
  importableSerials,
  isImportable,
  merakiDeviceFilterColumns,
  modelSubLine,
  merakiDeviceFilters,
  networkLabel,
  networksToWatchOnImport,
  parseMaxDevices,
  pruneSelection,
  uncollectedDevices,
  unwatchedNotice,
} from './merakiDevices';
import { MerakiSyncButton, MerakiSyncStatus } from './MerakiSyncStatus';
import { useMerakiSync } from './useMerakiSync';
import './MerakiOrgPage.css';

/** How the organization's devices become nodes: by themselves or not, filed by IP range or not,
 *  and how many at most.
 *
 *  ⚠️ The form follows the server until the operator touches it (the shape `ApSettings` uses): a
 *  reload after "Sync now" would otherwise put the stored cap back under a half-typed one.
 *
 *  ⚠️ And it starts following again only once the reload a save asked for **has arrived**
 *  (`onSaved` resolves then). Lowering `edited` at the moment of the save handed the form back to
 *  an `org` that was still the old one, so what had just been saved flicked back to its previous
 *  value until the list came in — two round trips and the whole device list later. */
function ImportSettingsCard({
  org,
  canConfig,
  onSaved,
}: {
  org: MerakiOrg;
  canConfig: boolean;
  /** Reload the page's data; resolves when the new values are in. */
  onSaved: () => Promise<void>;
}) {
  const { t } = useTranslation('system');
  const [edited, setEdited] = useState(false);
  const [importDevices, setImportDevices] = useState(org.import_devices);
  const [fileByPrefix, setFileByPrefix] = useState(org.file_by_prefix);
  const [maxText, setMaxText] = useState(String(org.max_devices));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (edited) return;
    setImportDevices(org.import_devices);
    setFileByPrefix(org.file_by_prefix);
    setMaxText(String(org.max_devices));
  }, [edited, org.import_devices, org.file_by_prefix, org.max_devices]);

  const max = parseMaxDevices(maxText);

  const save = () => {
    if (max === null) return;
    setBusy(true);
    setError(null);
    api
      .setMerakiImportSettings(org.id, {
        import_devices: importDevices,
        file_by_prefix: fileByPrefix,
        max_devices: max,
      })
      // Still `busy` while the reload runs, so nothing can be typed into a form about to follow it.
      .then(() => onSaved())
      .then(() => setEdited(false))
      .catch((e: unknown) => setError(errMsg(e, t('meraki.err.saveImportSettings'))))
      .finally(() => setBusy(false));
  };

  // A reader sees the values and cannot change them; only the Save button is withheld (ADR-056).
  const locked = !canConfig || busy;

  return (
    <Card className="meraki-orgpage-settings" title={t('meraki.settings.title')}>
      <label className="meraki-orgpage-check">
        <input
          type="checkbox"
          checked={importDevices}
          disabled={locked}
          onChange={(e) => {
            setEdited(true);
            setImportDevices(e.target.checked);
          }}
        />
        <span className="meraki-orgpage-check-text">
          <span className="meraki-orgpage-check-label">{t('meraki.settings.importDevices')}</span>
          <span className="meraki-orgpage-check-hint">
            {t('meraki.settings.importDevicesHint')}
          </span>
        </span>
      </label>

      <label className="meraki-orgpage-check">
        <input
          type="checkbox"
          checked={fileByPrefix}
          disabled={locked}
          onChange={(e) => {
            setEdited(true);
            setFileByPrefix(e.target.checked);
          }}
        />
        <span className="meraki-orgpage-check-text">
          <span className="meraki-orgpage-check-label">{t('meraki.settings.fileByPrefix')}</span>
          <span className="meraki-orgpage-check-hint">
            {t('meraki.settings.fileByPrefixHint', { folder: org.name })}
          </span>
        </span>
      </label>

      <label className="form-label meraki-orgpage-max" htmlFor="meraki-max-devices">
        {t('meraki.settings.maxDevices')}
        <TextInput
          id="meraki-max-devices"
          type="number"
          min={MAX_DEVICES_MIN}
          max={MAX_DEVICES_MAX}
          value={maxText}
          disabled={locked}
          onChange={(e) => {
            setEdited(true);
            setMaxText(e.target.value);
          }}
        />
        <span className="form-hint">{MAX_DEVICES_RANGE}</span>
      </label>

      {edited && max === null && (
        <p className="form-error">
          {t('meraki.settings.maxDevicesInvalid', { min: MAX_DEVICES_MIN, max: MAX_DEVICES_MAX })}
        </p>
      )}
      {error && <p className="form-error">{error}</p>}
      {canConfig && (
        <div className="meraki-orgpage-save">
          <Button variant="primary" disabled={busy || !edited || max === null} onClick={save}>
            {t('common:actions.save')}
          </Button>
        </div>
      )}
    </Card>
  );
}

/** Where a device is, or where an import would put it, with the reason underneath. */
function DestinationCell({
  device,
  orgName,
  groupName,
}: {
  device: MerakiDevice;
  orgName: string;
  groupName: (id: string) => string;
}) {
  const { t } = useTranslation('system');
  const { destination, note } = deviceDestination(device);
  const under =
    destination.kind === 'network'
      ? t('meraki.devices.underNetwork', { folder: orgName, network: destination.networkName })
      : '';
  const why = note ? t(`meraki.devices.filing.${note.reason}`, note.args) : '';
  return (
    <span className="meraki-dev-stack">
      {destination.kind === 'folder' && (
        // The outer `title` carries the folder's *name*. `EntityName` puts the id in its own, on
        // purpose, and a name cut off by the column would otherwise be unreadable in full.
        <span className="meraki-dev-line" title={groupName(destination.folderId)}>
          <EntityName name={groupName(destination.folderId)} id={destination.folderId} />
        </span>
      )}
      {destination.kind === 'network' && (
        <span className="meraki-dev-line" title={under}>
          {under}
        </span>
      )}
      {destination.kind === 'root' && (
        <span className="meraki-dev-line muted">{t('meraki.devices.treeRoot')}</span>
      )}
      {note && (
        <span className="meraki-dev-sub" title={why}>
          {why}
        </span>
      )}
    </span>
  );
}

export function MerakiOrgPage() {
  const { t } = useTranslation('system');
  const { orgId = '' } = useParams();
  const canConfig = useCan('manage_config');
  const { groupName } = useEntityNames();

  const [org, setOrg] = useState<MerakiOrg | null>(null);
  const [devices, setDevices] = useState<MerakiDevice[]>([]);
  const [networks, setNetworks] = useState<MerakiNetwork[]>([]);
  const [pollingOn, setPollingOn] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [sheet, setSheet] = useState(false);

  // Returns its promise — it never rejects — so a caller can wait for the new values to be in
  // (`ImportSettingsCard` does, before it lets the form follow the server again).
  const load = useCallback((): Promise<void> => {
    return Promise.all([api.listMerakiOrgs(), api.getMerakiPolling()])
      .then(([list, polling]) => {
        const found = list.find((o) => o.id === orgId) ?? null;
        setOrg(found);
        setPollingOn(polling.enabled);
        setBlock(null);
        setLoadError(null);
        // The two reads below answer 404 for an organization that is not there, and "not found"
        // is already what the page is about to say.
        if (!found) return undefined;
        return Promise.all([api.listMerakiDevices(orgId), api.listMerakiNetworks(orgId)]).then(
          ([devs, nets]) => {
            setDevices(devs);
            setNetworks(nets);
            setSelected((prev) => pruneSelection(prev, devs));
          },
        );
      })
      .catch((e: unknown) => {
        const refused = classifyLoadError(e);
        setBlock(refused);
        if (!refused) setLoadError(errMsg(e, t('meraki.err.loadOrg')));
      })
      .finally(() => setLoaded(true));
  }, [orgId, t]);

  useEffect(() => {
    void load();
  }, [load]);

  const sync = useMerakiSync(orgId, t('meraki.err.sync'), load);

  const toggle = useCallback((serial: string, on: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(serial);
      else next.delete(serial);
      return next;
    });
  }, []);

  const orgName = org?.name ?? '';
  // What the list is filtered BY is held apart from what the table draws: the drawn columns change
  // identity on every tick (the checkbox cell reads `selected`), and a filter hook holding them
  // would re-filter and re-count every device per tick (`merakiDeviceFilterColumns`).
  const filterColumns = useMemo(() => merakiDeviceFilterColumns(merakiDeviceFilters(t)), [t]);
  const columns = useMemo(() => {
    const cols: Column<MerakiDevice>[] = [];
    if (canConfig) {
      cols.push({
        key: 'select',
        header: t('meraki.devices.cols.select'),
        width: '64px',
        // Only a row that can be imported has anything to tick. A node gets an empty cell rather
        // than a disabled box: a control nobody can use is not drawn (ADR-056).
        render: (d) =>
          isImportable(d) ? (
            <input
              type="checkbox"
              aria-label={t('meraki.devices.selectDevice', { name: d.name || d.serial })}
              checked={selected.has(d.serial)}
              disabled={busy}
              onChange={(e) => toggle(d.serial, e.target.checked)}
            />
          ) : null,
      });
    }
    cols.push(
      {
        key: 'name',
        header: t('meraki.devices.cols.name'),
        width: '1.6fr',
        render: (d) => {
          const name = d.name || d.serial;
          return (
            <span className="meraki-dev-stack">
              {d.node_id ? (
                <Link
                  className="meraki-dev-line meraki-dev-link"
                  to={`/nodes/${d.node_id}`}
                  title={name}
                >
                  {name}
                </Link>
              ) : (
                <span className="meraki-dev-line" title={name}>
                  {name}
                </span>
              )}
              <span className="meraki-dev-sub mono" title={d.serial}>
                {d.serial}
              </span>
            </span>
          );
        },
      },
      {
        key: 'model',
        header: t('meraki.devices.cols.model'),
        width: '1.1fr',
        render: (d) => (
          <span className="meraki-dev-stack">
            <span className="meraki-dev-line" title={d.model ?? undefined}>
              {d.model ?? '—'}
            </span>
            <span className="meraki-dev-sub" title={modelSubLine(d, t)}>
              {modelSubLine(d, t)}
            </span>
          </span>
        ),
      },
      {
        key: 'network',
        header: t('meraki.devices.cols.network'),
        width: '1.3fr',
        render: (d) => (
          <span className="meraki-dev-stack">
            <span className="meraki-dev-line" title={networkLabel(d)}>
              {networkLabel(d)}
            </span>
            {/* Under the name rather than beside it: a chip that never shrinks left a long network
                name three letters wide. */}
            {!d.network_monitored && (
              <span className="meraki-dev-sub meraki-dev-flag">
                {t('meraki.devices.notWatched')}
              </span>
            )}
          </span>
        ),
      },
      {
        key: 'address',
        header: t('meraki.devices.cols.address'),
        width: '1.1fr',
        render: (d) => (
          <span className="mono" title={d.lan_ip ?? undefined}>
            {d.lan_ip ?? '—'}
          </span>
        ),
      },
      {
        key: 'state',
        header: t('meraki.devices.cols.state'),
        width: '150px',
        // Words, never a dot: the integrations screens keep out of the node-state palette (a green
        // thing here would read as a node that is up), so the label is the whole signal.
        render: (d) => (
          <span title={t(`meraki.devices.state.${d.state}`)}>
            {t(`meraki.devices.state.${d.state}`)}
          </span>
        ),
      },
      {
        key: 'destination',
        header: t('meraki.devices.cols.destination'),
        width: '2fr',
        render: (d) => <DestinationCell device={d} orgName={orgName} groupName={groupName} />,
      },
    );
    // The same spec objects the hook filters by, hung on the columns the table draws them under.
    for (const { key, filter } of filterColumns) {
      const drawn = cols.find((c) => c.key === key);
      if (drawn) drawn.filter = filter;
    }
    return cols;
  }, [t, canConfig, selected, busy, toggle, orgName, groupName, filterColumns]);

  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    filterColumns,
    devices,
  );

  const importSelected = () => {
    if (!org) return;
    const chosen = devicesToImport(devices, selected);
    if (chosen.length === 0) return;
    setBusy(true);
    setActionError(null);
    setNote(null);
    // The chosen devices' networks start being watched with them — a node in an unwatched network
    // is collected nothing for. Not while the organization imports on its own (決定 16).
    const watch = networksToWatchOnImport(chosen, devices, org.import_devices);
    api
      // No `file_by_prefix`: absent means the organization's own setting (see the file header).
      .importMerakiDevices({
        org_uuid: org.id,
        devices: chosen,
        ...(watch.length > 0 ? { monitored_network_ids: watch } : {}),
      })
      .then((result) => {
        // Everything not filed by IP range went under the organization's own folder, which is
        // named after it.
        const parts = merakiImportMessage(result, org.name);
        setNote(parts.map((part) => t(part.key, part.args)).join(' '));
        setSelected(new Set());
        load();
      })
      .catch((e: unknown) => setActionError(errMsg(e, t('meraki.import.err.import'))))
      .finally(() => setBusy(false));
  };

  // The three below each walk the whole list, so none of them is recomputed by a tick.
  const unwatched = useMemo(() => (org ? unwatchedNotice(org, networks) : []), [org, networks]);
  const watchAll = () => {
    if (!org || unwatched.length === 0) return;
    setBusy(true);
    setActionError(null);
    api
      .setMerakiNetworksMonitored(org.id, unwatched, true)
      .then(load)
      .catch((e: unknown) => setActionError(errMsg(e, t('meraki.err.watchAll'))))
      .finally(() => setBusy(false));
  };

  // Nodes nothing is collected for (ADR-164 決定 15). Read from the device list rather than from
  // `org.devices.monitored_unwatched`: the button has to name the networks, and only the list has
  // them. The two are pinned to each other on the server (`meraki_sync.rs`).
  const uncollected = useMemo(() => uncollectedDevices(devices), [devices]);
  const watchThese = () => {
    if (!org || uncollected.networkIds.length === 0) return;
    setBusy(true);
    setActionError(null);
    api
      .setMerakiNetworksMonitored(org.id, uncollected.networkIds, true)
      .then(load)
      .catch((e: unknown) => setActionError(errMsg(e, t('meraki.err.watchAll'))))
      .finally(() => setBusy(false));
  };

  const selectable = useMemo(() => importableSerials(shown), [shown]);

  return (
    <div className="meraki-orgpage">
      <PageHeader
        title={org?.name ?? t('meraki.name')}
        trail={[
          { label: t('nav:sections.settings') },
          { label: t('nav:settings.integrations'), to: '/settings/integrations' },
          { label: t('meraki.name'), to: MERAKI_PAGE_PATH },
          ...(org ? [{ label: org.name }] : []),
        ]}
        note={t('meraki.orgPage.note')}
        actions={
          org && canConfig && canSyncNow(org, pollingOn) ? (
            <MerakiSyncButton sync={sync} />
          ) : undefined
        }
      />

      {block ? (
        <LoadBlockNotice block={block} unavailable={t('integrations.unavailable')} />
      ) : !loaded ? (
        <p className="muted">{t('common:loading')}</p>
      ) : loadError ? (
        <p className="form-error">{loadError}</p>
      ) : !org ? (
        <Card>
          <p className="muted">{t('meraki.orgPage.notFound')}</p>
          <p className="meraki-orgpage-back">
            <Link to={MERAKI_PAGE_PATH}>{t('meraki.orgPage.back')}</Link>
          </p>
        </Card>
      ) : (
        <>
          <div className="meraki-orgpage-status">
            <span className="meraki-orgpage-orgid mono">
              {t('meraki.orgs.orgId', { id: org.org_id })}
            </span>
            {/* Why "Sync now" is missing, when it is: both switches make the server refuse one, so
                the button is not drawn — and an absent button explains nothing by itself (R6). */}
            <span>
              {org.enabled ? t('meraki.orgs.stateEnabled') : t('meraki.orgs.statePaused')}
            </span>
            {!pollingOn && <span>{t('meraki.polling.paused')}</span>}
            <MerakiSyncStatus org={org} error={sync.error} />
          </div>

          <ImportSettingsCard org={org} canConfig={canConfig} onSaved={load} />

          {/* First, because it is the one about devices already being watched: they have gone
              quiet. Shown whether or not automatic import is on. */}
          {uncollected.count > 0 && (
            <div className="meraki-orgpage-notice meraki-orgpage-uncollected">
              <span>{t('meraki.settings.uncollected', { count: uncollected.count })}</span>
              {canConfig && (
                <Button variant="outline" onClick={watchThese} disabled={busy}>
                  {t('meraki.settings.watchThese')}
                </Button>
              )}
            </div>
          )}
          {unwatched.length > 0 && (
            <div className="meraki-orgpage-notice">
              <span>{t('meraki.settings.unwatched', { count: unwatched.length })}</span>
              {canConfig && (
                <Button variant="outline" onClick={watchAll} disabled={busy}>
                  {t('meraki.settings.watchAll')}
                </Button>
              )}
            </div>
          )}
          {org.devices_over_cap > 0 && (
            <div className="meraki-orgpage-notice">
              <span>{t('meraki.settings.overCap', { count: org.devices_over_cap })}</span>
            </div>
          )}

          <TableToolbar>
            <FilterButton columns={filterCols} filters={filters} onOpen={() => setSheet(true)} />
            <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
            {canConfig && selectable.length > 0 && (
              <Button
                variant="outline"
                disabled={busy}
                onClick={() => setSelected(new Set([...selected, ...selectable]))}
              >
                {t('meraki.devices.selectAll')}
              </Button>
            )}
            {canConfig && selected.size > 0 && (
              <Button variant="outline" disabled={busy} onClick={() => setSelected(new Set())}>
                {t('meraki.devices.clearSelection')}
              </Button>
            )}
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={anyFiltered ? devices.length : undefined}
              noun={t('meraki.devices.noun', {
                count: anyFiltered ? devices.length : shown.length,
              })}
            />
            {canConfig && selected.size > 0 && (
              <Button variant="primary" onClick={importSelected} disabled={busy}>
                {t('meraki.import.importBtn', { count: selected.size })}
              </Button>
            )}
          </TableToolbar>

          {actionError && <p className="form-error meraki-orgpage-line">{actionError}</p>}
          {note && <p className="meraki-orgpage-line meraki-orgpage-ok">✓ {note}</p>}

          <div className="meraki-orgpage-table">
            <DataTable
              tableId="settings.merakiDevices"
              rows={shown}
              columns={columns}
              filters={filters}
              onFiltersChange={setFilters}
              filterCounts={counts}
              rowKey={(d) => d.serial}
              empty={anyFiltered ? t('common:filter.noMatch') : t('meraki.devices.empty')}
            />
          </div>
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={{
                name: t('meraki.devices.cols.name'),
                network: t('meraki.devices.cols.network'),
                state: t('meraki.devices.cols.state'),
              }}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}
    </div>
  );
}
