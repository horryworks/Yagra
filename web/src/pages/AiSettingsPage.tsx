// SPDX-License-Identifier: AGPL-3.0-only
// AI analysis (Settings ▸ AI analysis): the single active LLM provider behind "Explain this
// incident" (ADR-029). ManageSystem-gated (ADR-057: it picks which cloud incident data is sent
// to). The credential is write-only — the API returns only
// `has_api_key` — and the provider list, including which vendors send data outside the operator's
// cloud, comes from the backend so the egress warning cannot drift from what the adapters do.
//
// There is deliberately no second provider and no failover: where an incident's details are sent
// is a decision, not something to retry elsewhere.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useAuthStore, useCan } from '../store';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';
import type { LlmConfigView, LlmProviderChoice } from '../types/api';
import { formatExactTime } from '../lib/format';
import {
  DEFAULT_TOKENS,
  MAX_TOKENS,
  MIN_TOKENS,
  hasUsableStoredKey,
  keyIsEditable,
  keyIsRequired,
  toConfigInput,
  validateAiForm,
  type AiFormState,
} from './aiConfigForm';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, TextArea, Select, FieldHint, RequiredMark } from '../components/ui/Field';
import './AiSettingsPage.css';

export function AiSettingsPage() {
  const { t } = useTranslation('settings-ai');
  const authed = useAuthStore((s) => s.authed);
  // Picking the LLM provider decides which cloud incident data is sent to (ADR-057).
  const canSystem = useCan('manage_system');

  const [providers, setProviders] = useState<LlmProviderChoice[]>([]);
  const [stored, setStored] = useState<LlmConfigView | null>(null);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [loading, setLoading] = useState(true);

  const [provider, setProvider] = useState('vertex');
  const [model, setModel] = useState('');
  const [project, setProject] = useState('');
  const [location, setLocation] = useState('');
  const [maxTokens, setMaxTokens] = useState(String(DEFAULT_TOKENS));
  const [enabled, setEnabled] = useState(false);
  const [replaceKey, setReplaceKey] = useState(false);
  const [apiKey, setApiKey] = useState('');

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; text: string } | null>(null);

  const load = useCallback(() => {
    api
      .getLlmConfig()
      .then((res) => {
        setProviders(res.providers);
        setBlock(null);
        setStored(res.config ?? null);
        if (res.config) {
          setProvider(res.config.provider);
          setModel(res.config.model);
          setProject(res.config.project);
          setLocation(res.config.location);
          setMaxTokens(String(res.config.max_output_tokens));
          setEnabled(res.config.enabled);
        } else if (res.providers.length > 0) {
          setProvider(res.providers[0].key);
        }
        setReplaceKey(false);
        setApiKey('');
      })
      .catch((e: unknown) => {
        const b = classifyLoadError(e);
        if (b) setBlock(b);
        else setError(errMsg(e, t('loadFailed')));
      })
      .finally(() => setLoading(false));
  }, [t]);

  useEffect(() => {
    load();
  }, [load]);

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
    maxTokens,
    enabled,
    replaceKey,
    apiKey,
  };
  const keyStored = hasUsableStoredKey(stored, provider);
  const keyEditable = keyIsEditable(stored, form);
  const keyRequired = keyIsRequired(stored, choice, provider);

  const dirty = () => {
    setSaved(false);
    setTestResult(null);
  };

  const save = () => {
    const problem = validateAiForm(stored, choice, form);
    if (problem) {
      setError(t(`err.${problem}`, { min: MIN_TOKENS, max: MAX_TOKENS }));
      setSaved(false);
      return;
    }
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
      <div>
        <PageHeader
          title={t('nav:settings.ai')}
          trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.ai') }]}
          note={t('note')}
        />
        <LoadBlockNotice block={block} unavailable={t('unavailable')} permission="manage_system" />
      </div>
    );
  }

  return (
    <div>
      <PageHeader
        title={t('nav:settings.ai')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.ai') }]}
        note={t('note')}
      />

      <Card title={t('card.provider')}>
        <div className="ai-field">
          <label className="ai-label" htmlFor="ai-provider">
            {t('provider.label')}
          </label>
          <Select
            id="ai-provider"
            value={provider}
            disabled={!canSystem || loading || busy}
            onChange={(e) => {
              setProvider(e.target.value);
              setReplaceKey(false);
              setApiKey('');
              dirty();
            }}
          >
            {providers.map((p) => (
              <option key={p.key} value={p.key}>
                {t(`provider.${p.key}`)}
              </option>
            ))}
          </Select>
          <FieldHint>{t('provider.hint')}</FieldHint>
        </div>

        {choice && (
          <p className={choice.leaves_operator_boundary ? 'ai-egress leaves' : 'ai-egress stays'}>
            {choice.leaves_operator_boundary ? t('egress.leaves') : t('egress.stays')}
          </p>
        )}

        <div className="ai-field">
          <label className="ai-label" htmlFor="ai-model">
            {t('field.model')}
          </label>
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
          <FieldHint>{t('field.modelHint')}</FieldHint>
        </div>

        {choice?.needs_project && (
          <>
            <div className="ai-field">
              <label className="ai-label" htmlFor="ai-project">
                {t('field.project')}
              </label>
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
            </div>
            <div className="ai-field">
              <label className="ai-label" htmlFor="ai-location">
                {t('field.location')}
              </label>
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
              <FieldHint>{t('field.locationHint')}</FieldHint>
            </div>
          </>
        )}

        <div className="ai-field">
          <label className="ai-label" htmlFor="ai-key">
            {choice?.needs_project ? t('field.serviceAccount') : t('field.apiKey')}
            {keyRequired && <RequiredMark />}
          </label>
          {keyStored && (
            <label className="ai-check">
              <input
                type="checkbox"
                checked={replaceKey}
                disabled={!canSystem || busy}
                onChange={(e) => {
                  setReplaceKey(e.target.checked);
                  setApiKey('');
                  dirty();
                }}
              />
              <span>{t('field.replaceKey')}</span>
            </label>
          )}
          {keyEditable &&
            (choice?.needs_project ? (
              <TextArea
                id="ai-key"
                className="mono"
                rows={5}
                value={apiKey}
                autoComplete="off"
                spellCheck={false}
                disabled={!canSystem || loading || busy}
                onChange={(e) => {
                  setApiKey(e.target.value);
                  dirty();
                }}
              />
            ) : (
              <TextInput
                id="ai-key"
                className="mono"
                type="password"
                value={apiKey}
                autoComplete="new-password"
                disabled={!canSystem || loading || busy}
                onChange={(e) => {
                  setApiKey(e.target.value);
                  dirty();
                }}
              />
            ))}
          <FieldHint>
            {keyStored && !replaceKey
              ? t('field.keyStored')
              : choice?.credential_optional
                ? t('field.serviceAccountHint')
                : keyStored
                  ? t('field.clearHint')
                  : t('field.keyMissing')}
          </FieldHint>
        </div>
      </Card>

      <Card title={t('card.limits')}>
        <div className="ai-field">
          <label className="ai-label" htmlFor="ai-tokens">
            {t('field.maxTokens')}
          </label>
          <TextInput
            id="ai-tokens"
            className="ai-tokens mono"
            value={maxTokens}
            inputMode="numeric"
            disabled={!canSystem || loading || busy}
            onChange={(e) => {
              setMaxTokens(e.target.value);
              dirty();
            }}
          />
          <FieldHint>{t('field.maxTokensHint', { min: MIN_TOKENS, max: MAX_TOKENS })}</FieldHint>
        </div>

        <label className="ai-check">
          <input
            type="checkbox"
            checked={enabled}
            disabled={!canSystem || loading || busy}
            onChange={(e) => {
              setEnabled(e.target.checked);
              dirty();
            }}
          />
          <span>{t('field.enabled')}</span>
        </label>
        <FieldHint>{t('field.enabledHint')}</FieldHint>
      </Card>

      <div className="ai-actions">
        {canSystem && (
          <>
            <Button variant="primary" onClick={save} disabled={busy || loading}>
              {t('common:actions.save')}
            </Button>
            <Button
              variant="outline"
              onClick={runTest}
              disabled={stored == null || testing || busy}
              title={t('test.hint')}
            >
              {testing ? t('test.running') : t('test.button')}
            </Button>
          </>
        )}
        <span className="ai-state muted">
          {stored == null
            ? t('state.unconfigured')
            : t('state.updated', { at: formatExactTime(stored.updated_at) })}
        </span>
      </div>

      {!authed && <p className="muted">{t('signInHint')}</p>}
      {error && <p className="form-error">{error}</p>}
      {saved && <p className="ai-saved">{t('state.saved')}</p>}
      {testResult && (
        <p className={testResult.ok ? 'ai-saved' : 'form-error'}>{testResult.text}</p>
      )}
    </div>
  );
}
