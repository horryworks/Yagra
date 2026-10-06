// SPDX-License-Identifier: AGPL-3.0-only
// ADR-200 G3, G4, G5, G7, G8, G9 and G10: the amount of explanatory prose only goes down.
//
// The WebUI had about 100,000 characters of explanation on its screens (2026-10-05), written one
// feature at a time under ADR-055's "the text on screen is the manual". Nothing stopped the next
// paragraph, so deleting prose did not last. These tables are that stop. Since Inc.24 the shapes
// ADR-200 removed are forbidden outright: G4, G5, G9 and G10 each hold an allow-list of reasoned
// exceptions — no legacy list is left to shrink — and an entry that stops being needed fails too.
//
// - G3 `PROSE_CEILING` — per namespace, the characters in strings of five or more English words.
//   Fails above the ceiling, and also far below it (a ceiling left high is a ceiling that lets the
//   next paragraph back in): lower the number in the same change that removed the prose.
// - G4 `LONG_ALLOWED` — no string over 200 English / 120 Japanese characters, unless listed here
//   with the reason.
// - G5 `HOVER_ALLOWED` — no `title={t('…')}` over 60 English characters, unless listed with the
//   reason. Hover-only text cannot be read on touch (ADR-055 R4).
// - G7 `PAGE_NOTES` — every screen that passes its own `note` to `PageHeader` instead of taking
//   the nav description, with the reason. Only shrinks; an entry with `until` is a fact still
//   waiting for its place in the screen, and leaves in that increment.
// - G8 `INFO_COUNT` — every ⓘ (`InfoTip`) and pressable label (`InfoPress`). Its text is a `.info`
//   key, quoted once in the code, two sentences and 200 EN / 120 JA characters at most, and no
//   file draws more than three ⓘ. The count only moves with a reason: an ⓘ is the last resort.
//   `INFO_TEXT_SITES` names the files whose press shows a text that is not a `.info` key (a
//   metric's generated meaning), with the reason; those presses are not in the count.
// - G9 `POINTER_ALLOWED` — no string spells a menu path with `▸`, unless listed with the reason. A
//   pointer to a screen is a `ScreenLink`, which takes the names from the menu itself.
// - G10 `HINT_ALLOWED` — no static hint under a field or at the head of a dialog (`form-hint`,
//   `modal-hint`, a muted `FieldHint`), unless the file is listed with the reason. A validation
//   message is `FieldError` (or `Field`'s `error`); a line shown because of the form's state or
//   input is `form-status`; a cost stated before a click is `form-warning`.
//
// Raising a ceiling needs a reason in the commit message — a new screen's state message, say.
// What prose may stay at all is in `.claude/rules/ui-conventions.md` (the ADR-200 section).

import { describe, expect, it } from 'vitest';
import {
  flattenStrings,
  loadLocales,
  lookup,
  namespacesOf,
  resolveKey,
} from './testSupport/locales';
import {
  hintSites,
  hoverKeysIn,
  infoKeyAttrs,
  infoKeyLiterals,
  infoKeys,
  infoSites,
  isProse,
  jaFor,
  longKeys,
  noteKeyOf,
  NOT_MEASURED,
  pageHeaderTags,
  pointerKeys,
  proseMass,
  sentenceCount,
  wordCount,
} from './testSupport/prose';
import { readSources, SRC } from './testSupport/sources';

/** `[en, ja]` characters of prose per namespace. Measured 2026-10-06 (Inc.24); lower as prose goes. */
const PROSE_CEILING: Record<string, [number, number]> = {
  access: [1343, 736],
  alertNames: [166, 97],
  alerts: [942, 557],
  alertsConfig: [8360, 4431],
  auth: [79, 43],
  common: [717, 385],
  dashboard: [6229, 3448],
  format: [0, 0],
  metrics: [127, 65],
  monitoring: [11222, 6644],
  nav: [2907, 1367],
  nodes: [15378, 8693],
  rca: [1022, 528],
  reports: [643, 402],
  settings: [217, 101],
  'settings-ai': [736, 410],
  'settings-auth': [3540, 2095],
  'settings-forwarding': [1695, 900],
  'settings-relocation': [3661, 1808],
  'settings-tls': [1592, 857],
  'settings-tokens': [812, 442],
  'settings-upgrade': [3291, 1796],
  suppression: [866, 571],
  system: [12975, 7143],
  topology: [3104, 1757],
  troubleshoot: [3370, 1766],
};

/** How far below its ceiling a namespace may sit before the ceiling must come down. */
const SLACK: [number, number] = [300, 200];

