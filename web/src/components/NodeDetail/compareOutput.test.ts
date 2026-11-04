// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { compareColumns, compareRows } from './compareOutput';

// The shape a lab Huawei switch printed (values replaced), CRLF as it arrives over telnet.
const VRP = [
  'Info: The system is now comparing the configuration, please wait.',
  'Warning: The current configuration is not the same as the next startup configuration file. There may be several differences, and the following are some configurations beginning from the first:',
  ' ====== Current configuration line 12 ======',
  'vlan batch 500 502 to 504 905 1000',
  '#',
  'stp mode vbst',
  '',
  ' ====== Configuration file line 12 ======',
  'vlan batch 500 502 to 504 1000',
  '#',
  'stp mode vbst',
  '',
].join('\r\n');

describe('compareColumns', () => {
  it('splits the two blocks and drops what comes before them', () => {
    const c = compareColumns(VRP);
    expect(c).not.toBeNull();
    expect(c?.running).toEqual({
      line: 12,
      lines: ['vlan batch 500 502 to 504 905 1000', '#', 'stp mode vbst'],
    });
    expect(c?.saved).toEqual({
      line: 12,
      lines: ['vlan batch 500 502 to 504 1000', '#', 'stp mode vbst'],
    });
  });

  it('keeps the two line numbers apart when the device reports different ones', () => {
    const c = compareColumns(
      ' ====== Current configuration line 9 ======\nvcmp role client\n#\n ====== Configuration file line 8 ======\n#',
    );
    expect(c?.running.line).toBe(9);
    expect(c?.saved.line).toBe(8);
  });

  it('answers null for output in any other shape, so it is shown as it came', () => {
    expect(compareColumns('Warning: something else\n+ sysname sw-01')).toBeNull();
    expect(compareColumns(' ====== Current configuration line 3 ======\nx')).toBeNull();
    expect(
      compareColumns(' ====== Configuration file line 3 ======\nx\n ====== Current configuration line 3 ======\ny'),
    ).toBeNull();
  });
});

describe('compareRows', () => {
  it('marks only the rows whose two sides differ', () => {
    const c = compareColumns(VRP);
    if (!c) throw new Error('expected columns');
    expect(compareRows(c).map((r) => r.differs)).toEqual([true, false, false]);
  });

  // The shape TDC-class switches printed for `vcmp role client`: one extra line on the running
  // side, then the same lines. Only that line is marked; the rest stay paired.
  it('puts a line only one side has on a row of its own', () => {
    const c = compareColumns(
      ' ====== Current configuration line 9 ======\nvcmp role client\n#\nvlan batch 21 25\nvlan batch 94\n' +
        ' ====== Configuration file line 9 ======\nvlan batch 21 25\nvlan batch 94',
    );
    if (!c) throw new Error('expected columns');
    expect(compareRows(c)).toEqual([
      { running: 'vcmp role client', saved: undefined, differs: true },
      { running: '#', saved: undefined, differs: true },
      { running: 'vlan batch 21 25', saved: 'vlan batch 21 25', differs: false },
      { running: 'vlan batch 94', saved: 'vlan batch 94', differs: false },
    ]);
  });

  it('shows a line missing from the running side on the saved side only', () => {
    const c = compareColumns(
      ' ====== Current configuration line 4 ======\na\nc\n ====== Configuration file line 4 ======\na\nb\nc',
    );
    if (!c) throw new Error('expected columns');
    expect(compareRows(c)).toEqual([
      { running: 'a', saved: 'a', differs: false },
      { running: undefined, saved: 'b', differs: true },
      { running: 'c', saved: 'c', differs: false },
    ]);
  });

  it('pairs rows by position past the alignment bound', () => {
    const many = Array.from({ length: 401 }, (_, i) => `l${i}`).join('\n');
    const c = compareColumns(
      ` ====== Current configuration line 1 ======\nx\n${many}\n ====== Configuration file line 1 ======\n${many}`,
    );
    if (!c) throw new Error('expected columns');
    const rows = compareRows(c);
    expect(rows).toHaveLength(402);
    expect(rows[0]).toEqual({ running: 'x', saved: 'l0', differs: true });
  });
});
