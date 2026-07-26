// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { PORT_NAMES, PROTO_NAMES, portLabel, protoName } from './flowLabels';

// These labels are what the NodeDetail Flow tab and the dashboard Traffic-flow widgets both render,
// so the contract that matters is: known values get a mnemonic, unknown values still render
// something useful, and neither path can produce an empty string.

describe('protoName', () => {
  it('names the common IP protocols', () => {
    expect(protoName(1)).toBe('ICMP');
    expect(protoName(6)).toBe('TCP');
    expect(protoName(17)).toBe('UDP');
    expect(protoName(58)).toBe('ICMPv6');
  });

  it('falls back to `IP <n>` for anything unmapped', () => {
    expect(protoName(253)).toBe('IP 253');
    expect(protoName(0)).toBe('IP 0');
  });

  it('never returns an empty label for any valid protocol number', () => {
    for (let p = 0; p <= 255; p += 1) {
      expect(protoName(p).length).toBeGreaterThan(0);
    }
  });
});

describe('portLabel', () => {
  it('renders `<port> · <service>` for well-known ports', () => {
    expect(portLabel(443)).toBe('443 · HTTPS');
    expect(portLabel(22)).toBe('22 · SSH');
    expect(portLabel(161)).toBe('161 · SNMP');
    expect(portLabel(514)).toBe('514 · syslog');
  });

  it('renders the bare number for anything else', () => {
    expect(portLabel(49152)).toBe('49152');
    expect(portLabel(0)).toBe('0');
  });

  it('always begins with the port number so sorting/scanning by port still works', () => {
    for (const port of [22, 80, 443, 8080, 65535]) {
      expect(portLabel(port).startsWith(String(port))).toBe(true);
    }
  });
});

describe('lookup tables', () => {
  it('cover the protocols and ports an NMS actually sees on the wire', () => {
    // SNMP/syslog/BGP are the ones a network operator will look for first; a regression that drops
    // them would silently degrade every flow view to bare numbers.
    for (const p of [1, 6, 17]) expect(PROTO_NAMES[p]).toBeDefined();
    for (const port of [53, 161, 179, 443, 514]) expect(PORT_NAMES[port]).toBeDefined();
  });

  it('has no blank entries', () => {
    for (const name of Object.values(PROTO_NAMES)) expect(name.trim()).not.toBe('');
    for (const name of Object.values(PORT_NAMES)) expect(name.trim()).not.toBe('');
  });
});
