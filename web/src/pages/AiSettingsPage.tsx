// SPDX-License-Identifier: AGPL-3.0-only
// AI analysis (Settings ▸ AI analysis): the single active LLM provider behind "Explain this
// incident" (ADR-029). ManageSystem-gated (ADR-057: it picks which cloud incident data is sent
// to). The credential is write-only — the API returns only
// `has_api_key` — and the provider list, including which vendors send data outside the operator's
// cloud, comes from the backend so the egress warning cannot drift from what the adapters do.
//
// There is deliberately no second provider and no failover: where an incident's details are sent
// is a decision, not something to retry elsewhere.
//
// The screen explains itself through its controls (ADR-200): the credential is a `SecretInput`,
// the output limit carries its unit and range, Test waits for a save, and sending incident data
// outside the boundary is confirmed at the moment it is switched on. Each of those judgements is
// in `aiConfigForm.ts`, where a test reaches it.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useAuthStore, useCan } from '../store';
import { useLoad } from '../lib/useLoad';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';
import type { LlmConfigResponse, LlmConfigView, LlmProviderChoice } from '../types/api';
import { formatExactTime } from '../lib/format';
import {
  BASE_URL_EXAMPLE,
  DEFAULT_TOKENS,
  MAX_TOKENS,
  MIN_TOKENS,
  canTestAi,
  egressHost,
  hasUsableStoredKey,
  keyIsRequired,
  needsEgressConfirm,
  toConfigInput,
  validateAiForm,
  type AiFormState,
} from './aiConfigForm';
import { PageHeader } from '../components/ui/PageHeader';
import { Page, Inline } from '../components/ui/Page';
import { Card } from '../components/ui/Card';
import { CheckField, Fields, SaveNote, SettingRow } from '../components/ui/FormLayout';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { InfoTip } from '../components/ui/InfoTip';
import { SecretInput } from '../components/ui/SecretInput';
import { Field, TextInput, Select } from '../components/ui/Field';
import './AiSettingsPage.css';

