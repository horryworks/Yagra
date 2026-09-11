// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  adoptWidths,
  clampColumnWidth,
  clearColumn,
  clearTable,
  COLUMN_MAX_PX,
  COLUMN_MIN_PX,
  COLUMN_STEP_PX,
  freezeTracks,
  hasOverrides,
  MAX_STORED_COLUMNS,
  MAX_STORED_TABLES,
  resizeHandleLabel,
  resolveWidths,
  setWidths,
  trackPxAt,
  widthFromDrag,
  widthFromKey,
  widthsFor,
  type ColumnWidthDoc,
} from './columnWidths';

/** The Interfaces list, which is the widest table in the product and the one this shipped for. */
const COLUMNS = [
  { key: 'if_name', width: 'minmax(140px, 1.4fr)' },
  { key: 'if_alias', width: 'minmax(88px, 1.3fr)' },
  { key: 'oper', width: '94px' },
  { key: 'media', width: 'minmax(112px, 1fr)' },
  { key: 'flex' }, // no declared track at all — the `1fr` default
];

describe('resolveWidths', () => {
  it('hands back the very array it was given when nobody has dragged anything', () => {
    // The accepting case, first on purpose: a resolver that rebuilt every column would change what
    // every existing operator sees, and would satisfy each of the rejection tests below.
    expect(resolveWidths(COLUMNS, undefined)).toBe(COLUMNS);
    expect(resolveWidths(COLUMNS, {})).toBe(COLUMNS);
  });

  it('turns a stored width into a fixed pixel track and leaves the rest declared', () => {
    const out = resolveWidths(COLUMNS, { if_alias: 240 });
    expect(out.map((c) => c.width)).toEqual([
      'minmax(140px, 1.4fr)',
      '240px',
      '94px',
      'minmax(112px, 1fr)',
      undefined,
    ]);
  });

  it('sizes a column that declared no track at all', () => {
    expect(resolveWidths(COLUMNS, { flex: 300 })[4].width).toBe('300px');
  });

  it('never produces an `auto` track', () => {
    // An `auto` track sizes to its own content, so the header, the filter row and the data rows
    // would resolve one template to three different widths (ADR-054).
    const out = resolveWidths(COLUMNS, { if_name: 200, oper: 120 });
    expect(out.every((c) => c.width !== 'auto')).toBe(true);
  });

  it('clamps a stored width that is out of range', () => {
    expect(resolveWidths(COLUMNS, { oper: 5 })[2].width).toBe(`${COLUMN_MIN_PX}px`);
    expect(resolveWidths(COLUMNS, { oper: 99999 })[2].width).toBe(`${COLUMN_MAX_PX}px`);
  });

  it('ignores a stored key no column has, and a value that is not a number', () => {
    expect(resolveWidths(COLUMNS, { gone: 200 })).toBe(COLUMNS);
    expect(resolveWidths(COLUMNS, { oper: Number.NaN })).toBe(COLUMNS);
  });

  it('does not mutate the columns it was handed', () => {
    resolveWidths(COLUMNS, { if_name: 200 });
    expect(COLUMNS[0].width).toBe('minmax(140px, 1.4fr)');
  });
});

describe('clampColumnWidth', () => {
  it('leaves a usable width alone', () => {
    expect(clampColumnWidth(240)).toBe(240);
  });

  it('rounds to whole pixels', () => {
    expect(clampColumnWidth(240.4)).toBe(240);
  });

  it('holds the floor and the ceiling', () => {
    expect(clampColumnWidth(0)).toBe(COLUMN_MIN_PX);
    expect(clampColumnWidth(-500)).toBe(COLUMN_MIN_PX);
    expect(clampColumnWidth(COLUMN_MAX_PX + 1)).toBe(COLUMN_MAX_PX);
  });

  it('applies the floor as the OUTER bound', () => {
    // Between two constants this ordering cannot bite, which is exactly why it is pinned: the shape
    // has to survive someone making the ceiling depend on the window, where applying the floor
    // first would collapse a column to nothing (`mapPaneHeight.ts` learned this the hard way).
    expect(COLUMN_MIN_PX).toBeLessThan(COLUMN_MAX_PX);
    expect(clampColumnWidth(COLUMN_MIN_PX - 1)).toBe(COLUMN_MIN_PX);
  });
});

