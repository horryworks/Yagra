# Yagra

Yagra is a **network monitoring system (NMS)** that monitors network devices and
servers via **ICMP / SNMP / API calls**. It continuously watches liveness,
performance, and thresholds, and raises alerts on anomalies. It runs in Docker and
is architected from the start for **tens of thousands of nodes** and **distributed
polling**. Users access it through the WebUI.

> Status: **v0.1.3.** A functional stack (ICMP / SNMP v2c+v3 / URL monitoring / Cisco Meraki via
> the read-only Dashboard API, passive event monitoring, discovery & classification, alerting,
> dashboards, and reports) over PostgreSQL, Redis, NATS, and VictoriaMetrics via Docker Compose.
> Single-node by default, it now scales out with **distributed poller pools** — remote pollers at
> branch sites, assigned by location affinity and failed over automatically. HA stores remain a
> configuration step away, not a rewrite.

## Components

Each backend component is a workspace crate under `crates/`; the WebUI lives under `web/`.

| Component | Role | Crate / dir |
|---|---|---|
| Yagra-core | Orchestration, scheduling, northbound API | `crates/yagra-core` |
| Yagra-poller | ICMP/SNMP/API polling (stateless, scalable) | `crates/yagra-poller` |
| Yagra-discovery | Device discovery & classification | `crates/yagra-discovery` |
| Yagra-alert | State machine, hysteresis, dependency suppression | `crates/yagra-alert` |
| Yagra-ingest | Passive-event parsing (syslog / SNMP traps) + rate limiting | `crates/yagra-ingest` |
| Yagra-bus | Job distribution, poller fan-out | `crates/yagra-bus` |
| Yagra-transport | ICMP/SNMP/HTTP abstraction | `crates/yagra-transport` |
| Yagra-topology | Dependency graph & map | `crates/yagra-topology` |
| Yagra-web | Dashboards & visualization | `web/` |

Shared libraries: `crates/yagra-common` (cross-cutting types) and `crates/yagra-secrets`
(envelope encryption for credentials).

> Crate directory names match the functional `Yagra-*` names above (e.g. `crates/yagra-core`).

## Tech Stack

- **Backend:** Rust — Tokio, Axum, sqlx (PostgreSQL). Cargo workspace under `crates/`.
- **Frontend:** React + TypeScript + Vite, uPlot for time-series. Under `web/`.
- **Stores:** PostgreSQL (metadata), Redis (cache/locks/poller-assignment), VictoriaMetrics (TSDB — metrics).
- **Bus:** NATS (core⇄poller).
- **Northbound API:** REST (`/api/v1`).
- **Deploy:** Docker / Docker Compose (MVP) → Kubernetes (scale-out / HA).

## Passive event monitoring (syslog / SNMP traps / webhooks)

Besides active polling, Yagra receives events and matches them against operator-defined
rules (substring / regex) to raise alerts, which can be forwarded to PagerDuty (Events
API v2) and Jira Service Management (Alerts API) with native fire/resolve lifecycle:

- **syslog** (UDP 514) and **SNMP traps** (UDP 162, v1/v2c + informs) are received by the
  poller and forwarded to core over the bus. Enable per site via `YAGRA_SYSLOG_BIND` /
  `YAGRA_TRAP_BIND` (see `docker-compose.yml`). **SNMPv3 traps are not supported yet.**
- **Webhooks** are received on the core API: `POST /api/v1/ingest/webhook/<source-id>`
  with a per-source bearer token (create sources under *Alerts ▸ Event sources*).
- Rules (*Alerts ▸ Event rules*) assign severity, an auto-close TTL, an optional
  clear-pattern (e.g. link-up clears link-down), and a fire threshold (N events in M
  seconds). Received events are browsable under *Alerts ▸ Events*.

> **Deployment notes:** event→node correlation uses the datagram **source IP** — if
> Docker's bridge networking rewrites it on your host, run the poller with
> `network_mode: host`. If the host already runs a syslog daemon on port 514, remap the
> published port. Poller-side rate limits (per-source and global) bound event floods.

## Getting Started

```bash
# Backend (Rust workspace)
cargo build
cargo test

# Frontend (web/)
cd web && npm install && npm run dev

# Full stack (single-node Docker Compose)
docker compose up --build
```

## License

MIT
