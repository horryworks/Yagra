# Release Notes

## v0.1.0 (unreleased)

Initial repository scaffold and tested core.

### New Features
- Cargo workspace with component crates (Yagra-core, Yagra-poller, Yagra-discovery,
  Yagra-alert, Yagra-bus, Yagra-transport, Yagra-topology) plus shared libraries
  (yagra-common, yagra-secrets), and the Yagra-web WebUI under `web/`.
- Tested core logic: alert quality (hysteresis, flapping, dependency suppression,
  dedup/grouping, notification dispatch), threshold inheritance, RBAC, envelope
  encryption, Credential Finder rate limiting, and a bus message contract.
- Single-process "walking skeleton" wiring core → bus → poller → metric → REST API.
- React + TypeScript WebUI (typed API client, SSE, dashboard, alert list, charts).
- Docker Compose skeleton for single-node MVP (core, poller, stores, bus, TSDB).

> No functional release yet: integration with the external services (NATS,
> VictoriaMetrics, PostgreSQL, raw-socket ICMP, SNMP) is still in progress.