describe('widthFromDrag', () => {
  it('grows the column when the pointer moves right', () => {
    expect(widthFromDrag(200, 500, 560)).toBe(260);
  });

  it('shrinks it when the pointer moves left', () => {
    expect(widthFromDrag(200, 500, 440)).toBe(140);
  });

  it('is computed from the gesture origin, not accumulated', () => {
    // Two moves in one drag. An implementation that added each delta to the *current* width would
    // answer 320 for the second call; the origin form answers 260 both times it is asked about the
    // same pointer position, so a coalesced or dropped pointermove cannot make the edge creep.
    const first = widthFromDrag(200, 500, 530);
    const second = widthFromDrag(200, 500, 560);
    expect(first).toBe(230);
    expect(second).toBe(260);
  });

  it('clamps at both ends of a long drag', () => {
    expect(widthFromDrag(200, 500, -5000)).toBe(COLUMN_MIN_PX);
    expect(widthFromDrag(200, 500, 99999)).toBe(COLUMN_MAX_PX);
  });
});

describe('widthFromKey', () => {
  it('grows on ArrowRight, because the grip sits on the column’s right edge', () => {
    expect(widthFromKey(200, 'ArrowRight')).toBe(200 + COLUMN_STEP_PX);
  });

  it('shrinks on ArrowLeft', () => {
    expect(widthFromKey(200, 'ArrowLeft')).toBe(200 - COLUMN_STEP_PX);
  });

  it('claims no other key', () => {
    for (const k of ['ArrowUp', 'ArrowDown', 'Enter', ' ', 'Home', 'Tab']) {
      expect(widthFromKey(200, k)).toBeNull();
    }
  });

  it('clamps a held key at the bounds', () => {
    expect(widthFromKey(COLUMN_MIN_PX, 'ArrowLeft')).toBe(COLUMN_MIN_PX);
    expect(widthFromKey(COLUMN_MAX_PX, 'ArrowRight')).toBe(COLUMN_MAX_PX);
  });
});

describe('trackPxAt', () => {
  it('reads the used width of one track out of a resolved template', () => {
    expect(trackPxAt('160px 240px 94px', 1)).toBe(240);
  });

  it('reads a fractional used width', () => {
    expect(trackPxAt('160.5px 240px', 0)).toBe(160.5);
  });

  it('answers null for a grid that has not been laid out', () => {
    // An unrendered grid computes to `none`; guessing a number here would start a drag from a width
    // the column never had.
    expect(trackPxAt('none', 0)).toBeNull();
  });

  it('answers null past the end, and for a track that is not a pixel length', () => {
    expect(trackPxAt('160px 240px', 5)).toBeNull();
    expect(trackPxAt('160px 1fr', 1)).toBeNull();
    expect(trackPxAt('0px 240px', 0)).toBeNull();
    expect(trackPxAt('', 0)).toBeNull();
  });
});

describe('freezeTracks', () => {
  it('turns a laid-out grid into a width for every column', () => {
    expect(freezeTracks('160px 240px 94px', ['a', 'b', 'c'])).toEqual({ a: 160, b: 240, c: 94 });
  });

  it('is what makes “drag one column, the rest stay put” true', () => {
    // 🚨 Without this a table of `1fr` tracks hands the space between them: fixing one column wider
    // takes the difference out of the flexible ones and the table never grows, so the operator
    // watches the truncation move one column along. Measured on Events before the freeze: dragging
    // the first column +80px took exactly 80px off the other flexible track.
    const frozen = freezeTracks('160px 240px 94px', ['a', 'b', 'c']);
    const afterDrag: Record<string, number> = { ...frozen, a: 240 };
    const total = (w: Record<string, number>) => Object.values(w).reduce((s, n) => s + n, 0);
    expect(total(afterDrag)).toBe(total(frozen) + 80);
    expect(afterDrag.b).toBe(frozen.b);
    expect(afterDrag.c).toBe(frozen.c);
  });

  it('clamps what it measures', () => {
    expect(freezeTracks('10px 5000px', ['a', 'b'])).toEqual({
      a: COLUMN_MIN_PX,
      b: COLUMN_MAX_PX,
    });
  });

  it('skips a track it could not read rather than guessing one', () => {
    expect(freezeTracks('160px 1fr 94px', ['a', 'b', 'c'])).toEqual({ a: 160, c: 94 });
    expect(freezeTracks('none', ['a'])).toEqual({});
  });

  it('ignores tracks past the columns it was given', () => {
    expect(freezeTracks('160px 240px 94px', ['a', 'b'])).toEqual({ a: 160, b: 240 });
  });
});

