import { describe, expect, it } from 'vitest';
import { parseField, serializeField } from './templateModel';
import {
  presetLanguage,
  presetSource,
  presetTemplate,
  TEMPLATE_PRESETS,
  type PresetLanguage,
} from './templatePresets';

const LANGS: PresetLanguage[] = ['en', 'ja'];

describe('presets', () => {
  it('every preset is a template the editor can read', () => {
    for (const preset of TEMPLATE_PRESETS) {
      for (const lang of LANGS) {
        const src = presetSource(preset, lang);
        expect(parseField(src.subject).ok, `${preset}/${lang} subject`).toBe(true);
        expect(parseField(src.body).ok, `${preset}/${lang} body`).toBe(true);
      }
    }
  });

  it('every preset saves back as the text it was written in', () => {
    for (const preset of TEMPLATE_PRESETS) {
      for (const lang of LANGS) {
        const model = presetTemplate(preset, lang);
        expect(serializeField(model, 'subject')).toBe(presetSource(preset, lang).subject);
        expect(serializeField(model, 'body')).toBe(presetSource(preset, lang).body);
      }
    }
  });

  it('the detailed preset recovers in its own words and leaves suppress to fire', () => {
    const model = presetTemplate('detailed', 'ja');
    expect(model.resolve.subject).not.toBeNull();
    expect(model.resolve.body).not.toBeNull();
    expect(model.suppress).toEqual({ subject: null, body: null });
  });

  it('picks the preset language from the UI language', () => {
    expect(presetLanguage('ja')).toBe('ja');
    expect(presetLanguage('ja-JP')).toBe('ja');
    expect(presetLanguage('en')).toBe('en');
    expect(presetLanguage('fr')).toBe('en');
  });
});
