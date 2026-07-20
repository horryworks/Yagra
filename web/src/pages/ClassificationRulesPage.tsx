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

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { ClassificationRule, ClassificationRuleInput, ProfileSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark, FieldHint } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { EntityName } from '../components/ui/EntityName';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { EditIcon, TrashIcon, PowerIcon } from '../components/ui/icons';
import './ClassificationRulesPage.css';

const COLS = '90px 1.7fr 1.2fr 110px 110px';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function ClassificationRulesPage() {
  const { t } = useTranslation('monitoring');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<ClassificationRule[]>([]);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [query, setQuery] = useState('');
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<ClassificationRule | null>(null);
  const [deleting, setDeleting] = useState<ClassificationRule | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .listClassificationRules()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
        else if (e instanceof ApiError && e.status === 401) setUnavailable(false);
      })
      .finally(() => setLoading(false));
    api.listProfiles().then(setProfiles).catch(() => setProfiles([]));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const profileName = (id: string) => profiles.find((p) => p.id === id)?.name ?? id;

  // Filter over the rule's signature (OID prefix / sysDescr regex), maker/model, and the
  // resolved profile name — so an operator can find a rule by what it matches or where it points.
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (q === '') return rows;
    return rows.filter((r) =>
      [
        r.sysobjectid_prefix ?? '',
        r.sysdescr_regex ?? '',
        r.vendor ?? '',
        r.model ?? '',
        profiles.find((p) => p.id === r.profile_id)?.name ?? r.profile_id,
      ]
        .join(' ')
        .toLowerCase()
        .includes(q),
    );
  }, [rows, profiles, query]);

  // Quick enable/disable toggle: re-submit the rule with `enabled` flipped (the update API
  // takes the full body).
  const toggleEnabled = (r: ClassificationRule) => {
    setError(null);
    api
      .updateClassificationRule(r.id, ruleToInput({ ...r, enabled: !r.enabled }))
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, t('rules.err.update'))));
  };

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.classificationRules')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.classificationRules') }]}
        note={t('rules.note')}
      />

      {unavailable ? (
        <Card>
          <p className="muted">{t('rules.unavailable')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder={t('rules.searchPlaceholder')}
              ariaLabel={t('rules.searchAria')}
            />
            <TableSpacer />
            <ResultCount
              shown={filtered.length}
              total={rows.length}
              noun={t('common:noun.rule', { count: rows.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('rules.addRule')}
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <div className="ytable classrules-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">{t('rules.cols.priority')}</div>
              <div className="ytable-h">{t('rules.cols.match')}</div>
              <div className="ytable-h">{t('rules.cols.profile')}</div>
              <div className="ytable-h">{t('rules.cols.status')}</div>
              <div className="ytable-h right">{t('shared.colActions')}</div>
            </div>

            {filtered.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading
                    ? t('common:loading')
                    : rows.length === 0
                      ? t('rules.empty.none')
                      : t('rules.empty.noMatch')}
                </p>
                {!loading && (
                  <p className="yt-empty-sub">
                    {rows.length === 0 ? t('rules.empty.noneSub') : t('shared.trySearch')}
                  </p>
                )}
              </div>
            ) : (
              filtered.map((r) => (
                <div className="ytable-row" key={r.id} style={{ gridTemplateColumns: COLS }}>
                  <div className="ytable-cell mono">{r.priority}</div>
                  <div className="ytable-cell classrules-match">
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
                  </div>
                  <div className="ytable-cell">
                    <EntityName name={profileName(r.profile_id)} id={r.profile_id} />
                  </div>
                  <div className="ytable-cell">
                    <Badge tone={r.enabled ? 'up' : 'neutral'}>
                      {r.enabled ? t('rules.enabled') : t('rules.disabled')}
                    </Badge>
                  </div>
                  <div className="ytable-cell right">
                    {authed && (
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
                    )}
                  </div>
                </div>
              ))
            )}
          </div>
        </>
      )}

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

/** Normalize a rule (or edited copy) into the create/update request body. */
function ruleToInput(r: ClassificationRule): ClassificationRuleInput {
  return {
    priority: r.priority,
    sysobjectid_prefix: r.sysobjectid_prefix,
    sysdescr_regex: r.sysdescr_regex,
    profile_id: r.profile_id,
    vendor: r.vendor,
    model: r.model,
    enabled: r.enabled,
  };
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

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
    setBusy(true);
    setError(null);
    const call =
      mode === 'edit' && rule
        ? api.updateClassificationRule(rule.id, body)
        : api.createClassificationRule(body).then(() => undefined);
    call.then(onDone).catch((e: unknown) => {
      setError(errMsg(e, t('rules.err.save')));
      setBusy(false);
    });
  };

  return (
    <Modal
      title={mode === 'edit' ? t('rules.modal.editTitle') : t('rules.modal.addTitle')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            {mode === 'edit' ? t('common:actions.save') : t('rules.addRule')}
          </Button>
        </>
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
      {error && <p className="form-error">{error}</p>}
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteClassificationRule(rule.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('rules.err.delete')));
        setBusy(false);
      });
  };

  const sig = rule.sysobjectid_prefix ?? rule.sysdescr_regex ?? '';
  return (
    <Modal
      title={t('rules.modal.deleteTitle')}
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
      <p className="modal-confirm-text">
        <Trans
          t={t}
          i18nKey="rules.delete.confirm"
          values={{ sig, profile: profileName }}
          components={{ m: <strong className="mono" />, strong: <strong /> }}
        />
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