describe('setWidths', () => {
  it('writes a whole gesture at once', () => {
    expect(setWidths({}, 't', { a: 160, b: 240 })).toEqual({ t: { a: 160, b: 240 } });
  });

  it('merges over what was already stored', () => {
    expect(setWidths({ t: { a: 100, c: 300 } }, 't', { a: 160, b: 240 })).toEqual({
      t: { a: 160, c: 300, b: 240 },
    });
  });

  it('clamps every value and drops the ones that are not numbers', () => {
    expect(setWidths({}, 't', { a: 5, b: 99999, c: Number.NaN })).toEqual({
      t: { a: COLUMN_MIN_PX, b: COLUMN_MAX_PX },
    });
  });

  it('writes nothing at all rather than an empty table', () => {
    expect(setWidths({}, 't', {})).toEqual({});
  });
});

describe('resizeHandleLabel', () => {
  it('uses the caller’s label when it has one', () => {
    expect(resizeHandleLabel('if_alias', 'Description', { if_alias: 'Description' })).toBe(
      'Description',
    );
  });

  it('falls back to the column’s own heading when that is a plain string', () => {
    expect(resizeHandleLabel('if_alias', 'Description', undefined)).toBe('Description');
    expect(resizeHandleLabel('if_alias', 'Description', {})).toBe('Description');
  });

  it('falls back to the key when the heading is not text', () => {
    // A heading that is an element (an icon, a tooltip wrapper) has no string to announce. The key
    // is a poor name and a much better one than nothing: `role="slider"` with an empty accessible
    // name is a control a screen reader cannot place at all.
    expect(resizeHandleLabel('actions', { type: 'div' }, undefined)).toBe('actions');
    expect(resizeHandleLabel('actions', undefined, undefined)).toBe('actions');
    expect(resizeHandleLabel('actions', '', undefined)).toBe('actions');
  });
});

describe('the stored document', () => {
  it('records a width and reads it back', () => {
    const doc = setWidths({}, 'events.log', { message: 320 });
    expect(widthsFor(doc, 'events.log')).toEqual({ message: 320 });
    expect(hasOverrides(doc, 'events.log')).toBe(true);
    expect(hasOverrides(doc, 'alerts.history')).toBe(false);
  });

  it('clamps on the way in', () => {
    expect(setWidths({}, 't', { c: 5 })).toEqual({ t: { c: COLUMN_MIN_PX } });
  });

  it('does not mutate the document it was given', () => {
    const doc: ColumnWidthDoc = { t: { a: 100 } };
    setWidths(doc, 't', { b: 200 });
    expect(doc).toEqual({ t: { a: 100 } });
  });

  it('drops the table when the last column is cleared', () => {
    const doc = setWidths({}, 't', { c: 200 });
    expect(clearColumn(doc, 't', 'c')).toEqual({});
    expect(hasOverrides(clearColumn(doc, 't', 'c'), 't')).toBe(false);
  });

  it('keeps the siblings when one column is cleared', () => {
    const doc = setWidths({}, 't', { a: 200, b: 300 });
    expect(clearColumn(doc, 't', 'b')).toEqual({ t: { a: 200 } });
  });

  it('is unchanged by clearing something that was never stored', () => {
    const doc: ColumnWidthDoc = { t: { a: 200 } };
    expect(clearColumn(doc, 't', 'b')).toBe(doc);
    expect(clearTable(doc, 'other')).toBe(doc);
  });

  it('clears a whole table', () => {
    const doc = setWidths(setWidths({}, 't', { a: 200 }), 'u', { b: 300 });
    expect(clearTable(doc, 't')).toEqual({ u: { b: 300 } });
  });

  it('treats an empty table map as no override', () => {
    expect(hasOverrides({ t: {} }, 't')).toBe(false);
    expect(widthsFor({ t: {} }, 't')).toBeUndefined();
  });
});

