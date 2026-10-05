// SPDX-License-Identifier: AGPL-3.0-only
// The fields of a URL / DNS monitor's configuration, as one implementation.
//
// They were the body of a standalone `UrlCheckModal`/`DnsCheckModal` reachable only from the ⋮ menu
// on the Overview health card. "Edit node" now renders them for those kinds, so they live here as
// presentational components: the parent owns the draft, the error and the save, and these own the
// layout and the one dictionary fetch a URL form needs.
//
// The judgement stays where it was — `checkConfigForm.ts` — because Vitest never runs a `.tsx`.

import { useEffect, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { Button } from '../ui/Button';
import { FieldError, RequiredMark, Select, TextInput } from '../ui/Field';
import { isHttpCredentialKind } from '../../lib/credentialKinds';
import type { CredentialSummary } from '../../types/api';
import {
  withExtract,
  withExtractAdded,
  withExtractRemoved,
  BODY_MATCH_MODES,
  EXPECTED_STATUS_MODES,
  MAX_EXTRACT_ROWS,
  type DnsCheckDraft,
  type UrlCheckDraft,
} from './checkConfigForm';

const HTTP_METHODS = ['GET', 'HEAD', 'POST'] as const;
const DNS_RECORD_TYPES = ['A', 'AAAA', 'CNAME'] as const;

/** One labelled row. Exported because both this file's forms and the dialog that hosts them spell
 *  a field the same way; the shared `Field` exports the controls, not the label+hint wrapper.
 *  `sub` is a few words beside the label (ADR-200 kind 5), never a sentence; `hint` is for a state
 *  the field is in, not for explaining it. */
export function Row({
  label,
  sub,
  required,
  hint,
  children,
}: {
  label: string;
  sub?: string;
  required?: boolean;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <label className="modal-field">
      <span className="modal-field-label">
        {label} {required && <RequiredMark />}
        {sub && <span className="nd-field-sub">{sub}</span>}
      </span>
      {children}
      {hint && <span className="form-status">{hint}</span>}
    </label>
  );
}

/** What a URL monitor probes and what counts as healthy. */
export function UrlCheckFields({
  draft: d,
  onChange,
}: {
  draft: UrlCheckDraft;
  onChange: (next: UrlCheckDraft) => void;
}) {
  const { t } = useTranslation('nodes');
  // Credentials a URL monitor can present. Filtered to the kinds the probe understands, so the
  // picker cannot offer an SNMP community that would be rejected at poll time.
  const [creds, setCreds] = useState<CredentialSummary[]>([]);
  useEffect(() => {
    api
      .listCredentials()
      .then((list) => setCreds(list.filter((c) => isHttpCredentialKind(c.kind))))
      .catch(() => setCreds([]));
  }, []);

  const set = <K extends keyof UrlCheckDraft>(k: K, v: UrlCheckDraft[K]) =>
    onChange({ ...d, [k]: v });

  // The read budget only means anything when something reads the body, so it is shown only then —
  // one budget shared by both features, which is why this is not asked twice.
  const readsBody = d.bodyMatchEnabled || d.extracts.length > 0;

  return (
    <>
      <Row label={t('checkEdit.url')} required>
        <TextInput value={d.url} onChange={(e) => set('url', e.target.value)} />
      </Row>
      <Row label={t('checkEdit.method')}>
        <Select
          value={d.method}
          onChange={(e) => set('method', e.target.value as UrlCheckDraft['method'])}
        >
          {HTTP_METHODS.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </Select>
      </Row>
      <Row label={t('checkEdit.expectedStatus')}>
        <Select
          value={d.statusMode}
          onChange={(e) => set('statusMode', e.target.value as UrlCheckDraft['statusMode'])}
        >
          {EXPECTED_STATUS_MODES.map((m) => (
            <option key={m} value={m}>
              {t(`checkEdit.statusMode.${m}`)}
            </option>
          ))}
        </Select>
      </Row>
      {d.statusMode === 'exact' && (
        <Row label={t('checkEdit.statusCodes')}>
          <TextInput
            value={d.statusCodes}
            placeholder="200, 204"
            onChange={(e) => set('statusCodes', e.target.value)}
          />
        </Row>
      )}
      {d.statusMode === 'range' && (
        <div className="modal-field-row">
          <Row label={t('checkEdit.statusLo')}>
            <TextInput
              value={d.statusLo}
              inputMode="numeric"
              placeholder="200"
              onChange={(e) => set('statusLo', e.target.value)}
            />
          </Row>
          <Row label={t('checkEdit.statusHi')}>
            <TextInput
              value={d.statusHi}
              inputMode="numeric"
              placeholder="299"
              onChange={(e) => set('statusHi', e.target.value)}
            />
          </Row>
        </div>
      )}
      <Row label={t('checkEdit.timeoutMs')}>
        <TextInput
          value={d.timeoutMs}
          inputMode="numeric"
          onChange={(e) => set('timeoutMs', e.target.value)}
        />
      </Row>
      <label className="nd-check-toggle">
        <input
          type="checkbox"
          checked={d.verifyTls}
          onChange={(e) => set('verifyTls', e.target.checked)}
        />
        <span>{t('checkEdit.verifyTls')}</span>
      </label>
      {!d.verifyTls && <FieldError>{t('checkEdit.verifyTlsWarning')}</FieldError>}
      <label className="nd-check-toggle">
        <input
          type="checkbox"
          checked={d.followRedirects}
          onChange={(e) => set('followRedirects', e.target.checked)}
        />
        <span>{t('checkEdit.followRedirects')}</span>
      </label>
      <Row label={t('checkEdit.credential')}>
        <Select value={d.credentialId} onChange={(e) => set('credentialId', e.target.value)}>
          <option value="">{t('checkEdit.credentialNone')}</option>
          {creds.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </Select>
      </Row>
      {/* The server refuses this pair (400 credential_needs_tls); saying so here avoids a round
          trip, and says *why* rather than just refusing. */}
      {d.credentialId !== '' && !d.verifyTls && (
        <FieldError>{t('checkEdit.credentialNeedsTls')}</FieldError>
      )}
      <label className="nd-check-toggle">
        <input
          type="checkbox"
          checked={d.bodyMatchEnabled}
          onChange={(e) => set('bodyMatchEnabled', e.target.checked)}
        />
        <span>{t('checkEdit.bodyMatch')}</span>
      </label>
      {d.bodyMatchEnabled && (
        <>
          <Row label={t('checkEdit.bodyModeLabel')}>
            <Select
              value={d.bodyMode}
              onChange={(e) => set('bodyMode', e.target.value as UrlCheckDraft['bodyMode'])}
            >
              {BODY_MATCH_MODES.map((m) => (
                <option key={m} value={m}>
                  {t(`checkEdit.bodyMode.${m}`)}
                </option>
              ))}
            </Select>
          </Row>
          <Row label={t('checkEdit.bodyPattern')} sub={t('checkEdit.bodyPatternSub')} required>
            <TextInput
              value={d.bodyPattern}
              placeholder={'"status":"ok"'}
              onChange={(e) => set('bodyPattern', e.target.value)}
            />
          </Row>
        </>
      )}
      <Row label={t('checkEdit.jsonExtract')}>
        <div className="nd-extract-rows">
          {d.extracts.map((row, i) => (
            <div className="nd-extract-row" key={i}>
              <TextInput
                value={row.metric}
                placeholder={t('checkEdit.extractMetricPlaceholder')}
                aria-label={t('checkEdit.extractMetric')}
                onChange={(e) => onChange(withExtract(d, i, { metric: e.target.value }))}
              />
              <TextInput
                value={row.path}
                placeholder="data.queue.depth"
                aria-label={t('checkEdit.extractPath')}
                onChange={(e) => onChange(withExtract(d, i, { path: e.target.value }))}
              />
              <Button onClick={() => onChange(withExtractRemoved(d, i))}>
                {t('common:actions.remove')}
              </Button>
            </div>
          ))}
          {/* Hidden at the cap the validator enforces — one exported constant, so the form cannot
              offer a row the save would then refuse. */}
          {d.extracts.length < MAX_EXTRACT_ROWS && (
            <Button onClick={() => onChange(withExtractAdded(d))}>
              {t('checkEdit.extractAdd')}
            </Button>
          )}
        </div>
      </Row>
      {readsBody && (
        <>
          {/* Shown only while something reads the body, so the label alone says what it bounds.
              A keyword past the limit is a failed check, and the Overview card says so when it
              happens (`overview.bodyTruncated`). */}
          <Row label={t('checkEdit.bodyMaxBytes')}>
            <TextInput
              value={d.bodyMaxBytes}
              inputMode="numeric"
              suffix={t('checkEdit.bytes')}
              onChange={(e) => set('bodyMaxBytes', e.target.value)}
            />
          </Row>
          {/* Refused server-side too; saying it here says why before a round trip. */}
          {d.method === 'HEAD' && <FieldError>{t('checkEdit.bodyMatchNeedsBody')}</FieldError>}
        </>
      )}
    </>
  );
}

/** What a DNS monitor resolves, and against which resolver. */
export function DnsCheckFields({
  draft: d,
  onChange,
}: {
  draft: DnsCheckDraft;
  onChange: (next: DnsCheckDraft) => void;
}) {
  const { t } = useTranslation('nodes');
  const set = <K extends keyof DnsCheckDraft>(k: K, v: DnsCheckDraft[K]) =>
    onChange({ ...d, [k]: v });

  return (
    <>
      <Row label={t('checkEdit.dnsName')} required>
        <TextInput value={d.name} onChange={(e) => set('name', e.target.value)} />
      </Row>
      <Row label={t('checkEdit.recordType')}>
        <Select
          value={d.recordType}
          onChange={(e) => set('recordType', e.target.value as DnsCheckDraft['recordType'])}
        >
          {DNS_RECORD_TYPES.map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </Select>
      </Row>
      <div className="modal-field-row">
        {/* Blank means the poller's own resolver, so the placeholder says that rather than showing
            an example address that would read as the default. */}
        <Row label={t('checkEdit.resolver')}>
          <TextInput
            value={d.resolver}
            placeholder={t('add.resolverPlaceholder')}
            onChange={(e) => set('resolver', e.target.value)}
          />
        </Row>
        <Row label={t('checkEdit.resolverPort')}>
          <TextInput
            value={d.resolverPort}
            inputMode="numeric"
            onChange={(e) => set('resolverPort', e.target.value)}
          />
        </Row>
      </div>
      <div className="modal-field-row">
        <Row label={t('checkEdit.maxDepth')}>
          <TextInput
            value={d.maxDepth}
            inputMode="numeric"
            onChange={(e) => set('maxDepth', e.target.value)}
          />
        </Row>
        <Row label={t('checkEdit.timeoutMs')}>
          <TextInput
            value={d.timeoutMs}
            inputMode="numeric"
            onChange={(e) => set('timeoutMs', e.target.value)}
          />
        </Row>
      </div>
    </>
  );
}
