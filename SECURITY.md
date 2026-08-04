<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Security Policy

Yagra is a network monitoring system. It holds SNMP community strings, SNMPv3 credentials and
API tokens for the devices it watches, so a vulnerability here can become a vulnerability in the
network being monitored. Reports are taken seriously.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

- **Preferred:** [private vulnerability reporting](https://github.com/horryworks/Yagra/security/advisories/new)
  on this repository.
- **Alternative:** email **horryworks@gmail.com** with `[Yagra security]` in the subject.

Useful to include: the affected version or commit, the component (`yagra-core`, `yagra-poller`,
the WebUI, …), what an attacker gains, and a reproduction if you have one.

Yagra is maintained by one person. Expect an initial acknowledgement within **7 days**, and a fix
timeline proportional to severity rather than a fixed SLA. Credit is given in the release notes
unless you prefer otherwise.

## Supported versions

Only the most recent release is supported. Every image in the registry is a release — development
builds are never published there:

| Tag | Meaning |
|---|---|
| `ghcr.io/horryworks/yagra-*:v<version>` | A published release. Pin this. |
| `ghcr.io/horryworks/yagra-*:latest` | The latest **stable** release. Pre-releases (`-beta`, `-rc`) never move it. |
| `ghcr.io/horryworks/yagra-*:<git-sha>` | The immutable reference for one release. |

A running container reports what it was built from: `/etc/yagra-source-ref` holds the commit and
`/etc/yagra-build-profile` holds the compile profile (`release` for anything published here).

## Scope

**In scope:** everything in this repository — the Rust workspace under `crates/`, the WebUI under
`web/`, the SQL migrations, the Dockerfiles and the compose files.

**Out of scope, report upstream instead:**

- `vendor/snmp2/` — a vendored copy of an upstream crate, patched only for a build failure
  (`vendor/snmp2/PATCH_NOTE.md` documents exactly what was changed). Report upstream.
- Third-party dependencies — report to that project. If Yagra's *use* of a dependency is what
  makes an issue exploitable, that is in scope and worth reporting here.

## Deliberate design decisions that can look like bugs

These are known, intentional, and documented so a reviewer does not have to guess:

- **Secrets at rest use envelope encryption** (AES-256-GCM data keys wrapped by a KEK). The KEK is
  loaded from a **mounted file**, never from an environment variable. If `YAGRA_KEK_FILE` is not
  set, a random per-process KEK is generated so a developer can start the stack without a key —
  which means stored secrets do not survive a restart. That fallback is a development convenience;
  a real deployment must mount a KEK.
- **Poll jobs carry decrypted device credentials over the bus.** That is inherent to a stateless
  poller: the poller holds no secret store and is told what to use per job. Consequently, exposing
  NATS beyond the internal compose network requires the TLS + authentication configuration in
  `docker/nats/nats-server.conf`. Distributed deployments should additionally enable the NATS Auth
  Callout path, which mints per-poller-scoped credentials.
- **Group-scoped API tokens are rejected with `400`, not silently widened.** Group scope is
  designed and typed but not yet enforced at the query layer, so issuing such a token would hand an
  operator a credential they believe is least-privileged while it is in fact unrestricted. Refusing
  to mint it is the deliberate choice. Role-based permissions *are* enforced.
- **The MCP tool surface (`/mcp`) is disabled by default** (`YAGRA_ENABLE_MCP`) and always requires
  a token, including when the anonymous read-only dashboard is enabled.
- **The anonymous read-only dashboard is disabled by default** (`YAGRA_PUBLIC_DASHBOARD`).
- **The bootstrap admin password is generated randomly and printed once** to the core log on first
  start. There is no built-in default password.
- **The WebUI is served over HTTPS by default** on host `:443`, terminated in the web container.
  On first start core generates a **self-signed** certificate, so browsers warn until an operator
  imports a real one at Settings ▸ TLS. There is no plain-HTTP listener to fall back to.
- **The single-node `docker-compose.yml` is an evaluation stack.** It uses default database
  credentials and an ephemeral KEK. `DEPLOYMENT.md` describes what a real deployment changes.
- **Core's own API port is plaintext and, by default, reachable on the LAN.** Browsers do not use
  it — the web container proxies `/api/` and `/mcp` to core internally — but it is how machine
  clients (Prometheus, webhook senders, scripts) keep working before the certificate is trusted.
  Set `YAGRA_API_BIND=127.0.0.1` once those clients are on the TLS edge; Settings ▸ TLS says so too.
- **A PostgreSQL dump contains the WebUI's private key**, envelope-encrypted with the KEK like every
  other secret. Same sensitivity class as the device credentials `scripts/yagra-backup.sh` already
  captures, so nothing about backup handling changes — but the key is in there.
- **The poller container is granted `CAP_NET_RAW`** because ICMP needs a raw socket. No other
  container receives it, and containers run as non-root.

## Advisories already assessed

Re-reporting these is not necessary — the analysis is recorded here so it does not have to be
redone:

- **GHSA-qwww-vcr4-c8h2** — `react-router` / `react-router-dom`, high, "RSC Mode CSRF Bypass".
  **Not reachable in Yagra.** The WebUI is a purely client-side SPA built on `BrowserRouter`, with
  no React Server Components, no data-router server actions, and no server-side route handlers, so
  the vulnerable code path does not exist in the shipped bundle.
  ⚠️ **Do not run `npm audit fix --force`.** It downgrades to `react-router-dom@7.11.0`, which
  reintroduces an open redirect that *is* reachable — a strictly worse position. The real fix is
  React Router v8, which requires React 19; the two will be upgraded together.
- **RUSTSEC-2023-0071** — `rsa`, Marvin timing side-channel. Ignored in `deny.toml` with a written
  justification: the only RSA signing path in Yagra (Google service-account assertions for the
  BigQuery forwarding destination) is deliberately routed through `ring`, not through `rsa`, so the
  vulnerable primitive is not invoked.

## Hardening checklist for operators

`DEPLOYMENT.md` is the full reference; the security-relevant essentials:

1. Mount a KEK file and set `YAGRA_KEK_FILE`.
2. Change the PostgreSQL password from the compose default.
3. Import a real TLS certificate at Settings ▸ TLS, replacing the self-signed bootstrap one — then
   set `YAGRA_API_BIND=127.0.0.1` so core's plaintext API port stops answering on the LAN. Do them
   in that order: closing the port first breaks every machine client that cannot yet trust the
   certificate, and two simultaneous causes are hard to tell apart.
4. Keep NATS on an internal network, or enable its TLS + authentication configuration. This is a
   **separate certificate** from the WebUI's — the NATS server reads its own from
   `docker/nats/nats-server.conf` and Settings ▸ TLS does not manage it.
5. Prefer SNMPv3 over v2c wherever the device supports it.
6. Rotate the bootstrap admin password after first login, and use OIDC SSO where available.
