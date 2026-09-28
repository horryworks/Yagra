// SPDX-License-Identifier: AGPL-3.0-only
// Classification rules (Nodes ▸ Classification rules). Operator-editable mappings from a
// discovered device's SNMP signature (sysObjectID prefix — authoritative — and/or a sysDescr
// regex) to a Device profile. Discovery consults these to pre-select a profile on import, so a
// new device type is taught here as data, not a code change. CRUD against /classification-rules;
// ManageConfig-gated (503 in skeleton surfaced). Data-table standard v2: toolbar + modal-add.
//
// A rule matches when all of its set matchers match: a sysObjectID prefix AND/or a sysDescr
// regex. So a single rule can mean "this vendor AND this NOS" (e.g. Cisco's 9. prefix + an
// "ASA" keyword). Prefix-bearing rules outrank sysDescr-only ones; within that, lower priority
// wins — so a vendor's NOS-specific rules can precede its prefix-only catch-all.

import { useCallback, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import type { ClassificationRule, ClassificationRuleInput, ProfileSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark, FieldHint } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { EntityName } from '../components/ui/EntityName';
import { ListToolbar } from '../components/ui/ListToolbar';
import { columnLabels } from '../lib/listToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { useClientFilters } from '../lib/useClientFilters';
import { classificationRuleFilters } from './classificationFilters';
import { EditIcon, TrashIcon, PowerIcon } from '../components/ui/icons';
import './ClassificationRulesPage.css';
import { useLoad } from '../lib/useLoad';
import { LoadGate } from '../components/ui/LoadGate';
import { ruleToInput } from './classificationRuleForm';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';

export function ClassificationRulesPage() {
  const { t } = useTranslation('monitoring');
  const canConfig = useCan('manage_config');
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<ClassificationRule | null>(null);
  const [deleting, setDeleting] = useState<ClassificationRule | null>(null);
  const [error, setError] = useState<string | null>(null);

  const rules = useLoad(() => api.listClassificationRules(), [], {
    initial: [] as ClassificationRule[],
  });
  const { data: profiles, reload: profilesReload } = useLoad(() => api.listProfiles(), [], {
    initial: [] as ProfileSummary[],
  });
  const { data: rows, loading, reload: rulesReload } = rules;
  const load = useCallback(() => {
    rulesReload();
    profilesReload();
  }, [rulesReload, profilesReload]);

  const profileName = (id: string) => profiles.find((p) => p.id === id)?.name ?? id;

  // Quick enable/disable toggle: re-submit the rule with `enabled` flipped (the update API
  // takes the full body).
  const toggleEnabled = (r: ClassificationRule) => {
    setError(null);
    api
      .updateClassificationRule(r.id, ruleToInput({ ...r, enabled: !r.enabled }))
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, t('rules.err.update'))));
  };

  const columns = useMemo<Column<ClassificationRule>[]>(() => {
    const specs = classificationRuleFilters(t, profileName);
    const cols: Column<ClassificationRule>[] = [
      {
        key: 'priority',
        header: t('rules.cols.priority'),
        width: '90px',
        render: (r) => <span className="mono">{r.priority}</span>,
      },
      {
        key: 'match',
        header: t('rules.cols.match'),
        width: '1.7fr',
        render: (r) => (
          <span className="classrules-match">
            {r.sysobjectid_prefix && (
              <span className="classrules-sig">
                <span className="classrules-sig-kind">OID</span>
                <span className="classrules-sig-val mono">{r.sysobjectid_prefix}</span>
              </span>
            )}
            {r.sysdescr_regex && (
              <span className="classrules-sig">
                <span className="classrules-sig-kind">descr</span>
                <span className="classrules-sig-val mono">{r.sysdescr_regex}</span>
              </span>
            )}
          </span>
        ),
      },
      {
        key: 'profile',
        header: t('rules.cols.profile'),
        width: '1.2fr',
        render: (r) => <EntityName name={profileName(r.profile_id)} id={r.profile_id} />,
      },
      {
        key: 'status',
        header: t('rules.cols.status'),
        width: '110px',
        render: (r) => (
          <Badge tone={r.enabled ? 'up' : 'neutral'}>
            {r.enabled ? t('rules.enabled') : t('rules.disabled')}
          </Badge>
        ),
      },
      {
        key: 'actions',
        header: t('shared.colActions'),
        width: '110px',
        align: 'right',
        render: (r) =>
          canConfig ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  {
                    label: r.enabled ? t('rules.disable') : t('rules.enable'),
                    icon: <PowerIcon />,
                    onClick: () => toggleEnabled(r),
                  },
                  {
                    label: t('rules.editRule'),
                    icon: <EditIcon />,
                    onClick: () => setEditing(r),
                  },
                  {
                    label: t('rules.deleteRule'),
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
    // `profileName` and `toggleEnabled` are rebuilt every render; what they read is listed instead,
    // so a keystroke in a filter cell does not rebuild the columns and re-run the predicate.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t, canConfig, profiles]);

  // URL-backed: one table on this route, so a narrowed view is linkable.
  const filtering = useClientFilters(columns, rows);
  const { filters, setFilters, shown, counts, anyFiltered } = filtering;

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.classificationRules')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.classificationRules') }]}
        note={t('rules.note')}
      />

      <LoadGate load={rules} permission="manage_config" unavailable={t('rules.unavailable')}>
        <ListToolbar
          list={filtering}
          labels={columnLabels(columns)}
          count={{
            shown: shown.length,
            total: rows.length,
            noun: (n) => t('common:noun.rule', { count: n }),
          }}
        >
          {canConfig && (
            <Button variant="primary" onClick={() => setAdding(true)}>
              + {t('rules.addRule')}
            </Button>
          )}
        </ListToolbar>

        {error && <p className="form-error">{error}</p>}

        <DataTable
          tableId="nodes.classification"
          rows={shown}
          columns={columns}
          rowKey={(r) => r.id}
          filters={filters}
          onFiltersChange={setFilters}
          filterCounts={counts}
          loading={loading}
          empty={anyFiltered ? t('rules.empty.noMatch') : t('rules.empty.none')}
        />
      </LoadGate>

      {adding && (
        <RuleModal
          mode="add"
          profiles={profiles}
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            load();
          }}
        />
      )}
      {editing && (
        <RuleModal
          mode="edit"
          rule={editing}
          profiles={profiles}
          onClose={() => setEditing(null)}
          onDone={() => {
            setEditing(null);
            load();
          }}
        />
      )}
      {deleting && (
        <DeleteRuleModal
          rule={deleting}
          profileName={profileName(deleting.profile_id)}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        />
      )}
    </div>
  );
}

