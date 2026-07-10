# Release Notes

## v0.1.7

**Host-resource trends, fuller localization, and a smoother large-fleet + upgrade experience** — System
Health now charts CPU/load/memory/disk over time for the core and every poller, more of the WebUI is
available in Japanese, and both the polling scheduler and shutdown behavior were hardened for scale and
clean restarts.

### New Features
- **Host-resource trends in System Health** — the System Health page now charts **CPU, load, memory, and
  disk** over time for the **core and each poller**, so you can watch resource pressure build up rather
  than only seeing the current snapshot.

### Improvements
- **Fuller English / 日本語 localization** — the remaining WebUI areas — the application shell (top bar,
  sidebar, breadcrumbs), alerts triage, the shared node picker and time-range controls, and the
  remaining screens — are now localized, so switching to Japanese covers far more of the interface. Any
  still-untranslated text falls back to English.
- **Reorganized navigation** — the sidebar is grouped into **Monitor** and **Configure** sections with
  clearer labels, and third-party **Integrations** are gathered into a catalog.
- **Graceful shutdown** — the core and pollers now stop cleanly on `SIGTERM` / Ctrl-C (for example during
  a `docker compose` restart or a rolling upgrade), draining in-flight work and in-flight API requests
  instead of being terminated abruptly.
- **Faster scheduling and ingest for large fleets** — the poll scheduler builds each pool's working set
  concurrently, and notification delivery now runs off the result-ingest path, so a slow notification
  endpoint can no longer stall metric ingestion — both help throughput at tens of thousands of nodes.

### Bug Fixes
- **UI sizing/style consistency** — corrected several WebUI text sizes and modal styles that referenced a
  non-existent style token and rendered slightly larger than intended.

## v0.1.6

**Multi-language WebUI + per-interface metrics on SNMPv3 nodes** — the interface can now be switched
between English and Japanese instantly from Settings ▸ Preferences, and SNMPv3-monitored devices now
collect the same per-interface metrics that v2c devices already did.

### New Features
- **Interface language switching (English / 日本語)** — a new **Settings ▸ Preferences ▸ Language**
  control switches the whole UI language on the fly, with no reload. English is the default and the
  choice is remembered per browser. The application framework — top navigation, sidebar, breadcrumbs,
  sign-in, and the Preferences and About screens — and all shared value formatting (dates and times,
  relative times like "5m ago", node-status and HTTP-status labels, certificate-expiry text) are
  localized, with the Japanese resources loaded on demand only when you switch (zero cost on the
  default English path). Remaining screens are being localized progressively; any not-yet-translated
  text falls back to English, so nothing is ever blank.
- **Per-interface metrics on SNMPv3 nodes** — SNMPv3 (USM) nodes now walk interface tables with
  GETBULK the same way v2c nodes do, so per-interface counters are collected on devices that only
  permit v3. Previously v3 nodes were limited to scalar metrics.

## v0.1.5

**Distributed tracing (OpenTelemetry)** — Yagra can now export OpenTelemetry traces so a single
poll is traceable end to end across the central core and its pollers. It is opt-in and off by
default (structured logs and Prometheus metrics are unchanged), so there is zero overhead until you
point it at a collector.

### New Features
- **OpenTelemetry distributed tracing (self-observability)** — set `YAGRA_OTEL_ENDPOINT` (or the
  standard `OTEL_EXPORTER_OTLP_ENDPOINT`) to an OTLP/HTTP collector and both core and poller export
  spans that stitch one poll into a single trace — core's dispatch → the poller's poll → core's
  result ingest — plus a span per northbound API request. The trace context rides with jobs and
  results over the bus, so a trace spans distributed pollers and stays compatible with older pollers
  during a rolling upgrade. For large fleets, sample with `OTEL_TRACES_SAMPLER=parentbased_traceidratio`
  (+ `OTEL_TRACES_SAMPLER_ARG`) instead of tracing every poll. `docker compose --profile tracing up`
  starts a bundled Jaeger to view traces locally; leave the endpoint unset and Yagra logs exactly as
  before with no tracing overhead. See **DEPLOYMENT.md ▸ Distributed tracing (OpenTelemetry)**.

### Security
- **Patched a transitive advisory** — updated `crossbeam-epoch` to 0.9.20 to clear RUSTSEC-2026-0204
  (an invalid pointer dereference reachable only by debug-formatting a null pointer inside a metrics
  dependency; not exercised by Yagra, patched proactively).

## v0.1.4

**Dependency management** — the dependency graph that drives alert suppression is now editable, not
just viewable. Set a node's upstream from the WebUI, and dependency suppression now reacts to
parent-down events so a child that failed first is rolled up under its parent instead of paging on
its own.

