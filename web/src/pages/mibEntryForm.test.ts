// SPDX-License-Identifier: AGPL-3.0-only
// The only gate between an operator and the OID catalog.
//
// A bad entry here does not fail loudly: it is stored, offered in the collection editor, attached
// to a profile, and then reports nothing on every node using that profile — which reads as a device
// problem. So the shapes that must be refused are worth naming one by one.

import { describe, expect, it } from 'vitest';
import { isValidOid, mibEntryReady, OID_RE } from './mibEntryForm';

describe('the OID field', () => {
  it('accepts dotted-decimal identifiers', () => {
    for (const oid of ['1', '0', '1.3.6.1.2.1.1.1.0', '1.3.6.1.4.1.2011.5.25.31.1.1.1.1.5']) {
      expect(isValidOid(oid), oid).toBe(true);
    }
  });

  it('trims surrounding whitespace, since that is what gets stored', () => {
    // The submit path sends `oid.trim()`, so refusing this would reject an entry that is byte-wise
    // identical to one already in the catalog.
    expect(isValidOid('  1.3.6.1  ')).toBe(true);
    expect(isValidOid('\t1.3.6.1\n')).toBe(true);
  });

  it('refuses a symbolic or vendor-prefixed name', () => {
    // The string is stored verbatim and handed to the SNMP client, which resolves no MIB names.
    for (const oid of ['iso.3.6.1', 'sysDescr.0', 'SNMPv2-MIB::sysDescr.0', '1.3.6.1.2.1.1.1.0a']) {
      expect(isValidOid(oid), oid).toBe(false);
    }
  });

  it('refuses a malformed dotted form', () => {
    for (const oid of ['', '   ', '.', '.1.3', '1.3.', '1..3', '1.3 .6', '1,3,6', '1-3-6']) {
      expect(isValidOid(oid), oid).toBe(false);
    }
  });

  it('refuses numbers that are not plain arcs', () => {
    for (const oid of ['-1.2', '+1.2', '1.3e6', '1.5.0x10', '1.3.6.１']) {
      expect(isValidOid(oid), oid).toBe(false);
    }
  });

  it('anchors both ends, so a trailing line cannot smuggle anything in', () => {
    expect(isValidOid('1.3.6\nrm -rf')).toBe(false);
    expect(isValidOid('junk\n1.3.6')).toBe(false);
  });

  it('has no global flag, so repeated tests do not alternate', () => {
    // A `/g` regex carries `lastIndex` across calls, which would make the very same OID valid on
    // one keystroke and invalid on the next.
    expect(OID_RE.global).toBe(false);
    expect(OID_RE.test('1.3.6')).toBe(true);
    expect(OID_RE.test('1.3.6')).toBe(true);
  });
});

describe('the add-entry gate', () => {
  it('needs both a metric name and a valid OID', () => {
    expect(mibEntryReady('sys_descr', '1.3.6.1.2.1.1.1.0')).toBe(true);
    expect(mibEntryReady('', '1.3.6.1.2.1.1.1.0')).toBe(false);
    expect(mibEntryReady('sys_descr', '')).toBe(false);
    expect(mibEntryReady('', '')).toBe(false);
  });

  it('does not count a whitespace-only metric name', () => {
    // The submit path sends `metricName.trim()`, so a blank-looking name would be stored as ''.
    expect(mibEntryReady('   ', '1.3.6.1')).toBe(false);
    expect(mibEntryReady(' sys_descr ', '1.3.6.1')).toBe(true);
  });
});