/** Add or edit a classification rule (focused-editing modal). */
function RuleModal({
  mode,
  rule,
  profiles,
  onClose,
  onDone,
}: {
  mode: 'add' | 'edit';
  rule?: ClassificationRule;
  profiles: ProfileSummary[];
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('monitoring');
  const [priority, setPriority] = useState(String(rule?.priority ?? 100));
  const [prefix, setPrefix] = useState(rule?.sysobjectid_prefix ?? '');
  const [regex, setRegex] = useState(rule?.sysdescr_regex ?? '');
  const [profileId, setProfileId] = useState(rule?.profile_id ?? '');
  const [vendor, setVendor] = useState(rule?.vendor ?? '');
  const [model, setModel] = useState(rule?.model ?? '');
  const [enabled, setEnabled] = useState(rule?.enabled ?? true);
  const form = useSubmit({ errorFallback: t('rules.err.save'), onDone });

  const hasMatcher = prefix.trim() !== '' || regex.trim() !== '';
  const valid = hasMatcher && profileId !== '' && priority.trim() !== '';

  const submit = () => {
    if (!valid) return;
    const body: ClassificationRuleInput = {
      priority: Number(priority),
      sysobjectid_prefix: prefix.trim() || null,
      sysdescr_regex: regex.trim() || null,
      profile_id: profileId,
      vendor: vendor.trim() || null,
      model: model.trim() || null,
      enabled,
    };
    form.submit(() => {
      const call =
        mode === 'edit' && rule
          ? api.updateClassificationRule(rule.id, body)
          : api.createClassificationRule(body);
      return call.then(() => done());
    });
  };

  return (
    <Modal
      title={mode === 'edit' ? t('rules.modal.editTitle') : t('rules.modal.addTitle')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={mode === 'edit' ? t('common:actions.save') : t('rules.addRule')}
          canSubmit={valid}
        />
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">
          {t('rules.cols.profile')} <RequiredMark />
        </label>
        <Select value={profileId} onChange={(e) => setProfileId(e.target.value)}>
          <option value="">{t('rules.modal.chooseProfile')}</option>
          {profiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('rules.modal.oidPrefix')}</label>
        <TextInput
          className="mono"
          placeholder={t('rules.modal.oidPrefixPlaceholder')}
          value={prefix}
          onChange={(e) => setPrefix(e.target.value)}
        />
        <FieldHint>{t('rules.modal.oidPrefixHint')}</FieldHint>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('rules.modal.descrRegex')}</label>
        <TextInput
          className="mono"
          placeholder={t('rules.modal.descrRegexPlaceholder')}
          value={regex}
          onChange={(e) => setRegex(e.target.value)}
        />
        <FieldHint error={!hasMatcher}>
          {hasMatcher ? t('rules.modal.descrRegexHintBoth') : t('rules.modal.matcherRequired')}
        </FieldHint>
      </div>
      <div className="modal-field-row">
        <div className="modal-field">
          <label className="modal-field-label">
            {t('rules.cols.priority')} <RequiredMark />
          </label>
          <TextInput
            type="number"
            value={priority}
            onChange={(e) => setPriority(e.target.value)}
          />
          <FieldHint>{t('rules.modal.priorityHint')}</FieldHint>
        </div>
        <div className="modal-field">
          <label className="modal-field-label">{t('rules.modal.maker')}</label>
          <TextInput
            placeholder={t('shared.optional')}
            value={vendor}
            onChange={(e) => setVendor(e.target.value)}
          />
        </div>
        <div className="modal-field">
          <label className="modal-field-label">{t('rules.modal.model')}</label>
          <TextInput
            placeholder={t('shared.optional')}
            value={model}
            onChange={(e) => setModel(e.target.value)}
          />
        </div>
      </div>
      <label className="classrules-enabled">
        <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
        <span>{t('rules.modal.enabledLabel')}</span>
      </label>
      <FormError form={form} />
    </Modal>
  );
}

/** Confirm + delete a rule (destructive-consent modal). */
function DeleteRuleModal({
  rule,
  profileName,
  onClose,
  onDone,
}: {
  rule: ClassificationRule;
  profileName: string;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('monitoring');
  const sig = rule.sysobjectid_prefix ?? rule.sysdescr_regex ?? '';
  return (
    <ConfirmDeleteModal
      title={t('rules.modal.deleteTitle')}
      onConfirm={() => api.deleteClassificationRule(rule.id)}
      errorFallback={t('rules.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="rules.delete.confirm"
        values={{ sig, profile: profileName }}
        components={{ m: <strong className="mono" />, strong: <strong /> }}
      />
    </ConfirmDeleteModal>
  );
}
