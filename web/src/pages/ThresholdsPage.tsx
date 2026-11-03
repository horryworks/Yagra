// SPDX-License-Identifier: AGPL-3.0-only
// Threshold rules (Alerts ▸ Threshold rules). Thresholds resolve by hierarchical
// override — profile → group → node, most-specific wins (§3.3) — so each rule carries a
// scope level + id. CRUD against /thresholds. Rules are evaluated live: the alert engine
// snapshots them (refreshed every ~30s) and checks each matching poll sample through the
// same hysteresis/flapping machinery as liveness, so a breach fires a real alert.
//
// Data-table standard v2: a toolbar (count + "+ Add rule") over the shared `DataTable`; the
// add form and delete confirmation both go through modals. "The most specific scope wins" is not a
// paragraph above the table any more (ADR-200 Inc.14): a row that a narrower rule takes over from
// carries a pressable "Overridden on N nodes" badge instead, counted by the server (Inc.29).
//
// This is the one configuration list that grows with the fleet — a node-level override is per
// (node × metric) — so it uses the virtualized `DataTable` rather than a hand-rolled grid, and the
// server caps the response. When the cap bites, the toolbar says so: a silently short ruleset reads
// as "these are all the rules", which is exactly the wrong belief to hold about alerting config.
//
// **Since ADR-230 it is the one place a threshold rule is made or changed.** The level tabs and the
// target picker say where in the five levels the operator is looking; "Check a node" shows what
// reaches one node and which rule wins there. Node detail, Device profiles and the folder pane link
// here with the view in the URL (`thresholdQuery.ts::thresholdRulesHref`) instead of keeping lists
// of their own.

import { useCallback, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { Trans, useTranslation } from 'react-i18next';
import { api } from '../services/api';
import { useCan } from '../store';
import type { StoredThreshold, ThresholdPage } from '../types/api';
import { LIVENESS_METRIC } from '../lib/format';
import { metricMeaningKey } from '../lib/metricMeaning';
import { boundSides } from '../lib/thresholdBounds';
import { ThresholdModal } from '../components/ThresholdModal/ThresholdModal';
import { RuleBounds } from '../components/ThresholdModal/RuleBounds';
import { splitInterfaceScopeId } from '../lib/interfaceScope';
import { overriddenRows } from './thresholdOverrides';
import { PageHeader } from '../components/ui/PageHeader';
import { EmptyState } from '../components/ui/EmptyState';
import { InfoPress } from '../components/ui/InfoTip';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Badge } from '../components/ui/Badge';
import { EntityName } from '../components/ui/EntityName';
import { useEntityNames } from '../components/ui/entityNames';
import { IconButton } from '../components/ui/IconButton';
import { EditIcon } from '../components/ui/icons';
import { isAnyFiltered, specColumns } from '../lib/columnFilter';
import { useFilterParams } from '../lib/useFilterParams';
import { ListToolbar } from '../components/ui/ListToolbar';
import { serverToolbarFilters } from '../lib/listToolbar';
import {
  SCOPE_TABS,
  queryFor,
  ruleViewFrom,
  startFormFor,
  targetKind,
  thresholdFilters,
  writeRuleView,
  type RuleView,
} from './thresholdQuery';
import { Tabs } from '../components/ui/Tabs';
import { Select } from '../components/ui/Field';
import { GroupPicker } from '../components/ui/GroupPicker';
import { NodePicker } from '../components/NodePicker/NodePicker';
import { NodeRulesView } from '../components/NodeRules/NodeRulesView';
import { groupOptions } from '../lib/nodeTree';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TrashIcon } from '../components/ui/icons';
import './ThresholdsPage.css';
import { useLoad } from '../lib/useLoad';
import { LoadGate } from '../components/ui/LoadGate';