describe('the caps that keep the account document inside its 16 KiB', () => {
  it('evicts the first-inserted table and keeps the one the gesture just wrote', () => {
    let doc: ColumnWidthDoc = {};
    for (let i = 0; i < MAX_STORED_TABLES; i += 1) doc = setWidths(doc, `t${i}`, { a: 200 });
    doc = setWidths(doc, 'newest', { a: 200 });
    expect(Object.keys(doc)).toHaveLength(MAX_STORED_TABLES);
    expect(doc.newest).toBeDefined();
    expect(doc.t0).toBeUndefined();
    expect(doc.t1).toBeDefined();
  });

  it('evicts the first-inserted column and keeps the one the gesture just wrote', () => {
    let doc: ColumnWidthDoc = {};
    for (let i = 0; i < MAX_STORED_COLUMNS; i += 1) doc = setWidths(doc, 't', { [`c${i}`]: 200 });
    doc = setWidths(doc, 't', { newest: 200 });
    expect(Object.keys(doc.t)).toHaveLength(MAX_STORED_COLUMNS);
    expect(doc.t.newest).toBe(200);
    expect(doc.t.c0).toBeUndefined();
  });

  it('re-writing an existing column does not evict anything', () => {
    let doc: ColumnWidthDoc = {};
    for (let i = 0; i < MAX_STORED_COLUMNS; i += 1) doc = setWidths(doc, 't', { [`c${i}`]: 200 });
    doc = setWidths(doc, 't', { c0: 300 });
    expect(Object.keys(doc.t)).toHaveLength(MAX_STORED_COLUMNS);
    expect(doc.t.c0).toBe(300);
  });

  it('a document saturated at both caps stays well inside the endpoint’s allowance', () => {
    // 🚨 This is the assertion the two caps exist for. `PUT /api/v1/preferences` refuses a body over
    // 16 KiB (`MAX_USER_PREFS_BYTES`), the row is one per account, and every other WebUI preference
    // shares it — so this one must not be able to fill it alone. Ids are padded far past anything
    // real (`tableIds.ts` spells them in ~16 characters) so the bound holds for names nobody has
    // written yet.
    const doc: ColumnWidthDoc = {};
    for (let t = 0; t < MAX_STORED_TABLES; t += 1) {
      const table: Record<string, number> = {};
      for (let c = 0; c < MAX_STORED_COLUMNS; c += 1) {
        table[`column_key_padded_${String(c).padStart(4, '0')}`] = COLUMN_MAX_PX;
      }
      doc[`table_id_padded_to_thirty_two_${String(t).padStart(4, '0')}`] = table;
    }
    const bytes = JSON.stringify({ tableColumnWidths: doc }).length;
    expect(bytes).toBeLessThan(12 * 1024);
  });
});

describe('adoptWidths', () => {
  it('takes a document the WebUI itself wrote', () => {
    // Accepting case first, for the reason `resolveWidths`' first test gives: a reader that refused
    // everything would satisfy every rejection below and quietly drop every operator's widths.
    expect(adoptWidths({ 'events.log': { message: 320 } })).toEqual({
      'events.log': { message: 320 },
    });
  });

  it('clamps a value outside the range', () => {
    expect(adoptWidths({ t: { c: 99999 } })).toEqual({ t: { c: COLUMN_MAX_PX } });
  });

  it('survives anything at all, because the backend never looks inside', () => {
    for (const junk of [null, undefined, 42, 'nope', [], [1, 2], true]) {
      expect(adoptWidths(junk)).toEqual({});
    }
  });

  it('drops a table whose value is not an object, keeping the ones that are', () => {
    expect(adoptWidths({ bad: 'nope', worse: [1], good: { c: 200 } })).toEqual({
      good: { c: 200 },
    });
  });

  it('drops a column whose value is not a finite number', () => {
    expect(
      adoptWidths({ t: { a: '200', b: null, c: Number.NaN, d: Number.POSITIVE_INFINITY, e: 200 } }),
    ).toEqual({ t: { e: 200 } });
  });

  it('drops a table left with nothing', () => {
    expect(adoptWidths({ t: { a: 'x' } })).toEqual({});
    expect(adoptWidths({ t: {} })).toEqual({});
  });

  it('applies both caps to a document that arrived over them', () => {
    const tables: Record<string, Record<string, number>> = {};
    for (let i = 0; i < MAX_STORED_TABLES + 5; i += 1) tables[`t${i}`] = { a: 200 };
    expect(Object.keys(adoptWidths(tables))).toHaveLength(MAX_STORED_TABLES);

    const wide: Record<string, number> = {};
    for (let i = 0; i < MAX_STORED_COLUMNS + 5; i += 1) wide[`c${i}`] = 200;
    expect(Object.keys(adoptWidths({ t: wide }).t)).toHaveLength(MAX_STORED_COLUMNS);
  });
});