export function AiSettingsPage() {
  const { t } = useTranslation('settings-ai');
  const authed = useAuthStore((s) => s.authed);
  // Picking the LLM provider decides which cloud incident data is sent to (ADR-057).
  const canSystem = useCan('manage_system');

  const [providers, setProviders] = useState<LlmProviderChoice[]>([]);
  const [stored, setStored] = useState<LlmConfigView | null>(null);

  const [provider, setProvider] = useState('vertex');
  const [model, setModel] = useState('');
  const [project, setProject] = useState('');
  const [location, setLocation] = useState('');
  const [baseUrl, setBaseUrl] = useState('');
  const [maxTokens, setMaxTokens] = useState(String(DEFAULT_TOKENS));
  const [enabled, setEnabled] = useState(false);
  const [apiKey, setApiKey] = useState('');
  const [clearKey, setClearKey] = useState(false);

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; text: string } | null>(null);

  const {
    data: answer,
    block,
    error: loadError,
    loading,
    reload: load,
  } = useLoad(() => api.getLlmConfig(), [], {
    initial: null as LlmConfigResponse | null,
    errorFallback: t('loadFailed'),
  });

  // Each answer replaces the form: `data` keeps its identity until a new one arrives.
  useEffect(() => {
    if (!answer) return;
    const res = answer;
    setProviders(res.providers);
    setStored(res.config ?? null);
    if (res.config) {
      setProvider(res.config.provider);
      setModel(res.config.model);
      setProject(res.config.project);
      setLocation(res.config.location);
      setBaseUrl(res.config.base_url);
      setMaxTokens(String(res.config.max_output_tokens));
      setEnabled(res.config.enabled);
    } else if (res.providers.length > 0) {
      setProvider(res.providers[0].key);
    }
    setApiKey('');
    setClearKey(false);
  }, [answer]);

  const choice = useMemo(
    () => providers.find((p) => p.key === provider),
    [providers, provider],
  );

  // A stored credential belongs to the vendor it was entered for. Switching provider therefore
  // means there is no key for the new one — the backend clears it on the same reasoning, so the
  // form must ask rather than let a Gemini key be saved as the Claude credential. The rules
  // themselves live in `aiConfigForm.ts` so they are testable without rendering.
  const form: AiFormState = {
    provider,
    model,
    project,
    location,
    baseUrl,
    maxTokens,
    enabled,
    apiKey,
    clearKey,
  };
  const keyStored = hasUsableStoredKey(stored, provider, baseUrl);
  const confirmHost = egressHost(choice, form);
  const keyRequired = keyIsRequired(stored, choice, provider);

  const dirty = () => {
    setSaved(false);
    setTestResult(null);
  };

  const send = () => {
    setBusy(true);
    setError(null);
    setSaved(false);
    setTestResult(null);
    api
      .saveLlmConfig(toConfigInput(stored, form))
      .then(() => {
        setSaved(true);
        load();
      })
      .catch((e: unknown) => setError(errMsg(e, t('err.save'))))
      .finally(() => setBusy(false));
  };

  const save = () => {
    const problem = validateAiForm(stored, choice, form);
    if (problem) {
      setError(t(`err.${problem}`, { min: MIN_TOKENS, max: MAX_TOKENS }));
      setSaved(false);
      return;
    }
    if (needsEgressConfirm(stored, choice, form)) {
      setError(null);
      setConfirming(true);
      return;
    }
    send();
  };

  const runTest = () => {
    setTesting(true);
    setTestResult(null);
    api
      .testLlmProvider()
      .then((r) =>
        setTestResult({
          ok: r.ok,
          text: r.ok
            ? t('test.ok', { provider: stored?.provider ?? provider, ms: r.latency_ms ?? 0 })
            : t('test.failed', { error: r.error ?? '' }),
        }),
      )
      .catch((e: unknown) => setTestResult({ ok: false, text: errMsg(e, t('err.test')) }))
      .finally(() => setTesting(false));
  };

  if (block) {
    return (
      <Page layout="sections">
        <PageHeader
          title={t('nav:settings.ai')}
          trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.ai') }]}
        />
        <LoadBlockNotice block={block} permission="manage_system" />
      </Page>
    );
  }

  return (
    <Page layout="sections">
      <PageHeader
        title={t('nav:settings.ai')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.ai') }]}
      />

      <Card title={t('card.provider')}>
        <Fields className="ai-fields">
          <Field label={t('provider.label')}>
            <Select
              id="ai-provider"
              value={provider}
              disabled={!canSystem || loading || busy}
              onChange={(e) => {
                setProvider(e.target.value);
                setApiKey('');
                setClearKey(false);
                dirty();
              }}
            >
              {providers.map((p) => (
                <option key={p.key} value={p.key}>
                  {t(`provider.${p.key}`)}
                </option>
              ))}
            </Select>
          </Field>

          <Field label={t('field.model')}>
            <TextInput
              id="ai-model"
              className="mono"
              value={model}
              placeholder={choice?.suggested_model ?? ''}
              disabled={!canSystem || loading || busy}
              onChange={(e) => {
                setModel(e.target.value);
                dirty();
              }}
            />
          </Field>

          {choice?.needs_base_url && (
            <Field label={t('field.baseUrl')}>
              <TextInput
                id="ai-base-url"
                className="mono"
                value={baseUrl}
                placeholder={BASE_URL_EXAMPLE}
                disabled={!canSystem || loading || busy}
                onChange={(e) => {
                  setBaseUrl(e.target.value);
                  dirty();
                }}
              />
            </Field>
          )}

          {choice?.needs_project && (
            <>
              <Field label={t('field.project')}>
                <TextInput
                  id="ai-project"
                  className="mono"
                  value={project}
                  disabled={!canSystem || loading || busy}
                  onChange={(e) => {
                    setProject(e.target.value);
                    dirty();
                  }}
                />
              </Field>
              <Field label={t('field.location')}>
                <TextInput
                  id="ai-location"
                  className="mono"
                  value={location}
                  placeholder={choice.suggested_location ?? ''}
                  disabled={!canSystem || loading || busy}
                  onChange={(e) => {
                    setLocation(e.target.value);
                    dirty();
                  }}
                />
              </Field>
            </>
          )}

          <Field
            label={choice?.needs_project ? t('field.serviceAccount') : t('field.apiKey')}
            htmlFor="ai-key"
            required={keyRequired}
          >
            {!clearKey && (
              <SecretInput
                // A new provider, or a fresh read after a save, starts from the stored mark again.
                key={`${provider}:${keyStored}:${stored?.updated_at ?? ''}`}
                id="ai-key"
                stored={keyStored}
                value={apiKey}
                rows={choice?.needs_project ? 5 : undefined}
                // Only where an empty box means something other than "keep": with nothing stored,
                // Vertex runs on the instance's own identity.
                placeholder={
                  choice?.credential_optional && !keyStored
                    ? t(
                        choice.needs_base_url
                          ? 'field.apiKeyOptionalPlaceholder'
                          : 'field.serviceAccountPlaceholder',
                      )
                    : undefined
                }
                disabled={!canSystem || loading || busy}
                onChange={(v) => {
                  setApiKey(v);
                  dirty();
                }}
              />
            )}
            {keyStored && choice?.credential_optional && (
              <CheckField
                checked={clearKey}
                disabled={!canSystem || busy}
                onChange={(next) => {
                  setClearKey(next);
                  setApiKey('');
                  dirty();
                }}
                label={t(choice.needs_base_url ? 'field.removeKey' : 'field.useWorkloadIdentity')}
              />
            )}
          </Field>
        </Fields>
      </Card>

      <Card title={t('card.limits')}>
        <SettingRow
          label={t('field.maxTokens')}
          info={<InfoTip infoKey="settings-ai:tokens.info" label={t('field.maxTokens')} />}
        >
          <TextInput
            id="ai-tokens"
            className="ai-tokens mono"
            value={maxTokens}
            inputMode="numeric"
            suffix={t('field.tokensUnit')}
            disabled={!canSystem || loading || busy}
            aria-label={t('field.maxTokens')}
            onChange={(e) => {
              setMaxTokens(e.target.value);
              dirty();
            }}
          />
          <span className="muted">{t('field.maxTokensBand', { min: MIN_TOKENS, max: MAX_TOKENS })}</span>
        </SettingRow>

        <CheckField
          checked={enabled}
          disabled={!canSystem || loading || busy}
          onChange={(next) => {
            setEnabled(next);
            dirty();
          }}
          label={t('field.enabled')}
        />
      </Card>

      {/* One Save for both cards above, so it stands under them rather than in either's footer. */}
      <Inline gap={12} justify="end">
        <span className="ai-state muted">
          {stored == null
            ? t('state.unconfigured')
            : t('state.updated', { at: formatExactTime(stored.updated_at) })}
        </span>
        {(error ?? loadError) && <SaveNote tone="error">{error ?? loadError}</SaveNote>}
        {saved && <SaveNote tone="ok">{t('state.saved')}</SaveNote>}
        {testResult && (
          <SaveNote tone={testResult.ok ? 'ok' : 'error'}>{testResult.text}</SaveNote>
        )}
        {canSystem && (
          <>
            <Button variant="primary" onClick={save} disabled={busy || loading}>
              {t('common:actions.save')}
            </Button>
            <Button
              type="button"
              variant="outline"
              onClick={runTest}
              disabled={!canTestAi(stored, form) || testing || busy}
            >
              {testing ? t('test.running') : t('test.button')}
            </Button>
          </>
        )}
      </Inline>

      {!authed && <p className="muted">{t('common:loadBlock.signIn')}</p>}

      {confirming && (
        <ConfirmDeleteModal
          title={
            confirmHost
              ? t('egress.confirmTitleHost', { host: confirmHost })
              : t('egress.confirmTitle')
          }
          confirmLabel={t('egress.confirm')}
          onConfirm={() => api.saveLlmConfig(toConfigInput(stored, form))}
          errorFallback={t('err.save')}
          onClose={() => setConfirming(false)}
          onDone={() => {
            setConfirming(false);
            setSaved(true);
            setTestResult(null);
            load();
          }}
        >
          {confirmHost ? t('egress.leavesHost', { host: confirmHost }) : t('egress.leaves')}
        </ConfirmDeleteModal>
      )}
    </Page>
  );
}