/** Strings over 200 EN / 120 JA characters allowed on purpose, with the reason (G4). Checked both
 *  ways: a listed key that got shorter, or went away, has to leave. Empty since Inc.24 — the last
 *  one (the delete-node confirmation) said in 246 characters what fits in 150. */
const LONG_ALLOWED: Record<string, string> = {};

/** `title={t('…')}` strings over 60 EN characters allowed on purpose, with the reason (G5). Checked
 *  both ways. The detector reads every `title={t(` in a `.tsx`, and a component's `title` prop is
 *  not always a hover. */
const HOVER_ALLOWED: Record<string, string> = {
  'alertsConfig:routing.template.status.confirmTitle':
    "not a hover: the `title` prop of ConfirmDeleteModal is the dialog's heading, and it names " +
    'both halves of what the click does (templateEditor.spec.ts reads the dialog by it)',
};

/**
 * Why a screen passes its own `note` rather than taking its nav description (ADR-200 G7).
 *
 * - `fact` — the note says something the screen does not show yet. `until` names the increment
 *   that gives the fact a place (a badge, a state line, an info tip) and removes the note.
 * - `offNav` — a screen the menu does not list, so there is no nav description to take. Without
 *   `until`, its note must already fit the page-note limit (80 EN / 45 JA characters).
 * - `data` — the note is the screen's own numbers, not a sentence.
 */
interface PageNote {
  kind: 'data' | 'offNav' | 'fact';
  until?: string;
  /** For a note built from a key per subject rather than one literal key: the keys it can be, with
   *  `*` for the subject. Each one is held to the page-note limit. */
  noteKeys?: string;
  why: string;
}

const PAGE_NOTES: Record<string, PageNote> = {
  'pages/NodesPage.tsx': {
    kind: 'data',
    why: 'the fleet counts; Tier2a consistency.spec.ts reads them',
  },
  'pages/TopologyMapPage.tsx': { kind: 'offNav', why: 'opened from the tree, a node and a Geo map pin' },
  'pages/integrations/MerakiIntegrationPage.tsx': {
    kind: 'offNav',
    why: 'a page under Settings > Integrations',
  },
  'pages/integrations/MerakiOrgPage.tsx': {
    kind: 'offNav',
    why: 'one organization, opened from the Meraki page',
  },
  'pages/integrations/NetboxIntegrationPage.tsx': {
    kind: 'offNav',
    why: 'a page under Settings > Integrations',
  },
  'troubleshoot/report/ReportShell.tsx': {
    kind: 'offNav',
    noteKeys: 'troubleshoot:tools.*.desc',
    why: "one report per analysis tool; its note is the tool's line in the catalog",
  },
};

/** Every ⓘ and pressable label in the WebUI (ADR-200 G8). Raise only with a reason. */
// tip 27 (Inc.33): the set-parent dialog said what an upstream does under the field, the same
// sentence whatever the operator chose.
const INFO_COUNT = { tip: 27, press: 19 };

/** The files whose `<InfoPress` takes `text=` instead of an `.info` key, with the reason. Their
 *  text is not counted, capped or held to a key here, so each one says why that is right. Checked
 *  both ways. */
const INFO_TEXT_SITES: Record<string, string> = {
  'components/NodeDetail/OverviewTab.tsx':
    "a metric's meaning: the key is built from the metric name (metricMeaningKey) and the sentence " +
    'is generated from metric_meaning.rs, which MCP shares; ADR-200 changes how it is shown, not it',
};

/** No file draws more ⓘ than this (`<InfoTip`, or `<Field infoKey=…>`). */
const INFO_PER_FILE = 3;

/** The components that ARE the ⓘ, not screens that use one. */
const INFO_PRIMITIVES = ['components/ui/Field.tsx', 'components/ui/InfoTip.tsx'];

/** Keys that end in `.info` and are not an explanation: the `info` member of an enum, read with a
 *  built key (`severity.${s}`). Checked both ways, so a listed key that goes away leaves the list. */
const INFO_NOT_A_TIP: Record<string, string> = {
  'alerts:eventLog.action.info': 'the label of the `info` event-rule action',
  'dashboard:widgets.eventTriage.action.info': 'the label of the `info` event-rule action',
  'format:severity.info': 'the label of the `info` severity',
  'troubleshoot:findings.severity.info': 'the label of the `info` finding severity',
};

/** `▸` allowed on purpose, with the reason (G9). Checked both ways. */
const POINTER_ALLOWED: Record<string, string> = {
  'system:meraki.devices.underNetwork': 'a breadcrumb format, {{folder}} ▸ {{network}}',
};

