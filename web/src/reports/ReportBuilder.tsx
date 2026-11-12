// SPDX-License-Identifier: AGPL-3.0-only
// Report builder — create/edit a report definition (template). Reuses the Modal + Field controls.
// Left: report name/description/window + the ordered list of chosen sections with their settings.
// "Add section" reveals the section catalog (grouped), mirroring the dashboard widget picker. Local
// form state (save-on-submit), not an autosave store — a template is committed explicitly.

import { DragGrip, SortableItem, SortableList } from '../components/ui/Sortable';
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { Button } from '../components/ui/Button';
import { Field, TextInput, Select } from '../components/ui/Field';
import { FieldRow, Fields } from '../components/ui/FormLayout';
import { Inline, Stack } from '../components/ui/Page';
import { SectionHead } from '../components/ui/Section';
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
  /** Put the sections in the order the drag left them (ADR-255). */
  function reorderSections(ids: string[]) {
    setSections((s) => ids.flatMap((id) => s.filter((sec) => sec.id === id)));
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
      <Fields>
        <Field label={t('builder.name')} required>
          <TextInput
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={t('builder.namePlaceholder')}
            autoFocus
          />
        </Field>

        <Field label={t('builder.description')}>
          <TextInput
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder={t('builder.descriptionPlaceholder')}
          />
        </Field>

        <Field label={t('builder.timeWindow')}>
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
        </Field>

        <SectionHead
          title={t('builder.sectionsCount', { count: sections.length })}
          actions={
            <Button variant="ghost" onClick={() => setShowCatalog((v) => !v)}>
              {showCatalog ? t('builder.hideCatalog') : t('builder.addSection')}
            </Button>
          }
        />

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
          <SortableList
            ids={sections.map((sec) => sec.id)}
            onReorder={reorderSections}
            nameOf={(id) => {
              const sec = sections.find((x) => x.id === id);
              return sec ? (defByKind.get(sec.kind)?.title ?? sec.kind) : id;
            }}
          >
          {sections.map((sec, i) => {
            const def = defByKind.get(sec.kind);
            return (
              <SortableItem key={sec.id} id={sec.id} className="rb-section">
              <Stack gap={8}>
                <div className="rb-section-top">
                  <Inline gap={4}>
                    <DragGrip name={def?.title ?? sec.kind} />
                    <span className="rb-section-title">{def?.title ?? sec.kind}</span>
                  </Inline>
                  <div className="rp-actions">
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
                  <FieldRow>
                    {def.settings.map((setting) => {
                      const value = sec.settings[setting.key] ?? setting.default;
                      return (
                        <Field key={setting.key} label={setting.label}>
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
                        </Field>
                      );
                    })}
                  </FieldRow>
                )}
              </Stack>
              </SortableItem>
            );
          })}
          </SortableList>
        </div>

        <FormError form={form} />
      </Fields>
    </Modal>
  );
}
