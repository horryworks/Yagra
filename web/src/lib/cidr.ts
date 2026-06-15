// Expand a discovery target spec into host addresses. The backend takes an explicit target
// list (and re-validates each IP), so this is a UI convenience that also caps the total size.
// A spec is a comma/whitespace-separated list whose tokens may each be a single IPv4, an IPv4
// CIDR, or an inclusive range.

/** Parse a dotted-quad IPv4 into a uint32, or null if malformed. */
function parseIp(s: string): number | null {
  const octets = s.split('.');
  if (octets.length !== 4) return null;
  let v = 0;
  for (const o of octets) {
    if (!/^\d{1,3}$/.test(o)) return null;
    const n = Number(o);
    if (n > 255) return null;
    v = (v << 8) | n;
  }
  return v >>> 0;
}

/** Format a uint32 as a dotted-quad IPv4. */
function ipToStr(v: number): string {
  return `${(v >>> 24) & 255}.${(v >>> 16) & 255}.${(v >>> 8) & 255}.${v & 255}`;
}

/** Expand an inclusive IPv4 range token: "a.b.c.d-e.f.g.h" or shorthand "a.b.c.d-N" (N is the
 *  end's last octet, sharing the start's first three). Returns [] if malformed, reversed, or
 *  larger than `max`. */
function expandRange(token: string, max: number): string[] {
  const dash = token.indexOf('-');
  const start = parseIp(token.slice(0, dash).trim());
  const endRaw = token.slice(dash + 1).trim();
  if (start === null || !endRaw) return [];

  let end: number | null;
  if (endRaw.includes('.')) {
    end = parseIp(endRaw);
  } else if (/^\d{1,3}$/.test(endRaw) && Number(endRaw) <= 255) {
    end = ((start & 0xffffff00) | Number(endRaw)) >>> 0; // replace last octet only
  } else {
    end = null;
  }
  if (end === null || end < start) return [];
  if (end - start + 1 > max) return [];

  const ips: string[] = [];
  for (let v = start; v <= end; v++) ips.push(ipToStr(v >>> 0));
  return ips;
}

/** Expand a free-form discovery target spec into host IPs. Accepts a comma- or whitespace-
 *  separated list whose tokens may each be a single IPv4 ("10.0.0.5"), an IPv4 CIDR
 *  ("192.168.1.0/24"), or an inclusive range ("192.168.1.10-192.168.1.20" or shorthand
 *  "192.168.1.10-20"). De-duplicates (first-seen order preserved). Returns [] if the spec is
 *  empty, any token is malformed/over-cap, or the combined total exceeds `max`. */
export function expandTargets(input: string, max = 1024): string[] {
  const tokens = input
    .split(/[,\s]+/)
    .map((t) => t.trim())
    .filter(Boolean);
  if (tokens.length === 0) return [];

  const seen = new Set<string>();
  const out: string[] = [];
  for (const token of tokens) {
    let ips: string[];
    if (token.includes('/')) {
      ips = expandCidr(token, max);
    } else if (token.includes('-')) {
      ips = expandRange(token, max);
    } else {
      const v = parseIp(token);
      ips = v === null ? [] : [ipToStr(v)];
    }
    if (ips.length === 0) return []; // malformed or over-cap token → reject the whole spec
    for (const ip of ips) {
      if (seen.has(ip)) continue;
      seen.add(ip);
      out.push(ip);
      if (out.length > max) return [];
    }
  }
  return out;
}

/** Expand an IPv4 CIDR (e.g. "192.168.1.0/24") into host IPs, or return [ip] for a bare address.
 *  Returns [] for malformed input or a range larger than `max`. Network + broadcast are
 *  excluded for prefixes ≤ /30; /31 and /32 include all addresses. */
export function expandCidr(input: string, max = 1024): string[] {
  const s = input.trim();
  if (!s) return [];
  if (!s.includes('/')) return [s];

  const [base, prefixStr] = s.split('/');
  const prefix = Number(prefixStr);
  const octets = base.split('.').map(Number);
  if (
    octets.length !== 4 ||
    octets.some((n) => !Number.isInteger(n) || n < 0 || n > 255) ||
    !Number.isInteger(prefix) ||
    prefix < 0 ||
    prefix > 32
  ) {
    return [];
  }

  const hostBits = 32 - prefix;
  if (hostBits > 16) return []; // far beyond any sane cap
  const total = 2 ** hostBits;
  if (total > max && hostBits > 1) return [];

  const baseInt =
    ((octets[0] << 24) | (octets[1] << 16) | (octets[2] << 8) | octets[3]) >>> 0;
  const mask = hostBits === 32 ? 0 : (0xffffffff << hostBits) >>> 0;
  const network = (baseInt & mask) >>> 0;

  // Skip the network + broadcast addresses for /≤30; include all for /31 and /32.
  const skipEnds = hostBits >= 2;
  const start = skipEnds ? 1 : 0;
  const end = skipEnds ? total - 1 : total;

  const ips: string[] = [];
  for (let i = start; i < end && ips.length < max; i++) {
    const ip = (network + i) >>> 0;
    ips.push(`${(ip >>> 24) & 255}.${(ip >>> 16) & 255}.${(ip >>> 8) & 255}.${ip & 255}`);
  }
  return ips;
}