/** Files allowed a static hint, with the reason (G10). Checked both ways. Empty since Inc.24: the
 *  57 hints left then became a suffix or placeholder in the box, a label, an ⓘ, a link, a
 *  `FieldError`, a `form-warning` before a click, or a `form-status` line drawn from state. */
const HINT_ALLOWED: Record<string, string> = {};

const locales = loadLocales();
const measured = Object.keys(locales).filter((ns) => !NOT_MEASURED.includes(ns));

describe('G3: prose per namespace stays under its ceiling', () => {
  it('counts words the way the budget means', () => {
    expect(wordCount('Save')).toBe(1);
    expect(wordCount('Delete {{count}} <b>nodes</b> now')).toBe(3);
    expect(isProse('No API tokens yet.')).toBe(false);
    expect(isProse('No API tokens exist yet.')).toBe(true);
    expect(proseMass({ a: 'one two three four five', b: 'short' }, { a: 'いちにさん' })).toEqual([
      23, 5,
    ]);
    expect(
      proseMass({ n_one: 'one node is not polled now', n_other: 'x' }, { n_other: '台' }),
    ).toEqual([26, 1]);
  });

  it('has one ceiling per measured namespace, and no other', () => {
    expect(measured.length).toBeGreaterThan(20);
    expect(Object.keys(PROSE_CEILING).sort()).toEqual([...measured].sort());
  });

  it('no namespace is over its ceiling or far under it', () => {
    const off = measured.flatMap((ns) => {
      const [en, ja] = proseMass(locales[ns].en, locales[ns].ja);
      const [ce, cj] = PROSE_CEILING[ns] ?? [0, 0];
      const bad = en > ce || ja > cj || ce - en > SLACK[0] || cj - ja > SLACK[1];
      return bad ? [`${ns}: measured [${en}, ${ja}], ceiling [${ce}, ${cj}]`] : [];
    });
    expect(off).toEqual([]);
  });
});

describe('G4: no new string longer than 200 EN / 120 JA characters', () => {
  it('finds a long string and skips a short one', () => {
    const fake = {
      x: {
        en: { a: 'e'.repeat(201), b: 'ok' },
        ja: { a: 'j', b: 'j'.repeat(121) },
      },
    };
    expect(longKeys(fake)).toEqual(['x:a', 'x:b']);
  });

  it('inspected every measured string', () => {
    const inspected = measured.reduce(
      (n, ns) => n + Object.keys(flattenStrings(locales[ns].en)).length,
      0,
    );
    // Thousands on 2026-10-06; the floor only proves the walk was not empty.
    expect(inspected).toBeGreaterThan(3000);
  });

  it('the long strings are exactly the allowed ones', () => {
    expect(longKeys(locales)).toEqual(Object.keys(LONG_ALLOWED).sort());
  });
});

describe('G5: no new sentence that only a hover can read', () => {
  const files = readSources(SRC, { exts: ['.tsx'] });
  const sites = files.flatMap(([file, src]) => {
    const ns = namespacesOf(src);
    return hoverKeysIn(src).map((k) => ({
      file,
      key: resolveKey(k, ns, locales),
    }));
  });

  it('inspected the tree it is supposed to be reading', () => {
    // 315 literal title={t(…)} sites on 2026-10-06.
    expect(sites.length).toBeGreaterThan(250);
    expect(
      hoverKeysIn('<b title={t(\'a.b\')} /><i title={t("c:d", { n })} /><u title={x} />'),
    ).toEqual(['a.b', 'c:d']);
  });

  it('the long hover strings are exactly the allowed ones', () => {
    const long = new Set<string>();
    for (const { key } of sites) {
      if (!key) continue;
      const cut = key.indexOf(':');
      const v = flattenStrings(locales[key.slice(0, cut)].en)[key.slice(cut + 1)] ?? '';
      if (v.length > 60) long.add(key);
    }
    expect([...long].sort()).toEqual(Object.keys(HOVER_ALLOWED).sort());
  });
});

