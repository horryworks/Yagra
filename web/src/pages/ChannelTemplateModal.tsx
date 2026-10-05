// SPDX-License-Identifier: AGPL-3.0-only
// The notification-template editor (ADR-039), raised from a channel row on Alerts ▸ Notification
// delivery. Since Inc.2 it edits in the shape the notification arrives: variables are tags placed in
// the text, each point in an alert's life has a tab, and the preview beside it re-renders as the
// operator types. The code editor of Inc.1 stays, for templates the tags cannot express and for
// channels whose body must be JSON.
//
// Layout only: the template model is `templateModel.ts`, the field's DOM is `templateDom.ts`, and
// the pieces are `TemplateEditor.tsx`. What is saved is the same template text as before, so the
// server cannot tell which editor wrote it.
//
// Whether this channel's body must be JSON is the server's answer (`json_valid` is present on its
// preview), not a list kept here - see `channelTemplate.ts`.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../services/api';
import type {
  BuiltinSubjectTemplate,
  NotificationChannel,
  NotifyEvent,
  TemplatePreview,
  TemplateVariable,
} from '../types/api';
import { Modal } from '../components/ui/Modal';
import { Button } from '../components/ui/Button';
import { TextInput, TextArea } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { Tabs } from '../components/ui/Tabs';
import type { Point } from '../components/ui/popoverPlacement';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import {
  draftFor,
  insertAtCaret,
  isDirty,
  previewView,
  saveBody,
  subjectSpansLines,
  variableSnippet,
  withFreeLayout,
} from './channelTemplate';
import type { TemplateDraft } from './channelTemplate';
import {
  backToFire,
  builtinDraft,
  builtinTemplate,
  clearField,
  effective,
  fieldFollowsFire,
  fieldHasText,
  followsFire,
  isBlank,
  jsmTitle,
  JSM_MESSAGE_MAX_CHARS,
  openTemplate,
  PREVIEW_SAMPLES,
  previewSample,
  sampleForEvent,
  saveRequest,
  subjectIsBuiltin,
  TEMPLATE_EVENTS,
  visualChanged,
  withField,
  writeOwn,
  type BuiltinDraft,
  type PreviewSampleId,
  type TemplateField as Field,
  type Unsupported,
  type VisualTemplate,
} from './templateModel';
import { presetLanguage, presetTemplate, TEMPLATE_PRESETS } from './templatePresets';
import type { ChipLook } from './templateDom';
import { ChipSettings, TemplateField, VariablePicker, VariableTooltip, type FieldHandle } from './TemplateEditor';
import { bodyAfterTitle, builtinSource, hasOwnTemplate, laidOutBuiltinSource } from './templateDisplay';
import { BuiltinTemplateText, JsonText } from './BuiltinTemplateText';
import { BUILTIN_JSON_KEYS, JSON_SKELETON, TEMPLATE_FORMS } from './templateForm';
import { usePrefsStore } from '../prefs';
import './ChannelTemplateModal.css';

/** What the dialog learns before it can draw: the built-in draft, the variables, and whether the
 *  body must be JSON. */
interface Boot {
  draft: BuiltinDraft;
  variables: TemplateVariable[];
  /** `null` when the first preview failed, so the answer is unknown and the safe editor is used. */
  jsonBody: boolean | null;
  /** The built-in text as templates, per point in the alert's life (ADR-197); `null` when the
   *  server could not say. */
  builtin: BuiltinSubjectTemplate[] | null;
}

type Mode = 'visual' | 'code';

/** Wait this long after the last keystroke before asking for a preview. */
const PREVIEW_DELAY_MS = 400;

