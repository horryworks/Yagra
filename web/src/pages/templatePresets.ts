// SPDX-License-Identifier: AGPL-3.0-only
// Starting points the notification-template editor offers (ADR-039 Inc.2): "Detailed" and
// "Short", in the operator's language.
//
// Written as the template text the editor itself saves, and read through `parseField`, so a preset
// is exactly a template an operator could have built tag by tag - and `templatePresets.test.ts`
// fails if one stops being readable. They are operator-facing text in two languages, which is why
// they are not locale keys: a preset is a whole template, and translating it fragment by fragment
// would split one sentence across keys that a translator cannot see together.

import { parseField, templateFrom, type VisualTemplate, type FieldBranches } from './templateModel';

export const TEMPLATE_PRESETS = ['detailed', 'short'] as const;
export type TemplatePreset = (typeof TEMPLATE_PRESETS)[number];
export type PresetLanguage = 'en' | 'ja';

const SOURCES: Record<TemplatePreset, Record<PresetLanguage, { subject: string; body: string }>> = {
  detailed: {
    en: {
      subject:
        '{% if event == "resolve" %}Recovered: {{ subject_name }}{% else %}{{ severity }} {{ subject_name }} is {{ state }}{% endif %}',
      body:
        '{% if event == "resolve" %}{{ subject_name }} has recovered.\nAt (UTC): {{ at }}\nTags: {{ tags | join(", ") }}' +
        '{% else %}Device: {{ subject_name }} ({{ node_address | default("—") }})\nFolder: {{ group | default("—") }}\n' +
        '{% if metric is defined and value is defined and threshold is defined %}{{ metric }} reached {{ value }} (threshold {{ threshold }})\n{% endif %}' +
        'At (UTC): {{ at }}\nTags: {{ tags | join(", ") }}{% endif %}',
    },
    ja: {
      subject:
        '{% if event == "resolve" %}復旧: {{ subject_name }}{% else %}{{ severity }} {{ subject_name }} が {{ state }} になりました{% endif %}',
      body:
        '{% if event == "resolve" %}{{ subject_name }} は復旧しました。\n復旧時刻 (UTC): {{ at }}\nタグ: {{ tags | join(", ") }}' +
        '{% else %}対象: {{ subject_name }}（{{ node_address | default("—") }}）\nフォルダ: {{ group | default("—") }}\n' +
        '{% if metric is defined and value is defined and threshold is defined %}{{ metric }} が {{ value }} になりました（しきい値 {{ threshold }}）\n{% endif %}' +
        '発生時刻 (UTC): {{ at }}\nタグ: {{ tags | join(", ") }}{% endif %}',
    },
  },
  short: {
    en: {
      subject: '{{ subject_name }}: {{ state }}',
      body: '{{ subject_name }} is {{ state }}.\nAt (UTC): {{ at }}',
    },
    ja: {
      subject: '{{ subject_name }}: {{ state }}',
      body: '{{ subject_name }} が {{ state }} になりました。\n発生時刻 (UTC): {{ at }}',
    },
  },
};

/** The source text of a preset, for the test that proves it reads back. */
export function presetSource(preset: TemplatePreset, lang: PresetLanguage): { subject: string; body: string } {
  return SOURCES[preset][lang];
}

function branches(src: string): FieldBranches {
  const parsed = parseField(src);
  // A preset that cannot be read is a defect in this file, caught by its test; an empty row is the
  // harmless thing to show if one ever ships.
  return parsed.ok ? parsed.branches : { fire: [] };
}

/** A preset as the editor's model. */
export function presetTemplate(preset: TemplatePreset, lang: PresetLanguage): VisualTemplate {
  const src = SOURCES[preset][lang];
  return templateFrom(branches(src.subject), branches(src.body));
}

/** The preset language for a UI language code. */
export function presetLanguage(uiLanguage: string): PresetLanguage {
  return uiLanguage.toLowerCase().startsWith('ja') ? 'ja' : 'en';
}