describe('G7: a page note is the nav description unless the screen says why', () => {
  const files = readSources(SRC, { exts: ['.tsx'] });
  const sites = files.flatMap(([file, src]) =>
    pageHeaderTags(src).map((tag) => ({ file, src, tag })),
  );

  it('reads a page header the way the screen renders it', () => {
    const tags = pageHeaderTags(
      "<PageHeader title={t('a')} />\n// <PageHeader note={x} />\n" +
        "<PageHeader title={t('b')} note={t('c.d')} actions={rows.map((r) => <i>{r}</i>)} />",
    );
    expect(tags).toHaveLength(2);
    expect(tags.map(noteKeyOf)).toEqual([null, 'c.d']);
    expect(tags[1]).toContain('actions=');
  });

  it('inspected every page header', () => {
    // 55 on 2026-10-06.
    expect(sites.length).toBeGreaterThanOrEqual(45);
  });

  it('the screens that pass a note are exactly the declared ones', () => {
    const withNote = [...new Set(sites.filter((s) => /\bnote=/.test(s.tag)).map((s) => s.file))];
    expect(withNote.sort()).toEqual(Object.keys(PAGE_NOTES).sort());
  });

  it('a fact names the increment that removes it', () => {
    const open = Object.entries(PAGE_NOTES).filter(([, n]) => n.kind === 'fact' && !n.until);
    expect(open.map(([f]) => f)).toEqual([]);
  });

  /** `ns:a.*.c` → every `ns:a.<x>.c` the English locale has. */
  const keysMatching = (pattern: string): string[] => {
    const cut = pattern.indexOf(':');
    const ns = pattern.slice(0, cut);
    const re = new RegExp(
      '^' + pattern.slice(cut + 1).split('*').map((p) => p.replace(/[.]/g, '\\.')).join('[^.]+') + '$',
    );
    return Object.keys(flattenStrings(locales[ns].en))
      .filter((k) => re.test(k))
      .map((k) => `${ns}:${k}`);
  };

  const overNoteLimit = (nk: string): string[] => {
    const cut = nk.indexOf(':');
    const { en, ja } = locales[nk.slice(0, cut)];
    const e = String(lookup(en, nk.slice(cut + 1)) ?? '');
    const j = String(lookup(ja, nk.slice(cut + 1)) ?? '');
    return e.length > 80 || j.length > 45 ? [`${nk}: ${e.length} / ${j.length}`] : [];
  };

  it('expands a declared note pattern to the keys it names', () => {
    expect(keysMatching('troubleshoot:tools.*.desc')).toContain('troubleshoot:tools.anomaly.desc');
    expect(keysMatching('troubleshoot:tools.*.desc')).not.toContain('troubleshoot:tools.anomaly.name');
  });

  it('an off-menu note with no end date already fits the page-note limit', () => {
    const checked: string[] = [];
    const over = Object.entries(PAGE_NOTES)
      .filter(([, n]) => n.kind === 'offNav' && !n.until)
      .flatMap(([file, n]) => {
        if (n.noteKeys) {
          const keys = keysMatching(n.noteKeys);
          if (keys.length === 0) return [`${file}: ${n.noteKeys} names no key`];
          checked.push(...keys);
          return keys.flatMap(overNoteLimit);
        }
        return sites
          .filter((s) => s.file === file)
          .flatMap(({ src, tag }) => {
            const key = noteKeyOf(tag);
            const nk = key ? resolveKey(key, namespacesOf(src), locales) : null;
            if (!nk) return [`${file}: its note is not a literal t('…') key`];
            checked.push(nk);
            return overNoteLimit(nk);
          });
      });
    expect(over).toEqual([]);
    expect(checked.length).toBeGreaterThan(0);
  });
});

