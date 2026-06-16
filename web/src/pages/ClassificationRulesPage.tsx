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
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { ClassificationRule, ClassificationRuleInput, ProfileSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark, FieldHint } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { EditIcon, TrashIcon, PowerIcon } from '../components/ui/icons';
import './ClassificationRulesPage.css';

const COLS = '90px 1.7fr 1.2fr 110px 110px';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function ClassificationRulesPage() {
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
      .catch((e: unknown) => setError(errMsg(e, 'failed to update rule')));
  };

  return (
    <div>
      <PageHeader
        title="Classification rules"
        trail={[{ label: 'Nodes' }, { label: 'Classification rules' }]}
        note="Map a discovered device's sysObjectID / sysDescr to a Device profile. Discovery pre-selects the match on import."
      />

      {unavailable ? (
        <Card>
          <p className="muted">
            Classification-rule management is unavailable in skeleton mode (no metadata store).
          </p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder="Search signature, profile, maker…"
              ariaLabel="Search classification rules"
            />
            <TableSpacer />
            <ResultCount shown={filtered.length} total={rows.length} noun="rules" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add rule
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <div className="ytable classrules-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">Priority</div>
              <div className="ytable-h">Match</div>
              <div className="ytable-h">Profile</div>
              <div className="ytable-h">Status</div>
              <div className="ytable-h right">Actions</div>
            </div>

            {filtered.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading
                    ? 'Loading…'
                    : rows.length === 0
                      ? 'No classification rules'
                      : 'No rules match'}
                </p>
                {!loading && (
                  <p className="yt-empty-sub">
                    {rows.length === 0
                      ? 'Add a rule mapping a sysObjectID prefix or sysDescr pattern to a profile.'
                      : 'Try a different search.'}
                  </p>
                )}
              </div>
            ) : (
              filtered.map((r) => (
                <div className="ytable-row" key={r.id} style={{ gridTemplateColumns: COLS }}>
                  <div className="ytable-cell mono">{r.priority}</div>
                  <div className="ytable-cell classrules-match">
                    {r.sysobjectid_prefix && (
                      <span className="mono classrules-sig">
                        <span className="classrules-sig-kind">OID</span>
                        {r.sysobjectid_prefix}
                      </span>
                    )}
                    {r.sysdescr_regex && (
                      <span className="mono classrules-sig">
                        <span className="classrules-sig-kind">descr</span>
                        {r.sysdescr_regex}
                      </span>
                    )}
                  </div>
                  <div className="ytable-cell">{profileName(r.profile_id)}</div>
                  <div className="ytable-cell">
                    <Badge tone={r.enabled ? 'up' : 'neutral'}>
                      {r.enabled ? 'enabled' : 'disabled'}
                    </Badge>
                  </div>
                  <div className="ytable-cell right">
                    {authed && (
                      <span className="ytable-actions">
                        <IconButton
                          title={r.enabled ? 'Disable rule' : 'Enable rule'}
                          onClick={() => toggleEnabled(r)}
                        >
                          <PowerIcon />
                        </IconButton>
                        <IconButton title="Edit rule" onClick={() => setEditing(r)}>
                          <EditIcon />
                        </IconButton>
                        <IconButton title="Delete rule" danger onClick={() => setDeleting(r)}>
                          <TrashIcon />
                        </IconButton>
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
      setError(errMsg(e, 'failed to save rule'));
      setBusy(false);
    });
  };

  return (
    <Modal
      title={mode === 'edit' ? 'Edit classification rule' : 'Add classification rule'}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            {mode === 'edit' ? 'Save' : 'Add rule'}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">
          Profile <RequiredMark />
        </label>
        <Select value={profileId} onChange={(e) => setProfileId(e.target.value)}>
          <option value="">(choose a profile)</option>
          {profiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">sysObjectID prefix</label>
        <TextInput
          className="mono"
          placeholder="e.g. 1.3.6.1.4.1.9."
          value={prefix}
          onChange={(e) => setPrefix(e.target.value)}
        />
        <FieldHint>
          Authoritative — outranks sysDescr. End with a dot so 1.3…9. won't also match …91.
          Pair with a sysDescr regex to split one vendor by NOS.
        </FieldHint>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">sysDescr regex</label>
        <TextInput
          className="mono"
          placeholder="e.g. (?i)cisco|ios"
          value={regex}
          onChange={(e) => setRegex(e.target.value)}
        />
        <FieldHint error={!hasMatcher}>
          {hasMatcher
            ? 'With a prefix set, both must match (vendor + NOS); on its own, it matches sysDescr directly.'
            : 'Provide a sysObjectID prefix and/or a sysDescr regex.'}
        </FieldHint>
      </div>
      <div className="modal-field-row">
        <div className="modal-field">
          <label className="modal-field-label">
            Priority <RequiredMark />
          </label>
          <TextInput
            type="number"
            value={priority}
            onChange={(e) => setPriority(e.target.value)}
          />
          <FieldHint>Lower = evaluated first.</FieldHint>
        </div>
        <div className="modal-field">
          <label className="modal-field-label">Maker</label>
          <TextInput
            placeholder="optional"
            value={vendor}
            onChange={(e) => setVendor(e.target.value)}
          />
        </div>
        <div className="modal-field">
          <label className="modal-field-label">Model</label>
          <TextInput
            placeholder="optional"
            value={model}
            onChange={(e) => setModel(e.target.value)}
          />
        </div>
      </div>
      <label className="classrules-enabled">
        <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
        <span>Enabled</span>
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteClassificationRule(rule.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete rule'));
        setBusy(false);
      });
  };

  const sig = rule.sysobjectid_prefix ?? rule.sysdescr_regex ?? '';
  return (
    <Modal
      title="Delete classification rule"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            Delete
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        Delete the rule mapping <strong className="mono">{sig}</strong> →{' '}
        <strong>{profileName}</strong>? Discovery will stop suggesting this profile for matching
        devices (already-imported nodes are unaffected).
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