/** Confirm + delete a threshold rule (destructive-consent modal). */
function DeleteThresholdModal({
  rule,
  portNames,
  onClose,
  onDone,
}: {
  rule: StoredThreshold;
  portNames: Readonly<Record<string, string>>;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  // Its own resolver: this dialog is a separate component from the list, and the sentence it
  // shows names the same targets the row does. A raw UUID on a destructive confirmation is the
  // one place `no-raw-uuids-in-tables` matters most — the operator is being asked to be sure.
  const { scopeName } = useEntityNames();
  return (
    <ConfirmDeleteModal
      title={t('thresholds.deleteModal.title')}
      onConfirm={() => api.deleteThreshold(rule.id)}
      errorFallback={t('thresholds.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      {/* A global rule has no target, and the shared sentence used to interpolate the empty
          string straight into "… for <mono></mono>?" — an empty pair of quotes where the target
          belongs, on a destructive confirmation. Two sentences rather than a placeholder that is
          sometimes blank. Since ADR-078 the non-global sentence may name several, comma-joined. */}
      <Trans
        t={t}
        i18nKey={
          rule.scope_level === 'global'
            ? 'thresholds.deleteModal.bodyGlobal'
            : 'thresholds.deleteModal.body'
        }
        values={{
          level: t(`thresholds.scopeLevel.${rule.scope_level}`),
          metric: rule.metric === LIVENESS_METRIC ? t('format:liveness') : rule.metric,
          scope: rule.scope_ids
            .map((id) => scopeName(rule.scope_level, id, portNames))
            .join(', '),
        }}
        components={{ strong: <strong />, mono: <strong className="mono" /> }}
      />
    </ConfirmDeleteModal>
  );
}

/** How many targets the Scope cell draws before it falls back to counting (ADR-078 Inc.5).
 *
 *  The API accepts 32 (`MAX_SCOPE_IDS`), and a row that long would be a screenful on its own. The
 *  seeded maximum is 5 — `cisco_cpu_5min`, which every Cisco health template publishes — so this
 *  only ever engages on a rule an operator built by hand. ⚠️ 8 is a judgement, not a measurement. */
const MAX_SCOPE_LINES = 8;

export function ThresholdsPage() {
  const { t } = useTranslation('alertsConfig');
  const canConfig = useCan('manage_config');
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<StoredThreshold | null>(null);
  const [deleting, setDeleting] = useState<StoredThreshold | null>(null);
  const { scopeName, nodeName, profiles, groups } = useEntityNames();
  // The level, the target and the node being checked (ADR-230). Written beside the column filters
  // into one query string; each writer starts from the current params, so neither erases the other.
  const [searchParams, setSearchParams] = useSearchParams();
  const view = useMemo(() => ruleViewFrom(searchParams), [searchParams]);
  const setView = useCallback(
    (next: RuleView) => {
      const params = new URLSearchParams(searchParams);
      writeRuleView(params, next);
      setSearchParams(params, { replace: true });
    },
    [searchParams, setSearchParams],
  );
  /** "Check a node" was pressed and no node is chosen yet — the picker is showing. */
  const [checking, setChecking] = useState(false);
  const folderItems = useMemo(() => groupOptions(groups), [groups]);

  // ⚠️ **`filterCols` comes from the specs, not from `filterableColumns(columns)`.**
  // `useFilterParams` derives the filter state from whatever list it is given, `load` depends on
  // that state, and an effect depends on `load` — so the list has to be stable. The display columns
  // are not: they close over `scopeName`, which changes identity every time a name batch resolves,
  // and the ruleset would be refetched at that moment. The specs depend on `t` alone.
  const specs = useMemo(() => thresholdFilters(t), [t]);
  const filterCols = useMemo(() => specColumns(specs), [specs]);

  // The filters live in the URL — nothing else holds them, so a narrowed ruleset survives a
  // reload and can be shared. Since Inc.10 that is the shared codec (`useFilterParams`) rather
  // than a per-field `readFilters`/`writeFilters` pair: the column key **is** the query key, so
  // the bookmarks taken before this change still resolve. `replace: true` lives in the hook,
  // because a settled filter is not a place you navigated to.
  const { filters, setFilters } = useFilterParams(filterCols);
  const filtered = isAnyFiltered(filterCols, filters);

  // Refetch whenever the filter changes: the predicate runs in the database, so a browser-side
  // narrowing would only ever examine the 500 rules already on screen — which is the whole
  // reason this screen filters server-side (see `thresholdQuery.ts`).
  // The only call that asks for the override count: the two `total` questions below would pay a
  // fleet walk each and throw it away (ADR-200 Inc.33).
  const ruleset = useLoad(
    () => api.listThresholds({ ...queryFor(filterCols, filters, view), overridden: true }),
    // The tab and the target, not `view`: a new object on every URL write, the check included.
    [filterCols, filters, view.tab, view.target],
    {
      initial: {
        items: [],
        total: 0,
        truncated: false,
        overridden: {},
        port_names: {},
      } as ThresholdPage,
    },
  );
  const { data: page, loading, reload: rulesetReload } = ruleset;
  const rows = page.items;
  /** A port rule's port by its name, as the alert it raises names it (ADR-196 decision 6). */
  const portNames = page.port_names;
  // The whole target list, for the cell's `title`. Two names are drawn; this is what makes the
  // other two readable rather than merely counted.
  // `useCallback` because the columns memo below closes over it. Its inputs are `scopeName` and
  // the page's port names, which that memo already depends on — pinning the identity to the same
  // things keeps the memo recomputing exactly when it did before.
  const scopeTitle = useCallback(
    (row: StoredThreshold) =>
      row.scope_ids.map((id) => scopeName(row.scope_level, id, portNames)).join(', '),
    [scopeName, portNames],
  );
  /** On how many nodes a narrower rule on the same metric takes over from each row — counted by
   *  the server across the whole fleet (ADR-200 Inc.29), read in `thresholdOverrides.ts`. */
  const overridden = useMemo(() => overriddenRows(page), [page]);

  const columns = useMemo<Column<StoredThreshold>[]>(() => {
    const cols: Column<StoredThreshold>[] = [
      {
        // Two values, two columns (ADR-075 Inc.2). They shared a cell until an operator asked
        // which of the two things in it was the scope id — every other column here is one value
        // under a heading that names it, and this one was not. The headings are the add dialog's
        // field names verbatim, so "where do I set this?" is answered by the words matching.
        key: 'scope_level',
        header: t('thresholds.cols.scope'),
        width: '110px',
        render: (row) => <Badge tone="neutral">{t(`thresholds.scopeLevel.${row.scope_level}`)}</Badge>,
      },
      {
        // Not filterable, and the absence is the declaration: this list is capped at 500 rules
        // server-side, so a browser-side predicate would examine that prefix and report on it —
        // see `thresholdQuery.ts`. The API has no `scope_id` parameter to push it into.
        key: 'scope_id',
        header: t('thresholds.cols.scopeId'),
        // Wider since Inc.5, taken from the meaning column: stacking makes each line short, and
        // the width is what decides whether a profile name ends in an ellipsis at 1280px.
        width: '1.6fr',
        render: (row) => {
          // Where a narrower rule on the same metric takes over from this one (ADR-200 Inc.14) —
          // the page note "the most specific scope wins", said on the row that loses.
          const over = overridden.get(row.id);
          const badge = over && (
            <InfoPress
              infoKey="alertsConfig:thresholds.overridden.info"
              className="badge badge-neutral"
            >
              {t('thresholds.overridden.nodes', { count: over })}
            </InfoPress>
          );
          const target =
            // A global rule has no target to resolve — the type column already says "every node",
            // and `EntityName` on an empty id renders a bare em dash with no explanation of why.
            row.scope_level === 'global' || row.scope_ids.length === 0 ? (
              <span className="muted" title={t('thresholds.scopeIdNone')}>
                —
              </span>
            ) : (
              // Since ADR-078 a rule may name several, and Inc.5 draws them ALL, one per line, with
              // the row growing to suit (`autoRowHeight` below).
              //
              // 🚨 Two were drawn and the rest counted until an operator reported that the column
              // says nothing at all. The cell was one line and narrower than a single profile name
              // at most window widths, so the second name fell past the ellipsis — and so did the
              // "and N more" standing behind it. A five-profile rule and a one-profile rule rendered
              // identically, and the only way to tell them apart was a hover title, which a touch
              // device does not have.
              <span className="thresholds-scopes" title={scopeTitle(row)}>
                {row.scope_ids.slice(0, MAX_SCOPE_LINES).map((id) =>
                  row.scope_level === 'interface' ? (
                    // The id is `<node-uuid>:<ifindex>`, so the hover title has to be the node's
                    // half — `EntityName` would offer the whole composed string as a copyable
                    // "id", which is not an id anything else accepts.
                    <EntityName
                      key={id}
                      name={scopeName(row.scope_level, id, portNames)}
                      id={splitInterfaceScopeId(id)[0]}
                    />
                  ) : (
                    <EntityName key={id} name={scopeName(row.scope_level, id)} id={id} />
                  ),
                )}
                {row.scope_ids.length > MAX_SCOPE_LINES && (
                  <span className="muted">
                    {t('thresholds.scopeMore', { count: row.scope_ids.length - MAX_SCOPE_LINES })}
                  </span>
                )}
              </span>
            );
          return badge ? (
            <span className="thresholds-overridden">
              {target}
              {badge}
            </span>
          ) : (
            target
          );
        },
      },
      {
        key: 'q',
        header: t('thresholds.cols.metric'),
        width: '1.1fr',
        // The reachability rule carries the engine's internal check name. Every other surface
        // already shows it as "Reachability" (`AlertWhatText`), and this is the one screen where
        // an operator would otherwise meet the raw sentinel.
        render: (row) =>
          row.metric === LIVENESS_METRIC ? (
            <span>{t('format:liveness')}</span>
          ) : (
            // The title carries the whole string because the column is narrow and the pattern is
            // what gets cut off — the one part that tells two rules for the same metric apart.
            <span title={row.row_match ? `${row.metric} · ${row.row_match}` : undefined}>
              <span className="mono">{row.metric}</span>
              {/* The rows it reaches, when it is scoped to some (ADR-143) — without this, a rule for
                  one pool beside the rule for every pool reads as the same rule twice. */}
              {row.row_match ? (
                <span className="muted">
                  {' · '}
                  <span className="mono">{row.row_match}</span>
                </span>
              ) : null}
            </span>
          ),
      },
      {
        // What the metric measures. Every other column here is a property of the *rule*; this is
        // the only one that is a property of the metric, and without it a row like
        // `Reachability | below | (no bounds)` says how the rule behaves and nothing about what
        // it watches. Not filterable — it is derived from the metric, which has its own filter.
        key: 'meaning',
        header: t('thresholds.cols.meaning'),
        width: '1.6fr',
        render: (row) => {
          const key = metricMeaningKey(row.metric);
          // Rows are a fixed 44px, so the text is one line with an ellipsis. `title` carries the
          // tail for a narrow window — it is an overflow fallback, not the only place the
          // explanation lives (ADR-055 R4), which is why the column is not hover-only.
          return key ? (
            <span className="thresholds-meaning" title={t(key)}>
              {t(key)}
            </span>
          ) : (
            <span className="muted" title={t('thresholds.meaningUnknown')}>
              —
            </span>
          );
        },
      },
      {
        key: 'direction',
        header: t('thresholds.cols.direction'),
        width: '92px',
        // ⚠️ A rule bounding both sides reports `above` — the primary side (ADR-081) — so printing
        // that token would tell the operator the rule watches one direction when it watches two.
        // The cell asks the bounds instead.
        render: (row) => (
          <span className="muted">
            {t(
              boundSides(row) === 'both'
                ? 'thresholds.direction.range'
                : `thresholds.direction.${row.direction}`,
            )}
          </span>
        ),
      },
      {
        key: 'bounds',
        header: t('thresholds.cols.bounds'),
        width: '230px',
        render: (row) => <RuleBounds rule={row} />,
      },
      {
        key: 'dwell',
        header: t('thresholds.cols.dwell'),
        width: '100px',
        render: (row) => (
          <span className="muted">{t('thresholds.dwellValue', { n: row.dwell_samples })}</span>
        ),
      },
      {
        key: 'actions',
        header: t('thresholds.cols.actions'),
        width: '124px',
        align: 'right',
        // Drawn only for a caller who may use them (ADR-056) — not `disabled`, which explains
        // itself on hover only and so explains itself to nobody on a touch device.
        render: (row) =>
          canConfig && (
            <span className="ytable-actions">
              <IconButton title={t('common:actions.edit')} onClick={() => setEditing(row)}>
                <EditIcon />
              </IconButton>
              <IconButton
                title={t('common:actions.delete')}
                danger
                onClick={() => setDeleting(row)}
              >
                <TrashIcon />
              </IconButton>
            </span>
          ),
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
  }, [canConfig, overridden, portNames, scopeName, scopeTitle, specs, t]);
  /** Whether any reachability rule exists **anywhere** — `null` until the question is answered.
   *
   *  It cannot be read off `rows`: that list is the operator's current filter, capped by the
   *  server, so an absent rule and a narrowed view look identical. And the tri-state matters —
   *  rendering the warning while the answer is still `undefined` would flash "nothing is watching
   *  your fleet" on every page load.
   *
   *  Asked separately and unfiltered, because the question is about the whole ruleset — and
   *  re-asked with the list, after an add or a delete, the two moments the answer changes. */
  const { data: hasLiveness, reload: livenessReload } = useLoad(
    () => api.listThresholds({ q: LIVENESS_METRIC }).then((p) => p.total > 0),
    [filterCols, filters],
    { initial: null as boolean | null },
  );
  const load = useCallback(() => {
    rulesetReload();
    livenessReload();
  }, [rulesetReload, livenessReload]);

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.rules')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.rules') }]}
      />

      {/* Deleting the reachability rule is allowed — it is an ordinary row — and it switches off
          node-down paging for the whole fleet. Saying so where it happened is the alternative to
          making that one row undeletable, which would make it a different kind of row. The way
          back is the add button, beside the sentence rather than in it (ADR-200). */}
      {hasLiveness === false && (
        <Card className="thresholds-warn-card">
          <p className="thresholds-note">{t('thresholds.noLiveness')}</p>
          {canConfig && (
            <Button type="button" variant="outline" onClick={() => setAdding(true)}>
              {t('thresholds.add')}
            </Button>
          )}
        </Card>
      )}

      {view.check || checking ? (
        <>
          <div className="thresholds-check-head">
            <NodePicker
              value={view.check}
              valueLabel={view.check ? nodeName(view.check) : undefined}
              placeholder={t('thresholds.check.pick')}
              onChange={(n) => {
                setChecking(n == null);
                setView({ ...view, check: n?.id ?? null });
              }}
            />
            <button
              type="button"
              className="linklike"
              onClick={() => {
                setChecking(false);
                setView({ ...view, check: null });
              }}
            >
              {t('thresholds.check.back')}
            </button>
          </div>
          {view.check && <NodeRulesView nodeId={view.check} />}
        </>
      ) : (
      <>
      <Tabs
        tabs={SCOPE_TABS.map((key) => ({
          key,
          label: t(`thresholds.tabs.${key}`),
        }))}
        active={view.tab}
        onChange={(tab) => setView({ tab, target: null, check: null })}
      />
      {targetKind(view.tab) && (
        <div className="thresholds-target">
          <span className="muted">{t(`thresholds.tabs.${view.tab}`)}</span>
          {targetKind(view.tab) === 'profile' ? (
            <Select
              aria-label={t('thresholds.tabs.profile')}
              value={view.target ?? ''}
              onChange={(e) => setView({ ...view, target: e.target.value || null })}
            >
              <option value="">{t('thresholds.target.any')}</option>
              {profiles.map((pr) => (
                <option key={pr.id} value={pr.id}>
                  {pr.name}
                </option>
              ))}
            </Select>
          ) : targetKind(view.tab) === 'folder' ? (
            <GroupPicker
              options={folderItems}
              value={view.target ?? ''}
              emptyOption={t('thresholds.target.any')}
              onChange={(id) => setView({ ...view, target: id || null })}
            />
          ) : (
            <NodePicker
              value={view.target}
              valueLabel={view.target ? nodeName(view.target) : undefined}
              placeholder={t('thresholds.target.any')}
              onChange={(n) => setView({ ...view, target: n?.id ?? null })}
            />
          )}
        </div>
      )}
      <LoadGate load={ruleset} permission="manage_config">
        <ListToolbar
          list={serverToolbarFilters(filterCols, { filters, setFilters })}
          labels={{
            q: t('thresholds.cols.metric'),
            direction: t('thresholds.cols.direction'),
          }}
          count={{
            shown: rows.length,
            total: page.total,
            noun: (n) => t('common:noun.rule', { count: n }),
          }}
          note={
            <>
              {/* Says how many of how many when the server capped the response — never a bare count
                  that would read as the whole ruleset. */}
              {page.truncated && (
                <span className="muted thresholds-truncated">
                  {t('thresholds.truncated', { shown: rows.length, total: page.total })}
                </span>
              )}
            </>
          }
        >
          <Button variant="outline" onClick={() => setChecking(true)}>
            {t('thresholds.check.open')}
          </Button>
          {canConfig && (
            <Button variant="primary" onClick={() => setAdding(true)}>
              {t('thresholds.add')}
            </Button>
          )}
        </ListToolbar>

        {error && <p className="form-error">{error}</p>}

        <DataTable
          tableId="alerts.thresholds"
          rows={rows}
          columns={columns}
          // The Scope cell stacks one line per target, so a row is as tall as its rule is broad
          // (ADR-078 Inc.5). This is the only table that asks for it — see the prop's warning.
          autoRowHeight
          filters={filters}
          onFiltersChange={setFilters}
          rowKey={(r) => r.id}
          loading={loading}
          // Keyed off the filter, never off `rows.length`: with the predicate in SQL, a
          // filtered query that legitimately returns zero is indistinguishable from a
          // ruleset that has no rules at all — and on this table those read very differently.
          empty={
            filtered ? (
              <div className="yt-empty">
                <p className="yt-empty-title">{t('thresholds.emptyFiltered')}</p>
              </div>
            ) : (
              <EmptyState
                text={t('thresholds.empty')}
                action={
                  canConfig && (
                    <Button type="button" variant="primary" onClick={() => setAdding(true)}>
                      {t('thresholds.add')}
                    </Button>
                  )
                }
              />
            )
          }
        />
      </LoadGate>
      </>
      )}

      {adding && (
        <ThresholdModal
          mode="add"
          // The tab and target already chosen open the dialog there, so a rule added while looking
          // at one profile is a rule on that profile unless the operator changes it.
          start={startFormFor(view)}
          onClose={() => setAdding(false)}
          onSaved={load}
        />
      )}
      {editing && (
        // Keyed by the row's id so opening a *different* rule remounts the dialog with that rule's
        // values. Without it React would keep the mounted form state and show the previous rule.
        <ThresholdModal
          key={editing.id}
          mode="edit"
          rule={editing}
          portNames={portNames}
          onClose={() => setEditing(null)}
          onSaved={load}
        />
      )}
      {deleting && (
        <DeleteThresholdModal
          rule={deleting}
          portNames={portNames}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            setError(null);
            load();
          }}
        />
      )}
    </div>
  );
}
