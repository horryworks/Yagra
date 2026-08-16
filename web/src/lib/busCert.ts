// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind Settings ▸ Pollers ▸ Remote pollers (ADR-065), kept out of the `.tsx` so it
// can be tested — Vitest runs `src/**/*.test.ts` in a node environment and never executes a `.tsx`.
//
// Everything here is about one question an operator actually asks: **can a poller at that address
// connect, and if not, what has to change?** A TLS handshake fails unless the exact name the poller
// dials appears in the certificate's SAN list, and that failure surfaces at the remote site as a
// connection that never establishes — nothing on the central stack looks wrong. So the page has to
// answer it before the operator drives to the site, which means comparing what they typed against
// what the certificate carries, here.

import type { BusTlsView } from '../types/api';

/** How close to expiry the card starts warning. A bus certificate is not renewed automatically —
 *  see `bus_cert.rs` — so this is the operator's only prompt, and it has to leave time to visit the
 *  sites. 60 days rather than the WebUI certificate's 30 for exactly that reason: replacing this one
 *  means handing a new file to every remote poller, which is a scheduled job, not a click. */
export const BUS_CERT_WARN_DAYS = 60;

/** What the card says about the stored certificate, in the order an operator can act on. */
export type BusCertState =
  /** No certificate yet — the one-shot has not run, or this deployment has no bus store. */
  | 'absent'
  /** The private key will not decrypt. Nothing can be served or reissued until a new one is made. */
  | 'unreadable'
  /** Past `not_after`. Remote pollers are already refusing the handshake. */
  | 'expired'
  /** Stored but not on the volume the bus reads. */
  | 'not_materialized'
  /** Within `BUS_CERT_WARN_DAYS` of expiry. */
  | 'expiring'
  | 'ok';

/** Classify the stored certificate.
 *
 *  Ordered worst-first and returns exactly one state, because the card shows one line: a certificate
 *  that is both expired and unmaterialized is not two problems to an operator, it is one, and
 *  showing the milder of the two would be actively misleading. */
export function busCertState(cert: BusTlsView | null | undefined): BusCertState {
  if (!cert) return 'absent';
  if (cert.key_unreadable) return 'unreadable';
  if (cert.expires_in_days < 0) return 'expired';
  if (!cert.materialized) return 'not_materialized';
  if (cert.expires_in_days <= BUS_CERT_WARN_DAYS) return 'expiring';
  return 'ok';
}

/** Split what the operator typed into subject alternative names.
 *
 *  Accepts commas, whitespace and newlines, because all three are what a person pastes from a
 *  hosts file, a ticket or a spreadsheet. Deduplicated case-insensitively — DNS is case-insensitive
 *  and two SANs differing only in case would be one certificate entry pretending to be two, which
 *  makes `coversName` below disagree with the certificate it is describing. */
export function parseBusNames(text: string): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const raw of text.split(/[\s,]+/)) {
    const name = raw.trim();
    if (!name) continue;
    const key = name.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(name);
  }
  return out;
}

/** Does this certificate already cover `name`?
 *
 *  Case-insensitive, and **exact only** — no wildcard matching. That is deliberate rather than
 *  unfinished: `generate_self_signed` never emits a wildcard SAN, so accepting `*.example.net` here
 *  would report a name as covered that the certificate cannot actually present. A UI that is wrong
 *  in the permissive direction sends someone to a site to debug a connection this page told them
 *  would work. */
export function coversName(cert: BusTlsView | null | undefined, name: string): boolean {
  if (!cert) return false;
  const want = name.trim().toLowerCase();
  if (!want) return false;
  return cert.sans.some((s) => s.trim().toLowerCase() === want);
}

/** The names in `names` this certificate does not carry. Empty ⇒ every site can connect. */
export function namesNotCovered(cert: BusTlsView | null | undefined, names: string[]): string[] {
  return names.filter((n) => !coversName(cert, n));
}

/** The `.env` a remote site needs, given what the switch returned.
 *
 *  Built here rather than in the component for the reason above, and it is the one artifact the
 *  operator carries out of this screen: the secret is shown once, so if this string is wrong there
 *  is no second chance to notice before the site is configured. `lib/pollers.ts` builds the
 *  registration snippet; this adds only the three lines that differ once the bus is TLS. */
export function busPollerEnv(opts: {
  pollerId: string;
  pool: string;
  host: string;
  port?: number;
  secret: string;
  caPath?: string;
}): string {
  const port = opts.port ?? 4222;
  const ca = opts.caPath ?? '/etc/nats/certs/server-cert.pem';
  return [
    `YAGRA_POLLER_ID=${opts.pollerId}`,
    `YAGRA_POLLER_POOL=${opts.pool}`,
    // The username is the literal `poller` until per-poller tokens land (ADR-065 Inc.3); it is what
    // the static NATS account in nats-server.conf is called.
    `YAGRA_BUS_URL=tls://poller:${opts.secret}@${opts.host}:${port}`,
    `YAGRA_BUS_CA_FILE=${ca}`,
  ].join('\n');
}