export function ChannelTemplateModal({
  channel,
  onClose,
  onDone,
}: {
  channel: NotificationChannel;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t, i18n } = useTranslation('alertsConfig');
  const [boot, setBoot] = useState<Boot | null>(null);
  const [mode, setMode] = useState<Mode>('visual');
  const [model, setModel] = useState<VisualTemplate | null>(null);
  const [code, setCode] = useState<TemplateDraft>(() => draftFor(channel));
  // The code view's caret, as it was last seen in either field. A variable button inserts there;
  // with no caret seen yet it appends to the body, as the list always did.
  const codeCaret = useRef<{ field: 'subject' | 'body'; start: number; end: number } | null>(null);
  const codeSubjectRef = useRef<HTMLInputElement>(null);
  const codeSubjectAreaRef = useRef<HTMLTextAreaElement>(null);
  const codeBodyRef = useRef<HTMLTextAreaElement>(null);
  const [varTip, setVarTip] = useState<{ v: TemplateVariable; el: HTMLElement } | null>(null);
  const [unsupported, setUnsupported] = useState<Unsupported | null>(null);
  const [tab, setTab] = useState<NotifyEvent>('fire');
  const [sampleId, setSampleId] = useState<PreviewSampleId>('nodeDown');
  // Bumped whenever the rows are replaced wholesale, which remounts the fields to draw them.
  const [fieldKey, setFieldKey] = useState(0);
  const [picker, setPicker] = useState<{ field: Field; at: Point | null } | null>(null);
  const [chip, setChip] = useState<{ field: Field; el: HTMLElement; at: Point } | null>(null);
  const [preview, setPreview] = useState<TemplatePreview | null>(null);
  const [previewFailed, setPreviewFailed] = useState(false);
  const [pending, setPending] = useState(false);
  // A channel with no template opens on the built-in text, read-only, until the operator chooses a
  // way to start writing (ADR-197 decision 3).
  const [showBuiltin, setShowBuiltin] = useState(true);
  const [confirmReset, setConfirmReset] = useState(false);
  const handles = { subject: useRef<FieldHandle | null>(null), body: useRef<FieldHandle | null>(null) };
  const insertButtons = { subject: useRef<HTMLElement | null>(null), body: useRef<HTMLElement | null>(null) };
  const form = useSubmit({ errorFallback: t('routing.err.template'), onDone });

  // Opening: the draft, the variables, and one preview of what is stored, which is also how the
  // dialog learns whether this kind's body must be JSON.
  useEffect(() => {
    let live = true;
    Promise.all([
      api.getBuiltinTemplate(channel.kind).catch(() => null),
      api.listTemplateVariables().catch(() => [] as TemplateVariable[]),
      api
        .previewNotificationTemplate({ kind: channel.kind, subject: null, body: null })
        .catch(() => null),
    ]).then(([builtin, variables, first]) => {
      if (!live) return;
      const draft = builtinDraft(builtin);
      const jsonBody = first ? first.json_valid != null : null;
      const opened = openTemplate(
        {
          subject: channel.subject_template ?? null,
          body: channel.body_template ?? null,
          free_layout: channel.template_free_layout,
        },
        draft,
      );
      setBoot({ draft, variables, jsonBody, builtin });
      if (jsonBody === false && opened.ok) {
        setModel(opened.model);
        setMode('visual');
      } else {
        setMode('code');
        if (!opened.ok) setUnsupported(opened.error);
      }
    });
    return () => {
      live = false;
    };
  }, [channel]);

  const request = useMemo<{ subject: string | null; body: string | null; free_layout?: boolean }>(
    () => (mode === 'visual' && model ? saveRequest(model, boot?.draft ?? null) : saveBody(code)),
    [mode, model, code, boot],
  );
  // In the visual view, against the stored template as the editor reads it: one it would only
  // spell differently is not an edit, and must not offer to rewrite the operator's text.
  const dirty =
    mode === 'visual' && model
      ? visualChanged(
          { subject: channel.subject_template ?? null, body: channel.body_template ?? null },
          model,
          boot?.draft ?? null,
        )
      : isDirty(channel, code);
  const sample = previewSample(sampleId);

  // The preview follows the text, a moment after the typing stops. An answer to an older request is
  // dropped: it describes text the operator has already changed.
  const asked = useRef(0);
  useEffect(() => {
    if (!boot) return;
    const seq = ++asked.current;
    setPending(true);
    const timer = window.setTimeout(() => {
      api
        .previewNotificationTemplate({
          kind: channel.kind,
          event: sample.event,
          sample: sample.sample,
          subject: request.subject,
          body: request.body,
          free_layout: request.free_layout,
        })
        .then((r) => {
          if (seq !== asked.current) return;
          setPreview(r);
          setPreviewFailed(false);
        })
        .catch(() => {
          if (seq === asked.current) setPreviewFailed(true);
        })
        .finally(() => {
          if (seq === asked.current) setPending(false);
        });
    }, PREVIEW_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [boot, channel.kind, sample.event, sample.sample, request.subject, request.body, request.free_layout]);

  const optional = useMemo(
    () => new Set((boot?.variables ?? []).filter((v) => !v.always_present).map((v) => v.name)),
    [boot],
  );
  const rememberCaret = (field: 'subject' | 'body', el: HTMLInputElement | HTMLTextAreaElement) => {
    codeCaret.current = { field, start: el.selectionStart ?? el.value.length, end: el.selectionEnd ?? el.value.length };
  };

  const insertCodeVariable = (v: TemplateVariable) => {
    const at = codeCaret.current ?? { field: 'body' as const, start: code.body.length, end: code.body.length };
    const put = insertAtCaret(code[at.field], at.start, at.end, variableSnippet(v.name, v.always_present));
    setCode({ ...code, [at.field]: put.text });
    codeCaret.current = { field: at.field, start: put.caret, end: put.caret };
    // After React has written the new value, or the browser moves the caret to its end.
    requestAnimationFrame(() => {
      const el = at.field === 'subject' ? (codeSubjectRef.current ?? codeSubjectAreaRef.current) : codeBodyRef.current;
      el?.focus({ preventScroll: true });
      el?.setSelectionRange(put.caret, put.caret);
    });
  };

  const look: ChipLook = {
    labelOf: (n) => t(`routing.template.vars.${n}.label`, { defaultValue: n }),
    isOptional: (n) => optional.has(n),
    missingNote: (s) =>
      s.hideLine
        ? t('routing.template.missingNote.hide')
        : s.prefix !== undefined
          ? t('routing.template.missingNote.prefix', { text: s.prefix })
          : s.fallback
          ? t('routing.template.missingNote.text', { text: s.fallback })
          : t('routing.template.missingNote.empty'),
  };

  const closePopovers = () => {
    setPicker(null);
    setChip(null);
  };
  const replaceModel = (next: VisualTemplate) => {
    closePopovers();
    setModel(next);
    setFieldKey((k) => k + 1);
  };
  const chooseTab = (e: NotifyEvent) => {
    closePopovers();
    setTab(e);
    setSampleId((s) => sampleForEvent(e, s));
  };
  const chooseSample = (id: PreviewSampleId) => {
    setSampleId(id);
    if (mode === 'visual') {
      closePopovers();
      setTab(previewSample(id).event);
    }
  };
  const toCode = () => {
    closePopovers();
    setCode({ subject: request.subject ?? '', body: request.body ?? '' });
    setUnsupported(null);
    setMode('code');
  };
  const toVisual = () => {
    const opened = openTemplate(saveBody(code), boot?.draft ?? null);
    if (!opened.ok) {
      setUnsupported(opened.error);
      return;
    }
    setUnsupported(null);
    replaceModel(opened.model);
    setMode('visual');
  };

  const save = () =>
    form.submit(() => api.setNotificationTemplate(channel.id, request).then(() => done()));

  const own = hasOwnTemplate(channel);
  const copy = boot?.builtin ? builtinSource(boot.builtin) : null;
  const laidCopy = boot?.builtin ? laidOutBuiltinSource(boot.builtin) : null;
  const viewingBuiltin = boot !== null && !own && showBuiltin && boot.builtin !== null;
  const builtinAt = (e: NotifyEvent) => boot?.builtin?.find((b) => b.event === e) ?? null;

  // "Edit a copy of this text": the built-in, in the code editor, because the visual one cannot
  // hold its conditional parts (decision 4). Nothing is sent differently until it is saved.
  const editCopy = () => {
    if (!copy) return;
    closePopovers();
    setCode(code.freeLayout && laidCopy ? { ...laidCopy, freeLayout: true } : copy);
    codeCaret.current = null;
    setUnsupported(null);
    setMode('code');
    setShowBuiltin(false);
  };
  // The built-in subject as tags, the body left built-in (ADR-199): only offered where the tags view
  // opens at all and the server's subject reads as tags.
  const asTags =
    boot?.jsonBody === false && boot.draft
      ? () => {
          closePopovers();
          replaceModel(builtinTemplate(boot.draft));
          setMode('visual');
          setShowBuiltin(false);
        }
      : null;
  // Empty, as the button says: since ADR-199 the visual view would otherwise open on the built-in
  // subject, which now reads as tags.
  const startBlank = () => {
    if (mode === 'visual') replaceModel(builtinTemplate(null));
    setShowBuiltin(false);
  };
  // Both fields empty is the built-in text, at every point in the alert's life (decision 5).
  const resetToBuiltin = () =>
    form.submit(() => api.setNotificationTemplate(channel.id, { subject: null, body: null }).then(() => done()));

  // "Start from a JSON skeleton" (Inc.2 decision 11): the built-in JSON is no template, so a JSON
  // channel starts from a skeleton instead of a copy. PagerDuty's summary is still a copy.
  const startFromSkeleton = () => {
    closePopovers();
    setCode({ subject: shape.subject && copy ? copy.subject : '', body: JSON_SKELETON });
    codeCaret.current = null;
    setUnsupported(null);
    setMode('code');
    setShowBuiltin(false);
  };

  // Whether long lines wrap (ADR-198 decision 4): one switch for the code field, the built-in text
  // and the preview, remembered in this browser. Off, a line is drawn whole and scrolls sideways,
  // so where the template really breaks a line is what you see.
  const wrapLines = usePrefsStore((s) => s.templateWrap);
  const setWrapLines = usePrefsStore((s) => s.setTemplateWrap);

  const isJsm = channel.kind === 'jsm';
  const shape = TEMPLATE_FORMS[channel.kind];
  const subjectLabel =
    shape.subject === 'title'
      ? t('routing.template.jsmSubject')
      : shape.subject === 'summary'
        ? t('routing.template.pdSubject')
        : t('routing.template.subject');
  const bodyLabel =
    shape.body === 'json'
      ? t('routing.template.jsonBodyLabel')
      : shape.body === 'customDetails'
        ? t('routing.template.pdBody')
        : t('routing.template.body');
  const hintOf = (field: 'subject' | 'body'): string | null => {
    if (isJsm || channel.kind === 'email') {
      return t(`routing.template.${field}Hint.${isJsm ? 'jsm' : 'email'}`, { max: JSM_MESSAGE_MAX_CHARS });
    }
    if (channel.kind === 'pagerduty') {
      return field === 'subject' ? t('routing.template.pdSubjectHint') : t('routing.template.pdBodyHint');
    }
    return field === 'body' ? t('routing.template.webhookBodyHint') : null;
  };
  // A webhook sends no subject (Inc.2 decision 8): its field is drawn only to show and remove one
  // that was saved before this was known.
  const showCodeSubject = shape.subject !== null || code.subject.trim() !== '';
  const view = preview ? previewView(preview) : null;
  const unusedNow = shape.unusedAt.includes(sample.event);

  return (
    <Modal
      title={t('routing.template.title', { name: channel.name })}
      resizeId="channelTemplate"
      onClose={onClose}
      size="wide"
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('routing.template.save')}
          canSubmit={boot !== null && dirty}
        />
      }
    >
      <p className="tpl-intro">{t('routing.template.intro')}</p>
      {!boot ? (
        <p className="tpl-hint">{t('routing.template.updating')}</p>
      ) : (
        <>
        <label className="form-label form-check tpl-wrap">
          <input type="checkbox" checked={wrapLines} onChange={(e) => setWrapLines(e.target.checked)} />
          <span>{t('routing.template.wrap')}</span>
        </label>
        <div className={wrapLines ? 'tpl-layout' : 'tpl-layout is-nowrap'}>
          <div className="tpl-edit">
            <p className="tpl-sends">
              <span className="tpl-label-sm">{t('routing.template.sends.label')}</span>{' '}
              {t(`routing.template.sends.${channel.kind}`)}
            </p>
            <div
              className={`tpl-status ${!own ? 'is-builtin' : confirmReset ? 'is-confirm' : 'is-own'}`}
              role="status"
            >
              {!own ? (
                <>
                  <strong>{t('routing.template.status.builtinTitle')}</strong>
                  <p>{t('routing.template.status.builtinNote')}</p>
                </>
              ) : confirmReset ? (
                <>
                  <strong>{t('routing.template.status.confirmTitle')}</strong>
                  <p>{t('routing.template.status.confirmNote')}</p>
                  <div className="tpl-status-acts">
                    <Button variant="danger" className="tpl-small" onClick={resetToBuiltin} disabled={form.busy}>
                      {t('routing.template.status.confirm')}
                    </Button>
                    <Button className="tpl-small" onClick={() => setConfirmReset(false)} disabled={form.busy}>
                      {t('routing.template.status.cancel')}
                    </Button>
                  </div>
                </>
              ) : (
                <>
                  <strong>{t('routing.template.status.ownTitle')}</strong>
                  <p>{t('routing.template.status.ownNote')}</p>
                  <div className="tpl-status-acts">
                    <Button variant="danger" className="tpl-small" onClick={() => setConfirmReset(true)}>
                      {t('routing.template.status.reset')}
                    </Button>
                  </div>
                </>
              )}
            </div>

            {viewingBuiltin ? (
              <>
                {boot.jsonBody === false && !shape.json && (
                  <div className="tpl-presets">
                    <span className="tpl-label-sm">{t('routing.template.presets')}</span>
                    {copy && (
                      <Button variant="outline" className="tpl-small" onClick={editCopy}>
                        {t('routing.template.preset.builtin')}
                      </Button>
                    )}
                    {TEMPLATE_PRESETS.map((p) => (
                      <Button
                        key={p}
                        variant="outline"
                        className="tpl-small"
                        onClick={() => {
                          setShowBuiltin(false);
                          replaceModel(presetTemplate(p, presetLanguage(i18n.language)));
                        }}
                      >
                        {t(`routing.template.preset.${p}`)}
                      </Button>
                    ))}
                  </div>
                )}
                <Tabs
                  tabs={TEMPLATE_EVENTS.map((e) => ({ key: e, label: t(`routing.template.tabs.${e}`) }))}
                  active={tab}
                  onChange={chooseTab}
                />
                {shape.unusedAt.includes(tab) ? (
                  <p className="tpl-note">{t(`routing.template.builtinView.closeOnly.${tab}`)}</p>
                ) : (
                  <>
                    {shape.subject !== null && builtinAt(tab) && (
                      <div className="tpl-field-wrap">
                        <div className="tpl-field-head">
                          <span className="tpl-field-name">{subjectLabel}</span>
                          {hintOf('subject') && <span className="tpl-field-hint">{hintOf('subject')}</span>}
                        </div>
                        <BuiltinTemplateText
                          id="tpl-builtin-subject"
                          source={builtinAt(tab)!.subject}
                          label={subjectLabel}
                          multiline={false}
                        />
                      </div>
                    )}
                    <div className="tpl-field-wrap">
                      <div className="tpl-field-head">
                        <span className="tpl-field-name">{bodyLabel}</span>
                        {hintOf('body') && <span className="tpl-field-hint">{hintOf('body')}</span>}
                      </div>
                      {shape.json ? (
                        <>
                          <p className="tpl-note">{t('routing.template.builtinView.jsonSample')}</p>
                          <pre
                            id="tpl-builtin-body"
                            className="tpl-field tpl-builtin is-multi is-mono"
                            data-readonly={t('routing.template.builtinView.readOnly')}
                          >
                            {view && request.body === null ? <JsonText text={view.body} /> : t('routing.template.updating')}
                          </pre>
                          <details className="tpl-keys">
                            <summary>{t('routing.template.builtinView.keysTitle')}</summary>
                            <dl>
                              {BUILTIN_JSON_KEYS.map((k) => (
                                <div key={k}>
                                  <dt>
                                    <code>{k}</code>
                                  </dt>
                                  <dd>{t(`routing.template.jsonKeys.${k}`)}</dd>
                                </div>
                              ))}
                            </dl>
                          </details>
                        </>
                      ) : (
                        builtinAt(tab)?.body != null && (
                          <BuiltinTemplateText
                            id="tpl-builtin-body"
                            source={bodyAfterTitle(builtinAt(tab)!.body!, builtinAt(tab)!.subject)}
                            lead={
                              builtinAt(tab)!.body!.startsWith(builtinAt(tab)!.subject)
                                ? t('routing.template.builtinView.sameAsTitle')
                                : undefined
                            }
                            label={bodyLabel}
                            multiline
                          />
                        )
                      )}
                    </div>
                  </>
                )}
                <div className="tpl-builtin-acts">
                  {shape.json ? (
                    <Button onClick={startFromSkeleton}>{t('routing.template.builtinView.skeleton')}</Button>
                  ) : (
                    copy && <Button onClick={editCopy}>{t('routing.template.builtinView.copy')}</Button>
                  )}
                  {asTags && <Button variant="outline" onClick={asTags}>{t('routing.template.builtinView.asTags', { field: subjectLabel })}</Button>}
                  <Button variant="ghost" onClick={startBlank}>
                    {t('routing.template.builtinView.blank')}
                  </Button>
                </div>
                <p className="tpl-hint">
                  {shape.json ? t('routing.template.builtinView.jsonNote') : t('routing.template.builtinView.condNote', { field: subjectLabel })}
                </p>
              </>
            ) : (
              <>
              {boot.jsonBody === true ? (
                <p className="tpl-note">{t('routing.template.jsonCodeOnly')}</p>
              ) : (
                <div className="tpl-mode" role="group" aria-label={t('routing.template.modeLabel')}>
                  {(['visual', 'code'] as const).map((m) => (
                    <button
                      key={m}
                      type="button"
                      className={mode === m ? 'tpl-mode-btn is-on' : 'tpl-mode-btn'}
                      aria-pressed={mode === m}
                      onClick={m === 'visual' ? toVisual : toCode}
                      disabled={mode === m}
                    >
                      {t(`routing.template.mode.${m}`)}
                    </button>
                  ))}
                </div>
              )}

              {unsupported && (
                <div className="form-warning tpl-unsupported" role="status">
                  <p>{t('routing.template.unsupportedLead')}</p>
                  <p>
                    {t(`routing.template.unsupported.${unsupported.reason}`)} <code>{unsupported.snippet}</code>
                  </p>
                </div>
              )}

              {mode === 'visual' && model ? (
                <>
                  <div className="tpl-presets">
                    <span className="tpl-label-sm">{t('routing.template.presets')}</span>
                    {copy && (
                      <Button variant="outline" className="tpl-small" onClick={editCopy}>
                        {t('routing.template.preset.builtin')}
                      </Button>
                    )}
                    {TEMPLATE_PRESETS.map((p) => (
                      <Button
                        key={p}
                        variant="outline"
                        className="tpl-small"
                        onClick={() => replaceModel(presetTemplate(p, presetLanguage(i18n.language)))}
                      >
                        {t(`routing.template.preset.${p}`)}
                      </Button>
                    ))}
                  </div>

                  <Tabs
                    tabs={TEMPLATE_EVENTS.map((e) => ({
                      key: e,
                      label:
                        !followsFire(model, e)
                          ? t(`routing.template.tabs.${e}`)
                          : t('routing.template.tabSame', { label: t(`routing.template.tabs.${e}`) }),
                    }))}
                    active={tab}
                    onChange={chooseTab}
                  />

                  {tab !== 'fire' && followsFire(model, tab) ? (
                    <div className="tpl-same">
                      <p>{t('routing.template.sameAsFire')}</p>
                      <Button variant="outline" className="tpl-small" onClick={() => replaceModel(writeOwn(model, tab))}>
                        {t('routing.template.writeOwn')}
                      </Button>
                    </div>
                  ) : (
                    <>
                      {(['subject', 'body'] as const).map((field) => (
                        <div className="tpl-field-wrap" key={field}>
                          <div className="tpl-field-head">
                            <label className="tpl-field-name" htmlFor={`tpl-${field}`}>
                              {field === 'subject' ? subjectLabel : bodyLabel}
                            </label>
                            {hintOf(field) && <span className="tpl-field-hint">{hintOf(field)}</span>}
                            <span
                              className="tpl-insert"
                              ref={(el) => {
                                insertButtons[field].current = el;
                              }}
                            >
                              {fieldHasText(model, field) && (
                                <button
                                  type="button"
                                  className="tpl-link"
                                  onClick={() => replaceModel(clearField(model, field))}
                                >
                                  {t('routing.template.resetField')}
                                </button>
                              )}
                              <Button
                                variant="outline"
                                className="tpl-small"
                                aria-haspopup="listbox"
                                onClick={() =>
                                  setPicker((p) => (p && p.field === field && !p.at ? null : { field, at: null }))
                                }
                              >
                                {t('routing.template.insert')}
                              </Button>
                            </span>
                          </div>
                          {tab !== 'fire' && (
                            <p className="tpl-hint">
                              {fieldFollowsFire(model, tab, field) ? (
                                t('routing.template.fieldFollowsFire')
                              ) : (
                                <button
                                  type="button"
                                  className="tpl-link"
                                  onClick={() => replaceModel(backToFire(model, tab, field))}
                                >
                                  {t('routing.template.fieldBackToFire')}
                                </button>
                              )}
                            </p>
                          )}
                          <TemplateField
                            key={`${tab}-${field}-${fieldKey}-${i18n.language}`}
                            id={`tpl-${field}`}
                            segments={effective(model, tab, field)}
                            multiline={field === 'body'}
                            label={field === 'subject' ? subjectLabel : bodyLabel}
                            placeholder={t('routing.template.builtinPlaceholder')}
                            look={look}
                            handleRef={handles[field]}
                            onChange={(segs) => setModel((m) => (m ? withField(m, tab, field, segs) : m))}
                            onOpenPicker={(at) => setPicker({ field, at })}
                            onChipClick={(el) => {
                              const r = el.getBoundingClientRect();
                              setPicker(null);
                              setChip({ field, el, at: { x: r.left, y: r.bottom + 4 } });
                            }}
                          />
                        </div>
                      ))}
                      <p className="tpl-hint">{t('routing.template.insertHint')}</p>
                      {boot.draft !== null && subjectIsBuiltin(model, boot.draft) ? (
                        <p className="tpl-hint">{t('routing.template.builtinDraftNote')}</p>
                      ) : (
                        fieldHasText(model, 'subject') && <p className="tpl-hint">{t('routing.template.poolNote')}</p>
                      )}
                      {isBlank(effective(model, tab, 'body')) && (
                        <p className="tpl-hint">
                          {boot.jsonBody === false
                            ? t('routing.template.builtinBodyEmptyText')
                            : t('routing.template.builtinBodyEmpty')}
                        </p>
                      )}
                      {tab !== 'fire' && (
                        <Button variant="ghost" className="tpl-small" onClick={() => replaceModel(backToFire(model, tab))}>
                          {t('routing.template.backToFire')}
                        </Button>
                      )}
                    </>
                  )}
                </>
              ) : (
                <>
                  {showCodeSubject && (
                    <>
                      {shape.subject === null && (
                        <div className="form-warning tpl-unsent" role="status">
                          <strong>{t('routing.template.webhookSubject.title')}</strong>
                          <p>{t('routing.template.webhookSubject.note')}</p>
                        </div>
                      )}
                      <div className="tpl-code-head">
                        <label className="form-label" htmlFor="tpl-subject">
                          {subjectLabel}
                        </label>
                        {code.subject.trim() !== '' && (
                          <button type="button" className="tpl-link" onClick={() => setCode({ ...code, subject: '' })}>
                            {t('routing.template.resetField')}
                          </button>
                        )}
                      </div>
                      {/* Laid out, the subject spans lines (ADR-199) and is still sent as one line.
                          Not laid out, a line break it holds is sent, so it stays on screen. */}
                      {subjectSpansLines(code) ? (
                        <TextArea
                          id="tpl-subject"
                          className="mono"
                          rows={7}
                          wrap={wrapLines ? 'soft' : 'off'}
                          value={code.subject}
                          spellCheck={false}
                          placeholder={t('routing.template.builtinPlaceholder')}
                          inputRef={codeSubjectAreaRef}
                          onChange={(e) => setCode({ ...code, subject: e.target.value })}
                          onSelect={(e) => rememberCaret('subject', e.currentTarget)}
                        />
                      ) : (
                        <TextInput
                          id="tpl-subject"
                          className="mono"
                          value={code.subject}
                          spellCheck={false}
                          placeholder={t('routing.template.builtinPlaceholder')}
                          inputRef={codeSubjectRef}
                          onChange={(e) => setCode({ ...code, subject: e.target.value })}
                          onSelect={(e) => rememberCaret('subject', e.currentTarget)}
                        />
                      )}
                    </>
                  )}
                  <div className="tpl-code-head">
                    <label className="form-label" htmlFor="tpl-body">
                      {bodyLabel}
                    </label>
                    {code.body.trim() !== '' && (
                      <button type="button" className="tpl-link" onClick={() => setCode({ ...code, body: '' })}>
                        {t('routing.template.resetField')}
                      </button>
                    )}
                  </div>
                  <TextArea
                    id="tpl-body"
                    className="mono"
                    rows={8}
                    wrap={wrapLines ? 'soft' : 'off'}
                    value={code.body}
                    spellCheck={false}
                    placeholder={t('routing.template.builtinPlaceholder')}
                    inputRef={codeBodyRef}
                    onChange={(e) => setCode({ ...code, body: e.target.value })}
                    onSelect={(e) => rememberCaret('body', e.currentTarget)}
                  />
                  <label className="form-label form-check tpl-free-layout">
                    <input
                      type="checkbox"
                      id="tpl-free-layout"
                      checked={code.freeLayout === true}
                      onChange={(e) => setCode(withFreeLayout(code, e.target.checked, copy, laidCopy))}
                    />
                    <span>{t('routing.template.freeLayout.label')}</span>
                  </label>
                  {code.freeLayout && <p className="tpl-hint">{t('routing.template.freeLayout.hint')}</p>}
                  <p className="tpl-hint">
                    {request.subject === null && request.body === null
                      ? t('routing.template.builtinHint')
                      : t('routing.template.blankHint')}
                  </p>
                  <div className="tpl-vars">
                    <h3 className="tpl-vars-title">{t('routing.template.variables')}</h3>
                    <div className="tpl-vars-list">
                      {boot.variables.map((v) => (
                        <button
                          key={v.name}
                          type="button"
                          className="tpl-var"
                          aria-describedby={varTip?.v.name === v.name ? 'tpl-var-tip' : undefined}
                          // Keep focus (and the caret) in the field, so typing carries on after the insert.
                          onMouseDown={(e) => e.preventDefault()}
                          onMouseEnter={(e) => setVarTip({ v, el: e.currentTarget })}
                          onMouseLeave={() => setVarTip(null)}
                          onFocus={(e) => setVarTip({ v, el: e.currentTarget })}
                          onBlur={() => setVarTip(null)}
                          onClick={() => insertCodeVariable(v)}
                        >
                          {v.name}
                          {!v.always_present && <span className="tpl-var-opt">?</span>}
                        </button>
                      ))}
                    </div>
                  </div>
                </>
              )}
              </>
            )}
          </div>

          <div className="tpl-preview">
            <div className="tpl-preview-head">
              <h3 className="tpl-preview-title">{t('routing.template.previewTitle')}</h3>
              {pending && <Badge tone="neutral">{t('routing.template.updating')}</Badge>}
              {view?.jsonValid === false && <Badge tone="critical">{t('routing.template.notJson')}</Badge>}
            </div>
            <span className="tpl-label-sm">{t('routing.template.sampleLabel')}</span>
            <div className="tpl-samples">
              {PREVIEW_SAMPLES.map((s) => (
                <button
                  key={s.id}
                  type="button"
                  className={s.id === sampleId ? 'tpl-sample is-on' : 'tpl-sample'}
                  aria-pressed={s.id === sampleId}
                  onClick={() => chooseSample(s.id)}
                >
                  {t(`routing.template.samples.${s.id}`)}
                </button>
              ))}
            </div>
            {previewFailed && <p className="form-error">{t('routing.err.preview')}</p>}
            {view && unusedNow ? (
              <div className="tpl-card">
                <div className="tpl-card-meta">{t('routing.template.preview.closeOnly')}</div>
                <pre className="tpl-card-body is-json">
                  <JsonText text={'{"event_action":"resolve","dedup_key":"…"}'} />
                </pre>
              </div>
            ) : view && (
              <div className="tpl-card">
                {shape.subject !== null && (
                <div className="tpl-card-title">
                  {isJsm ? <JsmTitle title={view.subject} /> : view.subject}
                  {request.subject === null && <Badge tone="neutral">{t('routing.template.fieldBuiltin')}</Badge>}
                </div>
                )}
                {isJsm && (
                  <div className="tpl-card-meta">
                    {jsmTitle(view.subject).cut
                      ? t('routing.template.titleCut', { n: Array.from(jsmTitle(view.subject).cut).length })
                      : t('routing.template.titleCount', {
                          n: jsmTitle(view.subject).length,
                          max: JSM_MESSAGE_MAX_CHARS,
                        })}
                  </div>
                )}
                {shape.json && (
                  <div className="tpl-card-meta">
                    {channel.kind === 'pagerduty' ? t('routing.template.pdBody') : t('routing.template.preview.post')}
                  </div>
                )}
                <div className={shape.json ? 'tpl-card-body is-json' : 'tpl-card-body'}>
                  {shape.json ? <JsonText text={view.body} /> : view.body}
                  {request.body === null && <Badge tone="neutral">{t('routing.template.fieldBuiltin')}</Badge>}
                </div>
              </div>
            )}
            {view && view.problems.length > 0 && (
              <div className="tpl-problems">
                {/* Not an error state: delivery falls back the same way, so this IS what would be
                    sent. Saying so is the difference between "your template is broken" and "your
                    notification silently changed shape". */}
                <p className="tpl-problems-lead">{t('routing.template.fellBack')}</p>
                <ul>
                  {view.problems.map((p) => (
                    <li key={p}>{p}</li>
                  ))}
                </ul>
              </div>
            )}
            {isJsm && (
              <div className="tpl-fixed">
                <span className="tpl-label-sm">{t('routing.template.fixedTitle')}</span>
                <p>{t('routing.template.fixedJsm')}</p>
              </div>
            )}
            <details className="tpl-saved">
              <summary>{t('routing.template.savedAs')}</summary>
              <div className="tpl-label-sm">subject_template</div>
              <pre>{request.subject ?? t('routing.template.savedEmpty')}</pre>
              <div className="tpl-label-sm">body_template</div>
              <pre>{request.body ?? t('routing.template.savedEmpty')}</pre>
            </details>
          </div>
        </div>
        </>
      )}

      <FormError form={form} />

      {varTip && varTip.el.isConnected && (
        <VariableTooltip
          id="tpl-var-tip"
          anchor={varTip.el}
          name={varTip.v.name}
          snippet={variableSnippet(varTip.v.name, varTip.v.always_present)}
          optional={!varTip.v.always_present}
        />
      )}
      {picker && (
        <VariablePicker
          anchorRef={picker.at ? undefined : insertButtons[picker.field]}
          at={picker.at ?? undefined}
          isOptional={look.isOptional}
          onPick={(name) => {
            const field = picker.field;
            setPicker(null);
            handles[field].current?.insertVariable(name);
          }}
          onClose={() => setPicker(null)}
        />
      )}
      {chip && (
        <ChipSettings
          key={chip.el.dataset.var + String(chip.at.x) + String(chip.at.y)}
          chip={chip.el}
          at={chip.at}
          inSubject={chip.field === 'subject'}
          look={look}
          onChanged={() => handles[chip.field].current?.reread()}
          onClose={() => setChip(null)}
        />
      )}
    </Modal>
  );
}

/** A JSM title with the part JSM cuts off struck through. */
function JsmTitle({ title }: { title: string }) {
  const { kept, cut } = jsmTitle(title);
  return (
    <span className="tpl-jsm-title">
      {kept}
      {cut && (
        <>
          <span className="tpl-cut-mark" aria-hidden="true" />
          <span className="tpl-cut">{cut}</span>
        </>
      )}
    </span>
  );
}
