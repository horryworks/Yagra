// Expand an IPv4 CIDR into host addresses for a discovery sweep. The backend takes an explicit
// target list (and re-validates each IP), so this is a UI convenience that also caps the size.

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
