// SPDX-License-Identifier: AGPL-3.0-only
// The fields of a URL / DNS monitor's configuration, as one implementation.
//
// They were the body of a standalone `UrlCheckModal`/`DnsCheckModal` reachable only from the ⋮ menu
// on the Overview health card. "Edit node" now renders them for those kinds, so they live here as
// presentational components: the parent owns the draft, the error and the save, and these own the
// layout and the one dictionary fetch a URL form needs. They are drawn with the shared form parts
// (`Field`, `FieldRow`, `CheckField`, ADR-246) and sit inside the dialog's own `Fields`.
//
// The judgement stays where it was — `checkConfigForm.ts` — because Vitest never runs a `.tsx`.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { Button } from '../ui/Button';
import { Field, FieldError, Select, TextInput } from '../ui/Field';
import { CheckField, FieldGroup, FieldRow } from '../ui/FormLayout';
import { Stack } from '../ui/Page';
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
      <Field label={t('checkEdit.url')} required>
        <TextInput value={d.url} onChange={(e) => set('url', e.target.value)} />
      </Field>
      <Field label={t('checkEdit.method')}>
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
      </Field>
      <Field label={t('checkEdit.expectedStatus')}>
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
      </Field>
      {d.statusMode === 'exact' && (
        <Field label={t('checkEdit.statusCodes')}>
          <TextInput
            value={d.statusCodes}
            placeholder="200, 204"
            onChange={(e) => set('statusCodes', e.target.value)}
          />
        </Field>
      )}
      {d.statusMode === 'range' && (
        <FieldRow>
          <Field label={t('checkEdit.statusLo')}>
            <TextInput
              value={d.statusLo}
              inputMode="numeric"
              placeholder="200"
              onChange={(e) => set('statusLo', e.target.value)}
            />
          </Field>
          <Field label={t('checkEdit.statusHi')}>
            <TextInput
              value={d.statusHi}
              inputMode="numeric"
              placeholder="299"
              onChange={(e) => set('statusHi', e.target.value)}
            />
          </Field>
        </FieldRow>
      )}
      <Field label={t('checkEdit.timeoutMs')}>
        <TextInput
          value={d.timeoutMs}
          inputMode="numeric"
          onChange={(e) => set('timeoutMs', e.target.value)}
        />
      </Field>
      <CheckField
        checked={d.verifyTls}
        onChange={(next) => set('verifyTls', next)}
        label={t('checkEdit.verifyTls')}
      />
      {!d.verifyTls && <FieldError>{t('checkEdit.verifyTlsWarning')}</FieldError>}
      <CheckField
        checked={d.followRedirects}
        onChange={(next) => set('followRedirects', next)}
        label={t('checkEdit.followRedirects')}
      />
      {/* The server refuses a credential without TLS verification (400 credential_needs_tls);
          saying so here avoids a round trip, and says *why* rather than just refusing. */}
      <Field
        label={t('checkEdit.credential')}
        error={d.credentialId !== '' && !d.verifyTls ? t('checkEdit.credentialNeedsTls') : null}
      >
        <Select value={d.credentialId} onChange={(e) => set('credentialId', e.target.value)}>
          <option value="">{t('checkEdit.credentialNone')}</option>
          {creds.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </Select>
      </Field>
      <CheckField
        checked={d.bodyMatchEnabled}
        onChange={(next) => set('bodyMatchEnabled', next)}
        label={t('checkEdit.bodyMatch')}
      />
      {d.bodyMatchEnabled && (
        <>
          <Field label={t('checkEdit.bodyModeLabel')}>
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
          </Field>
          {/* The few words beside the label (ADR-200 kind 5) say how the keyword matches. */}
          <Field
            label={
              <>
                {t('checkEdit.bodyPattern')}
                <span className="nd-field-sub">{t('checkEdit.bodyPatternSub')}</span>
              </>
            }
            required
          >
            <TextInput
              value={d.bodyPattern}
              placeholder={'"status":"ok"'}
              onChange={(e) => set('bodyPattern', e.target.value)}
            />
          </Field>
        </>
      )}
      {/* A group, not a label: each row holds two inputs and a button, and each input carries its
          own `aria-label`. */}
      <FieldGroup label={t('checkEdit.jsonExtract')}>
        <Stack gap={8}>
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
        </Stack>
      </FieldGroup>
      {/* Shown only while something reads the body, so the label alone says what it bounds. A
          keyword past the limit is a failed check, and the Overview card says so when it happens
          (`overview.bodyTruncated`). A HEAD request has no body to read: refused server-side too,
          and saying it here says why before a round trip. */}
      {readsBody && (
        <Field
          label={t('checkEdit.bodyMaxBytes')}
          error={d.method === 'HEAD' ? t('checkEdit.bodyMatchNeedsBody') : null}
        >
          <TextInput
            value={d.bodyMaxBytes}
            inputMode="numeric"
            suffix={t('checkEdit.bytes')}
            onChange={(e) => set('bodyMaxBytes', e.target.value)}
          />
        </Field>
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
      <Field label={t('checkEdit.dnsName')} required>
        <TextInput value={d.name} onChange={(e) => set('name', e.target.value)} />
      </Field>
      <Field label={t('checkEdit.recordType')}>
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
      </Field>
      <FieldRow>
        {/* Blank means the poller's own resolver, so the placeholder says that rather than showing
            an example address that would read as the default. */}
        <Field label={t('checkEdit.resolver')}>
          <TextInput
            value={d.resolver}
            placeholder={t('add.resolverPlaceholder')}
            onChange={(e) => set('resolver', e.target.value)}
          />
        </Field>
        <Field label={t('checkEdit.resolverPort')}>
          <TextInput
            value={d.resolverPort}
            inputMode="numeric"
            onChange={(e) => set('resolverPort', e.target.value)}
          />
        </Field>
      </FieldRow>
      <FieldRow>
        <Field label={t('checkEdit.maxDepth')}>
          <TextInput
            value={d.maxDepth}
            inputMode="numeric"
            onChange={(e) => set('maxDepth', e.target.value)}
          />
        </Field>
        <Field label={t('checkEdit.timeoutMs')}>
          <TextInput
            value={d.timeoutMs}
            inputMode="numeric"
            onChange={(e) => set('timeoutMs', e.target.value)}
          />
        </Field>
      </FieldRow>
    </>
  );
}
