// SPDX-License-Identifier: AGPL-3.0-only
// Shared flow-label helpers (ADR-031): IP protocol number → short name, and destination port →
// service label. Used by the NodeDetail Flow tab and the dashboard Traffic-flow widgets so both
// render protocols/ports the same way. Pure lookups with a numeric fallback — no i18n (these are
// protocol/service mnemonics, not translatable copy).

/** IP protocol number → short name (the common ones); unknown falls back to `IP <n>`. */
export const PROTO_NAMES: Record<number, string> = {
  1: 'ICMP',
  2: 'IGMP',
  6: 'TCP',
  17: 'UDP',
  47: 'GRE',
  50: 'ESP',
  58: 'ICMPv6',
  89: 'OSPF',
  132: 'SCTP',
};

/** Short name for an IP protocol number (`TCP`, `UDP`, …), else `IP <n>`. */
export const protoName = (p: number): string => PROTO_NAMES[p] ?? `IP ${p}`;

/** Well-known destination ports → service label; others render as the bare number. */
export const PORT_NAMES: Record<number, string> = {
  22: 'SSH',
  25: 'SMTP',
  53: 'DNS',
  80: 'HTTP',
  123: 'NTP',
  161: 'SNMP',
  179: 'BGP',
  443: 'HTTPS',
  514: 'syslog',
  3389: 'RDP',
};

/** A destination-port label: `443 · HTTPS` for well-known ports, else the bare number. */
export const portLabel = (port: number): string =>
  PORT_NAMES[port] ? `${port} · ${PORT_NAMES[port]}` : String(port);
