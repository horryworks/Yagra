// SPDX-License-Identifier: AGPL-3.0-only
// Report builder — create/edit a report definition (template). Reuses the Modal + Field controls.
// Left: report name/description/window + the ordered list of chosen sections with their settings.
// "Add section" reveals the section catalog (grouped), mirroring the dashboard widget picker. Local
// form state (save-on-submit), not an autosave store — a template is committed explicitly.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { Button } from '../components/ui/Button';
import { TextInput, Select, RequiredMark } from '../components/ui/Field';
import { api } from '../services/api';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import type {
  ReportDefinition,
  ReportSectionDef,
  ReportSectionInstance,
  ReportSpec,
} from '../types/api';
import {
  RANGE_OPTIONS,
  DEFAULT_RANGE_SECS,
  newSection,
  sanitizeSpec,
} from './types';

interface Props {
  catalog: ReportSectionDef[];
  /** The definition to edit, or null to create a new one. */
  definition: ReportDefinition | null;
  onClose: () => void;
  onSaved: () => void;
}

export function ReportBuilder({ catalog, definition, onClose, onSaved }: Props) {
  const { t } = useTranslation('reports');
  // `spec` is opaque JSON to the backend (it stores and returns it untouched), so the document
  // types it `unknown`; this is the WebUI's reading of its own document, and `sanitizeSpec` is
  // what makes an older or partial one usable.
  const initial = useMemo(
    () => (definition ? sanitizeSpec(definition.spec as ReportSpec | null) : null),
    [definition],
  );
  const [name, setName] = useState(definition?.name ?? '');
  const [description, setDescription] = useState(definition?.description ?? '');
  const [rangeSecs, setRangeSecs] = useState<number>(
    initial?.params.range_secs ?? DEFAULT_RANGE_SECS,
  );
  const [sections, setSections] = useState<ReportSectionInstance[]>(initial?.sections ?? []);
  const [showCatalog, setShowCatalog] = useState(sections.length === 0);
  const form = useSubmit({ errorFallback: t('builder.err.saveFailed'), onDone: onSaved });

  const defByKind = useMemo(() => {
    const m = new Map<string, ReportSectionDef>();
    for (const d of catalog) m.set(d.kind, d);
    return m;
  }, [catalog]);

  const groups = useMemo(() => {
    const by = new Map<string, ReportSectionDef[]>();
    for (const d of catalog) {
      const arr = by.get(d.group) ?? [];
      arr.push(d);
      by.set(d.group, arr);
    }
    return [...by.entries()];
  }, [catalog]);

  function addSection(def: ReportSectionDef) {
    setSections((s) => [...s, newSection(def)]);
  }
  function removeSection(i: number) {
    setSections((s) => s.filter((_, idx) => idx !== i));
  }
  function moveSection(i: number, dir: -1 | 1) {
    setSections((s) => {
      const j = i + dir;
      if (j < 0 || j >= s.length) return s;
      const next = [...s];
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });
  }
  function setSetting(i: number, key: string, value: unknown) {
    setSections((s) =>
      s.map((sec, idx) =>
        idx === i ? { ...sec, settings: { ...sec.settings, [key]: value } } : sec,
      ),
    );
  }

  function save() {
    const trimmed = name.trim();
    if (!trimmed) {
      form.refuse(t('builder.err.nameRequired'));
      return;
    }
    if (sections.length === 0) {
      form.refuse(t('builder.err.noSections'));
      return;
    }
    const body = {
      name: trimmed,
      description: description.trim() || undefined,
      spec: { version: 1, params: { range_secs: rangeSecs }, sections },
    };
    form.submit(async () => {
      if (definition) await api.updateReportDefinition(definition.id, body);
      else await api.createReportDefinition(body);
      return done();
    });
  }

  return (
    <Modal
      title={definition ? t('builder.editTitle') : t('builder.newTitle')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('builder.save')}
          busyLabel={t('builder.saving')}
        />
      }
    >
      <div className="rb">
        <label className="rb-field">
          <span>
            {t('builder.name')} <RequiredMark />
          </span>
          <TextInput
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={t('builder.namePlaceholder')}
            autoFocus
          />
        </label>

        <label className="rb-field">
          <span>{t('builder.description')}</span>
          <TextInput
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder={t('builder.descriptionPlaceholder')}
          />
        </label>

        <label className="rb-field">
          <span>{t('builder.timeWindow')}</span>
          <Select
            value={String(rangeSecs)}
            onChange={(e) => setRangeSecs(Number(e.target.value))}
          >
            {RANGE_OPTIONS.map((o) => (
              <option key={o.secs} value={o.secs}>
                {t(o.labelKey)}
              </option>
            ))}
          </Select>
        </label>

        <div className="rb-sections-head">
          <span>{t('builder.sectionsCount', { count: sections.length })}</span>
          <Button variant="ghost" onClick={() => setShowCatalog((v) => !v)}>
            {showCatalog ? t('builder.hideCatalog') : t('builder.addSection')}
          </Button>
        </div>

        {showCatalog && (
          <div className="rb-catalog">
            {groups.map(([group, defs]) => (
              <div key={group} className="rb-cat-group">
                <div className="rb-cat-group-title">{group}</div>
                {defs.map((d) => (
                  <div key={d.kind} className="rb-cat-item">
                    <div>
                      <div className="rb-cat-item-title">{d.title}</div>
                      <div className="rb-cat-item-blurb">{d.blurb}</div>
                    </div>
                    <Button variant="ghost" onClick={() => addSection(d)}>
                      {t('common:actions.add')}
                    </Button>
                  </div>
                ))}
              </div>
            ))}
          </div>
        )}

        <div className="rb-sections">
          {sections.length === 0 && (
            <div className="rb-empty">{t('builder.noSections')}</div>
          )}
          {sections.map((sec, i) => {
            const def = defByKind.get(sec.kind);
            return (
              <div key={sec.id} className="rb-section">
                <div className="rb-section-top">
                  <span className="rb-section-title">{def?.title ?? sec.kind}</span>
                  <div className="rp-actions">
                    <button
                      className="rb-icon"
                      aria-label={t('builder.moveUp')}
                      disabled={i === 0}
                      onClick={() => moveSection(i, -1)}
                    >
                      ↑
                    </button>
                    <button
                      className="rb-icon"
                      aria-label={t('builder.moveDown')}
                      disabled={i === sections.length - 1}
                      onClick={() => moveSection(i, 1)}
                    >
                      ↓
                    </button>
                    <button
                      className="rb-icon"
                      aria-label={t('builder.removeSection')}
                      onClick={() => removeSection(i)}
                    >
                      ×
                    </button>
                  </div>
                </div>
                {def && def.settings.length > 0 && (
                  <div className="rb-section-settings">
                    {def.settings.map((setting) => {
                      const value = sec.settings[setting.key] ?? setting.default;
                      return (
                        <label key={setting.key} className="rb-setting">
                          <span>{setting.label}</span>
                          {setting.kind === 'select' ? (
                            <Select
                              value={String(value)}
                              onChange={(e) => setSetting(i, setting.key, e.target.value)}
                            >
                              {(setting.options ?? []).map((o) => (
                                <option key={o.value} value={o.value}>
                                  {o.label}
                                </option>
                              ))}
                            </Select>
                          ) : (
                            <TextInput
                              type="number"
                              value={String(value)}
                              onChange={(e) =>
                                setSetting(i, setting.key, Number(e.target.value))
                              }
                            />
                          )}
                        </label>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          })}
        </div>

        <FormError form={form} />
      </div>
    </Modal>
  );
}