describe('G8: an explanation behind ⓘ is short, keyed, and counted', () => {
  const files = readSources(SRC, { skipDirs: ['api', 'locales'] });

  it('reads a source the way the components are written', () => {
    const src = [
      '// <InfoTip infoKey="a:x.info" label="x" />',
      '<Field label={l} htmlFor="f" infoKey="a:f.info">',
      "<InfoTip infoKey='a:y.info' label={t('y')} />",
      '<InfoPress infoKey={k} className="badge">{n}</InfoPress>',
      '<InfoPress text={meaning}>{label}</InfoPress>',
      "const K = { s: 'a:z.info' };",
    ].join('\n');
    expect(infoSites(src)).toEqual({ tip: 2, press: 1, pressText: 1 });
    expect(infoKeyLiterals(src)).toEqual(['a:f.info', 'a:y.info', 'a:z.info']);
    expect(infoKeyAttrs(src)).toEqual(['a:f.info', 'a:y.info']);
    expect(infoKeys({ a: { en: { x: { info: 'i' }, info: 'j', infoText: 'k' } } })).toEqual([
      'a:info',
      'a:x.info',
    ]);
    expect(sentenceCount('One. Two, e.g. this. Three?')).toBe(4);
    expect(sentenceCount('一つ。二つ。')).toBe(2);
  });

  it('inspected the tree it is supposed to be reading', () => {
    expect(files.length).toBeGreaterThan(500);
  });

  const literals = files.flatMap(([, src]) => infoKeyLiterals(src));
  const all = infoKeys(locales);
  const tips = all.filter((k) => !(k in INFO_NOT_A_TIP));

  it('every key it sets aside as not an explanation still exists', () => {
    expect(Object.keys(INFO_NOT_A_TIP).filter((k) => !all.includes(k))).toEqual([]);
  });

  it('every .info key is named exactly once in the code, and every named one exists', () => {
    expect([...new Set(literals)].sort()).toEqual(tips);
    expect(literals.filter((k, i) => literals.indexOf(k) !== i)).toEqual([]);
  });

  it('an infoKey is written with its namespace', () => {
    const bare = files.flatMap(([file, src]) =>
      infoKeyAttrs(src)
        .filter((k) => !/^[A-Za-z][\w-]*:/.test(k))
        .map((k) => `${file}: ${k}`),
    );
    expect(bare).toEqual([]);
  });

  it('each explanation is at most two sentences and 200 EN / 120 JA characters', () => {
    const over = tips.flatMap((nk) => {
      const cut = nk.indexOf(':');
      const { en, ja } = locales[nk.slice(0, cut)];
      const key = nk.slice(cut + 1);
      const e = flattenStrings(en)[key] ?? '';
      const j = jaFor(ja, key);
      const bad = e.length > 200 || j.length > 120 || sentenceCount(e) > 2 || sentenceCount(j) > 2;
      return bad ? [`${nk}: ${e.length} / ${j.length} characters`] : [];
    });
    expect(over).toEqual([]);
  });

  it(`no file draws more than ${INFO_PER_FILE} ⓘ, and the total is the counted one`, () => {
    const total = { tip: 0, press: 0 };
    const crowded: string[] = [];
    const withText: string[] = [];
    for (const [file, src] of files) {
      if (INFO_PRIMITIVES.includes(file)) continue;
      const n = infoSites(src);
      total.tip += n.tip;
      total.press += n.press;
      if (n.pressText > 0) withText.push(file);
      if (n.tip > INFO_PER_FILE) crowded.push(`${file}: ${n.tip}`);
    }
    expect(crowded).toEqual([]);
    expect(total).toEqual(INFO_COUNT);
    // A press with free text instead of a key is declared, with its reason, in both directions.
    expect(withText.sort()).toEqual(Object.keys(INFO_TEXT_SITES).sort());
  });
});

describe('G9: a pointer to another screen is a link, not a spelled-out menu path', () => {
  it('finds a ▸ in either language', () => {
    const fake = { x: { en: { a: 'See Settings ▸ Pollers.', b: 'ok' }, ja: { a: '', b: 'A ▸ B' } } };
    expect(pointerKeys(fake)).toEqual(['x:a', 'x:b']);
  });

  it('the strings with ▸ are exactly the allowed ones', () => {
    const found = pointerKeys(locales);
    // The allowed breadcrumb is itself a hit, so an empty answer means the walk read nothing.
    expect(found.length).toBeGreaterThan(0);
    expect(found).toEqual(Object.keys(POINTER_ALLOWED).sort());
  });
});

describe('G10: no new static hint under a field or at the head of a dialog', () => {
  const files = readSources(SRC, { exts: ['.tsx'] });

  it('tells a hint from an error', () => {
    const src = [
      '<FieldHint>a</FieldHint>',
      '<FieldHint error>b</FieldHint>',
      '<span className="form-hint">c</span>',
      '<p className="form-hint form-hint-error">d</p>',
      '<span className="modal-hint">e</span>',
      '// <span className="modal-hint">f</span>',
    ].join('\n');
    expect(hintSites(src)).toEqual({ fieldHint: 1, formHint: 1, modalHint: 1 });
  });

  it('inspected the tree it is supposed to be reading', () => {
    // 234 `.tsx` files on 2026-10-06.
    expect(files.length).toBeGreaterThan(200);
    // The shapes that replaced the hints are there to be read, so the walk is not over nothing.
    const replaced = files.filter(([, src]) => /\b(form-status|FieldError)\b/.test(src)).length;
    expect(replaced).toBeGreaterThan(20);
  });

  it('the files with a static hint are exactly the allowed ones', () => {
    const withHint = files
      .filter(([, src]) => {
        const n = hintSites(src);
        return n.fieldHint + n.formHint + n.modalHint > 0;
      })
      .map(([file]) => file);
    expect(withHint.sort()).toEqual(Object.keys(HINT_ALLOWED).sort());
  });
});
