// SPDX-License-Identifier: AGPL-3.0-only
// The notification-template editor (ADR-039), raised from a channel row on Alerts ▸ Notification
// routing. Subject and body, a variable palette, and a preview against a representative alert.
//
// Layout only: every decision lives in `channelTemplate.ts` where Vitest can reach it (the runner
// is `environment: 'node'` with `include: ['src/**/*.test.ts']`, so a test in this file would
// never run — testing.md).
//
// The preview matters more than it looks: a template is code whose first real execution is during
// an outage. If it cannot be checked while it is being written, it sits broken until the night it
// is needed.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import type { NotificationChannel, TemplateVariable } from '../types/api';
import { Modal } from '../components/ui/Modal';
import { Button } from '../components/ui/Button';
import { TextInput, TextArea } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import {
  draftFor,
  isBuiltin,
  isDirty,
  previewMatches,
  previewView,
  saveBody,
  variableSnippet,
} from './channelTemplate';
import type { PreviewView, TemplateDraft } from './channelTemplate';
import './ChannelTemplateModal.css';

export function ChannelTemplateModal({
  channel,
  onClose,
  onDone,
  onError,
}: {
  channel: NotificationChannel;
  onClose: () => void;
  onDone: () => void;
  onError: (m: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [draft, setDraft] = useState<TemplateDraft>(() => draftFor(channel));
  const [variables, setVariables] = useState<TemplateVariable[]>([]);
  const [preview, setPreview] = useState<PreviewView | null>(null);
  const [previewFor, setPreviewFor] = useState<TemplateDraft | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .listTemplateVariables()
      .then(setVariables)
      .catch(() => setVariables([]));
  }, []);

  // Editing invalidates the preview rather than leaving it on screen: output for text the operator
  // has since changed reads as confirmation of the text they are now looking at.
  const edit = (next: TemplateDraft) => setDraft(next);
  const stale = !previewMatches(previewFor, draft);

  const runPreview = () => {
    const shownFor = draft;
    setBusy(true);
    api
      .previewNotificationTemplate({ kind: channel.kind, ...saveBody(draft) })
      .then((r) => {
        setPreview(previewView(r));
        setPreviewFor(shownFor);
      })
      .catch((e: unknown) => onError(errMsg(e, t('routing.err.preview'))))
      .finally(() => setBusy(false));
  };

  const save = () => {
    setBusy(true);
    api
      .setNotificationTemplate(channel.id, saveBody(draft))
      .then(onDone)
      .catch((e: unknown) => onError(errMsg(e, t('routing.err.template'))))
      .finally(() => setBusy(false));
  };

  return (
    <Modal
      title={t('routing.template.title', { name: channel.name })}
      onClose={onClose}
      size="wide"
      footer={
        <>
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="outline" onClick={runPreview} disabled={busy}>
            {t('routing.template.preview')}
          </Button>
          <Button
            variant="primary"
            onClick={save}
            disabled={busy || !isDirty(channel, draft)}
          >
            {t('routing.template.save')}
          </Button>
        </>
      }
    >
      <p className="tpl-intro">{t('routing.template.intro')}</p>

      <label className="form-label" htmlFor="tpl-subject">
        {t('routing.template.subject')}
      </label>
      <TextInput
        id="tpl-subject"
        className="mono"
        value={draft.subject}
        spellCheck={false}
        placeholder={t('routing.template.builtinPlaceholder')}
        onChange={(e) => edit({ ...draft, subject: e.target.value })}
      />

      <label className="form-label" htmlFor="tpl-body">
        {t('routing.template.body')}
      </label>
      <TextArea
        id="tpl-body"
        className="mono"
        rows={8}
        value={draft.body}
        spellCheck={false}
        placeholder={t('routing.template.builtinPlaceholder')}
        onChange={(e) => edit({ ...draft, body: e.target.value })}
      />
      <p className="tpl-hint">
        {isBuiltin(draft) ? t('routing.template.builtinHint') : t('routing.template.blankHint')}
      </p>

      <div className="tpl-vars">
        <h3 className="tpl-vars-title">{t('routing.template.variables')}</h3>
        <div className="tpl-vars-list">
          {variables.map((v) => (
            <button
              key={v.name}
              type="button"
              className="tpl-var"
              title={v.description}
              onClick={() =>
                edit({
                  ...draft,
                  body: draft.body + variableSnippet(v.name, v.always_present),
                })
              }
            >
              {v.name}
              {!v.always_present && <span className="tpl-var-opt">?</span>}
            </button>
          ))}
        </div>
      </div>

      {preview && (
        <div className={stale ? 'tpl-preview is-stale' : 'tpl-preview'}>
          <h3 className="tpl-preview-title">
            {t('routing.template.previewTitle')}
            {stale && <Badge tone="neutral">{t('routing.template.stale')}</Badge>}
            {preview.jsonValid === false && (
              <Badge tone="critical">{t('routing.template.notJson')}</Badge>
            )}
          </h3>
          <dl className="tpl-preview-out">
            <dt>{t('routing.template.subject')}</dt>
            <dd className="mono">{preview.subject}</dd>
            <dt>{t('routing.template.body')}</dt>
            <dd className="mono tpl-preview-body">{preview.body}</dd>
          </dl>
          {preview.problems.length > 0 && (
            <div className="tpl-problems">
              {/* Not an error state: delivery falls back the same way, so this IS what would be
                  sent. Saying so is the difference between "your template is broken" and
                  "your notification silently changed shape". */}
              <p className="tpl-problems-lead">{t('routing.template.fellBack')}</p>
              <ul>
                {preview.problems.map((p) => (
                  <li key={p}>{p}</li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}
    </Modal>
  );
}
