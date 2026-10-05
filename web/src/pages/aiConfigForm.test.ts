// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ AI form rules (ADR-029). The cases that matter are the ones where getting it wrong is
// silent: submitting an empty `api_key` where "keep the stored one" was meant would wipe a working
// credential, and omitting it after a vendor switch would leave one provider running on another's
// key until the next incident produced a 502.

import { describe, expect, it } from 'vitest';
import type { LlmConfigView, LlmProviderChoice } from '../types/api';
import {
  aiFormChanged,
  canTestAi,
  DEFAULT_TOKENS,
  formFromStored,
  hasUsableStoredKey,
  keyIsRequired,
  needsEgressConfirm,
  toConfigInput,
  validateAiForm,
  type AiFormState,
} from './aiConfigForm';

const VERTEX: LlmProviderChoice = {
  key: 'vertex',
  suggested_model: 'gemini-2.5-pro',
  suggested_location: 'asia-northeast1',
  leaves_operator_boundary: false,
  needs_project: true,
  credential_optional: true,
};

const CLAUDE: LlmProviderChoice = {
  key: 'claude',
  suggested_model: 'claude-sonnet-4-5',
  suggested_location: null,
  leaves_operator_boundary: true,
  needs_project: false,
  credential_optional: false,
};

function form(over: Partial<AiFormState> = {}): AiFormState {
  return {
    provider: 'claude',
    model: 'a-model',
    project: '',
    location: '',
    maxTokens: String(DEFAULT_TOKENS),
    enabled: true,
    apiKey: '',
    clearKey: false,
    ...over,
  };
}

function storedConfig(over: Partial<LlmConfigView> = {}): LlmConfigView {
  return {
    provider: 'claude',
    model: 'a-model',
    project: '',
    location: '',
    enabled: true,
    max_output_tokens: DEFAULT_TOKENS,
    has_api_key: true,
    leaves_operator_boundary: true,
    updated_at: '2026-07-27T00:00:00Z',
    ...over,
  };
}

describe('stored credential scope', () => {
  it('applies only to the vendor it was entered for', () => {
    const stored = storedConfig({ provider: 'gemini' });
    expect(hasUsableStoredKey(stored, 'gemini')).toBe(true);
    // A Gemini key is not a Claude key. The backend drops it on the switch; the form must agree,
    // or it would show "a credential is stored" over a field the save is about to null out.
    expect(hasUsableStoredKey(stored, 'claude')).toBe(false);
  });

  it('is absent before anything has been configured', () => {
    expect(hasUsableStoredKey(null, 'claude')).toBe(false);
    expect(hasUsableStoredKey(storedConfig({ has_api_key: false }), 'claude')).toBe(false);
  });
});

describe('what the save sends for api_key', () => {
  it('omits the field entirely when a stored credential is being kept', () => {
    // The distinction that matters: omitted keeps, empty CLEARS. Sending '' here would wipe a
    // working key just because the admin edited the model.
    const body = toConfigInput(storedConfig(), form({ model: 'a-newer-model' }));
    expect('api_key' in body).toBe(false);
    expect(body.model).toBe('a-newer-model');
  });

  it('sends the new value once one is typed over a stored credential', () => {
    const body = toConfigInput(storedConfig(), form({ apiKey: 'sk-new' }));
    expect(body.api_key).toBe('sk-new');
  });

  it('keeps the stored credential when the replace box is left empty or blank', () => {
    // The SecretInput rule: an empty box is "keep", never "clear".
    expect('api_key' in toConfigInput(storedConfig(), form({ apiKey: '' }))).toBe(false);
    expect('api_key' in toConfigInput(storedConfig(), form({ apiKey: '   ' }))).toBe(false);
  });

  it('sends an empty value only when asked to clear — how Vertex moves onto Workload Identity', () => {
    const stored = storedConfig({ provider: 'vertex', has_api_key: true });
    const body = toConfigInput(
      stored,
      form({ provider: 'vertex', project: 'p', location: 'global', clearKey: true, apiKey: 'x' }),
    );
    expect(body.api_key).toBe('');
  });

  it('sends a multi-line key file as it was pasted', () => {
    const json = '{\n  "type": "service_account"\n}\n';
    const body = toConfigInput(null, form({ provider: 'vertex', apiKey: json }));
    expect(body.api_key).toBe(json);
  });

  it('sends the field on a vendor switch even with nothing typed', () => {
    // Otherwise the omission would mean "keep", and the previous vendor's key would ride along.
    const body = toConfigInput(
      storedConfig({ provider: 'gemini' }),
      form({ provider: 'claude', apiKey: 'sk-claude' }),
    );
    expect(body.api_key).toBe('sk-claude');
  });

  it('trims identifiers but never the credential', () => {
    const body = toConfigInput(null, form({ model: '  m  ', apiKey: '  sk-x  ' }));
    expect(body.model).toBe('m');
    // Trimming a secret would silently corrupt a key whose format we do not own.
    expect(body.api_key).toBe('  sk-x  ');
  });
});