### New Features
- **Manage node dependencies from the WebUI** — a node's upstream (the edge that drives parent-down
  alert suppression and root-cause roll-up) can now be set, changed, or cleared after the node is
  created. The node-detail header gains a **Dependency…** action, and a new **Topology ▸ Dependency
  view** page lists every node with its upstream, live status, and current root-cause attribution,
  and lets you edit each edge inline. Self-dependencies and cycles are rejected. The Network map
  already visualized these edges; now you can define them.

### Bug Fixes
- **Dependency suppression no longer misses out-of-order outages** — root-cause attribution is now
  event-driven. Previously a child's alert was attributed to its parent only at the moment the child
  first went down, so a child that failed *before* its parent was never rolled up and kept paging on
  its own. Now, when a parent goes down, already-active downstream alerts are re-evaluated and rolled
  up under the parent's incident (their standalone page is closed); symmetrically, a child left
  suppressed after its parent recovers while still down re-pages on its own.

## v0.1.3

**Distributed poller pools** — Yagra can now spread polling across pollers placed close to the
monitored devices. Each node carries a **pool** attribute (a site or region); the central server
tracks poller liveness, assigns each pool's nodes across its live pollers with consistent hashing,
and pushes each poller its working set over the bus. A remote poller dials out to the central NATS
bus (NAT/firewall-friendly), so branch-site segments can be monitored without inbound access. This
release also brings the passive event monitoring, topology map, and per-node event features.

### New Features
- **Distributed poller pools (location-affinity assignment + failover)** — deploy pollers at remote
  sites and assign nodes to them by a **pool** attribute (defaults to one pool, `default`). The
  central coordinator watches poller heartbeats, distributes each pool's work across its live
  pollers using a consistent-hash ring (so adding or losing a poller reshuffles the minimum), and
  fails a departed poller's nodes over to the survivors in the same pool automatically. Pollers
  receive their assignment as a working-set snapshot and then incremental deltas, and reconnect with
  a full resync — no polling gaps across restarts. A new **Settings ▸ Pollers** page lists every
  poller with its pool, status, version, working-set size, and last heartbeat, warns when a pool has
  nodes but no live poller, and includes a **Register poller** dialog that generates the remote
  poller's configuration. A pool with no live poller falls back to the existing central polling, so
  upgrades are seamless. Ships with a `docker-compose.poller.yml` for remote sites and a NATS
  TLS + authentication configuration for exposing the bus across trust boundaries.
- **Passive event monitoring (syslog / SNMP traps / webhooks)** — besides active polling, Yagra now
  receives syslog (RFC 5424/3164) and SNMP v1/v2c traps and accepts inbound webhooks, matches them
  against operator-defined rules (substring / regex) to raise alerts, and can forward the
  fire/resolve lifecycle to PagerDuty (Events API v2) and Jira Service Management (Alerts API).
- **Topology Network map** — a new visualization of the dependency graph, so you can see how nodes
  relate and how a parent outage cascades to its children.
- **Per-node event view** — the node-detail page gains an **Events** tab, and the Events page can be
  filtered to a single node (with a node picker and a `?node_id=` deep link). The event log fetching,
  paging, and columns are shared between the two views.
- **Free-text search on Alerts ▸ Events** — filter the event log by an arbitrary search string.

## v0.1.2

Cisco Meraki monitoring over the **read-only** Dashboard API. Meraki devices appear as ordinary
nodes in the same HostTree as your SNMP/ICMP hosts, but are collected per **organization** (one
paged, org-wide API call covers many devices) so a large Meraki estate never trips the org's API
rate limit — and the integration can only ever *read* from Meraki.

### New Features
- **Cisco Meraki (Dashboard API) monitoring** — add a Meraki organization under **Settings ▸
  Integrations ▸ Cisco Meraki** with a read-only API key (one key can onboard several organizations
  at once). An import wizard enumerates the org's networks and devices and lets you choose which to
  monitor; imported devices become normal nodes, auto-placed under an **Organization → Network**
  group tree, and carry a **Meraki** badge in the inventory tree. Collected metrics — device
  availability, WAN uplink loss / latency / status, client count, and traffic usage — surface on a
  new Cisco Meraki card on the node-detail page. Per-organization controls let you pause/resume
  collection, tune per-tier polling cadence and the request-rate budget, and edit which networks are
  in scope.

### Security
- **Read-only by design, with layered safeguards** — the integration issues HTTP **GET only** to
  Meraki (no configuration is ever written), and every request is restricted to allow-listed Meraki
  API hosts so the API key cannot be sent elsewhere. The key is **encrypted at rest** (never
  returned by the API or written to logs), polling is paced well under the per-organization API cap
  to leave headroom for your own tooling, and a global **Meraki polling kill switch** on the
  Integrations page can halt all Meraki collection instantly.

## v0.1.1

A small follow-up to the first release: a dedicated About page, a self-health page split out of
Pollers, and a genuinely rootless WebUI container.

### New Features
- **Settings ▸ About** — a new page that gathers product identity and running versions in one place:
  the live Core / API version (via a new public `GET /api/v1/version` endpoint), the WebUI build
  version, the repository link, and the license. Showing both versions side by side makes a
  core/web skew during a rolling upgrade obvious at a glance.

