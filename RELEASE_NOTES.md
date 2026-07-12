# Release Notes

## v0.1.9

**Phone support for the WebUI, scaling to tens of thousands of nodes, and a round of security
hardening** — the WebUI now adapts to phones, the inventory / topology / dashboards stay responsive
at fleet scale (searching the fleet on the server instead of loading it into the browser), and
sessions, logins, TLS guidance, and metric ingest are hardened.

### New Features
- **Phone / mobile support for the WebUI** — the WebUI now adapts to phone-sized screens: a mobile
  app shell with a navigation drawer and bottom sheets, card-style tables, forms, and node detail, a
  pane switcher so a node's detail is reachable on a small screen, fuller event and interface views,
  and a Preferences toggle to force the mobile or desktop layout. Desktop rendering is unchanged.

### Improvements
- **Scales to tens of thousands of nodes** — the inventory tree is virtualized and loads a group's
  members lazily (only open, visible groups fetch), topology is paged on the server, and per-group
  and fleet status rollups are computed server-side; live node status now arrives incrementally over
  a stream instead of full-page refetches. Large inventories stay responsive.
- **Fleet-wide search instead of loading the whole inventory** — the node inventory filter and the
  Troubleshoot scope picker now search on the server (debounced and capped), and node pickers use the
  same typeahead, so the browser never has to hold the entire fleet to find a node.
- **Human names instead of raw IDs across the fleet** — referenced nodes, groups, and profiles now
  resolve to their names on the Dependencies, Classification-rules, Mutes, and Maintenance pages
  (with copy-of-the-id on hover), and name resolution covers the whole fleet, not just the first page.
- **Faster, steadier metric ingest and polling at scale** — poll results are handled by a matcher
  plus asynchronous batch writers, SNMP table walks are folded into a single session, dependency
  suppression re-sweeps incrementally, and alert-config and sweep-spec resolution are cached per
  configuration generation — smoothing throughput on large fleets.
- **Faster node-detail interfaces list** — a node's per-interface throughput and status now load with
  a constant handful of time-series queries for the whole node instead of several queries per
  interface, so the interface list on a many-port switch opens far faster and refreshes cheaply.
- **Snappier chart and health views** — the interface, busiest-links heatmap, and host-resource
  charts fetch their independent series concurrently, and the URL-monitor probe reuses a pooled HTTP
  client, trimming latency on those views.
- **Higher default edge intake limits** — the syslog / SNMP-trap per-source and global rate-limit
  defaults were raised (200 / 5000 messages per second) to suit chassis-scale event volume.
- **Polished report viewer and confirmations** — the report viewer is now the standard app dialog
  (keyboard focus handling, and a bottom sheet on mobile), and deleting a report, template, or
  schedule uses the app's themed confirmation dialog instead of a native browser prompt.
- **Container health checks** — the core image now reports readiness through a real health check, and
  the web container waits for the core to be serving before it starts, avoiding first-request errors
  on a cold start.

### Bug Fixes
- **Consistent spacing and emphasis** — several WebUI styles referenced undefined spacing tokens
  (rendering with collapsed spacing) or off-scale font weights; these now use shared tokens, so
  spacing and emphasis are consistent and theme-aware.
- **Chart series colors follow the theme** — time-series chart colors are now drawn from the theme's
  series palette, so they adapt to light and dark mode and no longer reuse the status
  (warning / critical) colors.

### Security
- **Sessions can be revoked and now expire** — disabling, demoting, resetting the password of, or
  deleting a user immediately invalidates that account's active sessions, and sessions now expire
  after an idle period and an absolute lifetime; a new logout endpoint revokes the current token
  server-side. Previously a bearer token stayed valid until the core restarted, so an admin action on
  a compromised account did not cut off an already-issued token.
- **Login brute-force protection** — the login endpoint now applies a per-account exponential lockout
  after repeated failures plus a global attempt-rate cap, throttling password-guessing runs and the
  CPU-exhaustion vector of repeatedly forcing a password hash.
- **Metric-name validation at ingest** — metric names arriving from pollers are re-validated where
  they enter the time-series store, so a malformed or hostile name can't inject stray series or
  labels (a cardinality / data-integrity safeguard).
- **TLS guidance for the WebUI / API** — the WebUI image and deployment now document how to terminate
  HTTPS — an opt-in in-container TLS server block or an external reverse proxy — so the login
  password, bearer tokens, and submitted device credentials are not exposed in plaintext beyond a
  trusted network. The default remains plain HTTP for LAN / behind-proxy use; this is guidance and
  configuration only.

## v0.1.8

**Searchable passive-event log store, richer Events and dashboards, SSRF hardening, and large-fleet
speedups** — received syslog messages and SNMP traps now land in a searchable log store, the Events views
gained filtering, dashboard widgets can be resized and made taller, the URL-monitor probe is hardened
against SSRF, and node inventory and topology load faster on big fleets.

### New Features
- **Passive-event log store with full-text search** — received syslog messages and SNMP traps are now
  persisted to a dedicated log store (VictoriaLogs) via asynchronous batched writes and can be searched
  full-text from the Events views, so passive events are retained and queryable instead of transient.
- **Filters on the Events views** — events can now be narrowed by time range, event kind, whether they
  matched a rule, and free-text / regex search, making it far easier to find a specific event in a busy
  stream.
- **Resizable, taller dashboard widgets** — dashboard widgets now support a stepped per-widget height (with
  an edit-mode height selector) and corner drag-to-resize on a snap-aligned, gap-free grid; edits can be
  discarded with Cancel, and a widget's content fills its resized cell.

### Improvements
- **Faster node list and topology on large fleets** — the node inventory and topology views now derive the
  fallback status of not-yet-observed nodes with a single time-series query instead of one query per node.
  This is most noticeable on large inventories and immediately after a core restart, when the alert engine
  has not yet formed an opinion on every node.
- **Tunable database connection pool** — the core's PostgreSQL connection pool size is now configurable via
  the `YAGRA_PG_MAX_CONNECTIONS` environment variable and defaults higher, lifting a concurrency ceiling
  that could throttle the scheduler, result ingest, and API under tens-of-thousands-of-nodes load.
- **Faster node-detail open** — a node's SNMP scalar readings now load concurrently instead of one after
  another, so the Overview tab fills in more quickly on devices with many scalar metrics.

### Bug Fixes
- **Dashboard chart flicker** — fixed a ResizeObserver-driven flicker on fill-mode (height-filling)
  dashboard charts.
- **UI text-size consistency** — the remaining hardcoded WebUI text sizes (including one that referenced a
  previously-undefined style token and silently rendered at the wrong size) now use shared size tokens, so
  text renders at consistent, themeable sizes.

### Security
- **URL-monitor SSRF hardening** — the HTTP(S) monitoring probe now resolves and connects only through an
  SSRF-filtered resolver, applied to the initial target **and every redirect hop**. This closes two ways
  the loopback / link-local / cloud-metadata blocklist could be bypassed: a monitored endpoint could
  redirect the probe to a hostname that resolved to a blocked address, and a DNS-rebinding target could
  pass the pre-flight check yet be dialed at a blocked address. Private/internal ranges remain allowed (an
  NMS legitimately monitors them); only the loopback/link-local/metadata escalation surface is refused.

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