describe('validation', () => {
  it('accepts a complete key-provider form', () => {
    expect(validateAiForm(null, CLAUDE, form({ apiKey: 'sk-x' }))).toBeNull();
  });

  it('requires a model id', () => {
    expect(validateAiForm(null, CLAUDE, form({ model: '   ', apiKey: 'k' }))).toBe('modelRequired');
  });

  it('requires both GCP identifiers for Vertex', () => {
    const f = form({ provider: 'vertex', project: 'p' });
    expect(validateAiForm(null, VERTEX, f)).toBe('projectRequired');
    expect(validateAiForm(null, VERTEX, { ...f, location: 'global' })).toBeNull();
  });

  it('lets Vertex save with no credential at all', () => {
    // Empty means Workload Identity — the deployment that stores no secret. Demanding a key here
    // would push operators into keeping a service-account JSON they do not need.
    expect(keyIsRequired(null, VERTEX, 'vertex')).toBe(false);
    expect(
      validateAiForm(null, VERTEX, form({ provider: 'vertex', project: 'p', location: 'global' })),
    ).toBeNull();
  });

  it('requires a key for the key-only providers until one is stored', () => {
    expect(keyIsRequired(null, CLAUDE, 'claude')).toBe(true);
    expect(validateAiForm(null, CLAUDE, form())).toBe('keyRequired');
    // Once stored, editing other fields must not demand it again.
    expect(keyIsRequired(storedConfig(), CLAUDE, 'claude')).toBe(false);
    expect(validateAiForm(storedConfig(), CLAUDE, form())).toBeNull();
  });

  it('range-checks the output budget against the backend bounds', () => {
    for (const bad of ['0', '10', '100000', '', 'lots', '4096.5']) {
      expect(validateAiForm(storedConfig(), CLAUDE, form({ maxTokens: bad }))).toBe('tokensRange');
    }
    expect(validateAiForm(storedConfig(), CLAUDE, form({ maxTokens: '256' }))).toBeNull();
    expect(validateAiForm(storedConfig(), CLAUDE, form({ maxTokens: '65536' }))).toBeNull();
  });
});

describe('clearing a stored credential', () => {
  it('is refused for a provider that needs one', () => {
    expect(validateAiForm(storedConfig(), CLAUDE, form({ clearKey: true }))).toBe('keyRequired');
  });

  it('is accepted where the credential is optional', () => {
    const stored = storedConfig({ provider: 'vertex' });
    const f = form({ provider: 'vertex', project: 'p', location: 'global', clearKey: true });
    expect(validateAiForm(stored, VERTEX, f)).toBeNull();
  });
});

describe('Test waits for a save', () => {
  it('is not offered before anything is stored', () => {
    expect(canTestAi(null, form())).toBe(false);
  });

  it('is offered while the form matches what is stored', () => {
    const stored = storedConfig();
    expect(aiFormChanged(stored, formFromStored(stored))).toBe(false);
    expect(canTestAi(stored, formFromStored(stored))).toBe(true);
    // Whitespace the save would trim is not a change.
    expect(canTestAi(stored, { ...formFromStored(stored), model: ' a-model ' })).toBe(true);
  });

  it('is withdrawn by any edit a save would send', () => {
    const stored = storedConfig();
    const base = formFromStored(stored);
    for (const edit of [
      { model: 'another' },
      { maxTokens: '4096' },
      { enabled: false },
      { provider: 'gemini' },
      { apiKey: 'sk-new' },
    ] satisfies Partial<AiFormState>[]) {
      expect(canTestAi(stored, { ...base, ...edit }), JSON.stringify(edit)).toBe(false);
    }
  });

  it('treats a stored configuration with no credential as unchanged when nothing is typed', () => {
    const stored = storedConfig({ provider: 'vertex', has_api_key: false, project: 'p' });
    expect(canTestAi(stored, formFromStored(stored))).toBe(true);
  });
});

describe('confirming that incident data leaves the boundary', () => {
  it('asks when sending is switched on for a vendor outside the boundary', () => {
    expect(needsEgressConfirm(null, CLAUDE, form())).toBe(true);
    expect(needsEgressConfirm(storedConfig({ enabled: false }), CLAUDE, form())).toBe(true);
    // An enabled configuration moved onto such a vendor is a new decision too.
    expect(
      needsEgressConfirm(storedConfig({ provider: 'vertex' }), CLAUDE, form({ provider: 'claude' })),
    ).toBe(true);
  });

  it('does not ask again for a configuration already sending to that vendor', () => {
    expect(needsEgressConfirm(storedConfig(), CLAUDE, form({ model: 'another' }))).toBe(false);
  });

  it('does not ask when nothing will be sent, or nothing leaves', () => {
    expect(needsEgressConfirm(null, CLAUDE, form({ enabled: false }))).toBe(false);
    expect(needsEgressConfirm(null, VERTEX, form({ provider: 'vertex' }))).toBe(false);
    expect(needsEgressConfirm(null, undefined, form())).toBe(false);
  });
});