### Improvements
- **System Health is now its own page** — Yagra's self-health (poll-loop counters, backing-service
  reachability, data coverage) moved out of Settings ▸ Pollers into a dedicated Settings ▸ System
  Health page. The Pollers entry is reserved as a placeholder for future distributed-poller
  configuration.

### Security
- **The WebUI container now runs fully rootless** — the web image is built on `nginx-unprivileged`
  and serves as a non-root user, so it listens on container port **8080** instead of 80. The
  bundled Docker Compose files already map this, so standard deployments are unaffected (the WebUI
  stays on host port 3000 by default); update any custom reverse proxy or orchestration that
  targeted the web container's port 80.

## v0.1.0

First release of Yagra — a network monitoring system (NMS) that watches network devices and
servers via ICMP / SNMP / API calls, built from the start for tens of thousands of nodes and
distributed polling. Ships as Docker containers with a Rust backend and a React WebUI.

### New Features
- **Monitoring & polling** — ICMP liveness, SNMP v2c and v3 (USM auth/priv) scalar and table
  collection (including multi-index tables), and URL / HTTP(S) endpoint monitoring with TLS
  certificate-expiry tracking. Raw counters are stored and rates derived at query time (wrap/reset
  safe). Configurable global and per-Device-Profile poll intervals, with interval jitter and
  per-device rate limiting / backpressure.
- **Discovery & classification** — network discovery over IP ranges and lists with a
  stored-credential picker; automatic device classification by sysObjectID / sysDescr that applies
  the matching Device Profile; a Credential Finder that tries stored credentials by reference
  (rate-limited per device, attempted values never logged).
- **Device profiles & collection** — a role × network-OS profile taxonomy with editable Collection
  Templates, Classification Rules, and a curated MIB/OID catalog, all searchable. Built-in coverage
  for common vendors (Cisco, Huawei — including USG firewall sessions and RAM, Meraki MX/MS, A10)
  plus standard host and interface MIBs.
- **Alerting** — an explicit state machine with dwell-time hysteresis, flapping detection,
  dependency suppression with root-cause roll-up (parent down ⇒ children suppressed), maintenance
  windows, mutes, and dedup / grouping. Append-only alert history shows human-readable node/check
  names and a "What" (metric + condition) column, with read-only acknowledgement reflected from
  external tools such as PagerDuty / Jira Service Management (ADR-015). Email and webhook
  notification channels with retry.
- **Dashboards & visualization** — an admin-customizable Shared Dashboard and a per-user,
  multi-board My Dashboard built from a widget library: fleet health timeline, Top-N CPU / memory /
  interfaces, traffic deltas, aggregate throughput, utilization heatmap, geo map, dependency /
  root-cause topology, data-coverage gauge, alert-history aggregations, and poller / collection
  health. A node tree with split-view node detail, per-interface sparklines, and time-series charts
  with a shared range control (1h–7d + custom) and configured-bandwidth reference lines.
- **Reports** — customizable report sections rendered to HTML / CSV / PDF, with saved definitions
  and scheduled runs (Dashboard ▸ Reports).
- **Troubleshooting** — an analysis-tools catalog running asynchronous analysis jobs over
  VictoriaMetrics (e.g. anomaly detection), scoped to a single node or a group (recursively).
- **Administration** — role-based access control (viewer / operator / admin) with group-scoped
  visibility, user management, an immutable audit log of every configuration change and login, and
  Yagra self-health monitoring (Settings ▸ Pollers and `GET /api/v1/system-health`).
- **WebUI** — a React + TypeScript single-page app over one typed REST client, with live updates
  via SSE (bearer-authenticated), light / dark theming, and virtualized, server-paged lists built
  for tens of thousands of nodes.
- **Deployment** — a single-node Docker Compose stack (Yagra-core, Yagra-poller, Yagra-web,
  PostgreSQL, Redis, NATS, VictoriaMetrics) with non-root images and `CAP_NET_RAW` granted only to
  the poller for raw-socket ICMP. Expand-contract database migrations keep upgrades data-preserving.

### Security
- Monitoring credentials (SNMP communities, SNMPv3 USM credentials, API tokens) are encrypted at
  rest with envelope encryption — AES-256-GCM data keys wrapped by a key-encryption key loaded from
  a mounted secret file. Secrets are never logged, returned in API responses, or used as metric /
  trace labels.
- Every state-changing API endpoint enforces authentication and RBAC and is recorded in the audit
  log.
- Outbound HTTP from URL monitors and notification webhooks is guarded against SSRF: loopback,
  link-local, and cloud-metadata targets are refused — including IPv4-mapped IPv6 forms and across
  HTTP redirects — while legitimate private / internal ranges stay monitorable.
- TLS certificate verification is on by default for URL monitors; disabling it is an explicit,
  per-monitor opt-in.
